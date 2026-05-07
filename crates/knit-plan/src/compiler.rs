//! Main compilation logic: [`DataModel`] → [`ExecutionPlan`].
//!
//! This module contains the [`compile()`] function — the primary entry point for
//! `knit-plan`. It orchestrates dependency analysis, partition planning, RNG tree
//! construction, and field plan compilation into a single coherent execution plan.

use std::collections::{BTreeMap, HashMap};

use knit_core::{
    DataModel, DataType, DistributionKind, Entity, Field, GeneratorSpec, NullSpec, Value,
};

use crate::error::PlanError;
use crate::graph;
use crate::partition;
use crate::rng_tree;
use crate::types::*;

/// Compile a validated [`DataModel`] into an [`ExecutionPlan`].
///
/// This is the main entry point for the planning phase. It performs:
/// 1. Dependency graph construction and phase assignment
/// 2. Row count resolution and partition planning
/// 3. Field plan compilation (GeneratorSpec → GeneratorPlan)
/// 4. RNG tree construction for deterministic seeding
/// 5. Index strategy selection based on entity sizes
///
/// # Errors
///
/// Returns [`PlanError::UnknownEntity`] if a relationship references an entity
/// that doesn't exist in the model.
pub fn compile(model: &DataModel) -> Result<ExecutionPlan, PlanError> {
    // 1. Build dependency graph and assign phases.
    let assignment = graph::assign_phases(model)?;

    // 2. Resolve row counts.
    let row_counts = graph::resolve_row_counts(model);

    // 3. Build entity lookup.
    let entity_map: HashMap<&str, &Entity> =
        model.entities.iter().map(|e| (e.name.as_str(), e)).collect();

    // 4. Build relationship lookup: from_entity → Vec<(to_entity, fk_field)>.
    let mut fk_fields: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for rel in &model.relationships {
        let fk_field = rel
            .foreign_key
            .clone()
            .unwrap_or_else(|| format!("{}_id", rel.to));
        fk_fields
            .entry(rel.from.clone())
            .or_default()
            .push((rel.to.clone(), fk_field));
    }

    // 5. Build index strategy.
    let index_strategy = build_index_strategy(&row_counts);

    // 6. Compute partitions and entity plans per phase.
    let mut phases = Vec::new();
    let mut total_partitions = 0usize;
    let mut estimated_total_rows = 0u64;
    let mut estimated_total_bytes = 0u64;

    // Collect entity info for RNG tree.
    let mut rng_entities: Vec<(String, Vec<String>, u32)> = Vec::new();

    for phase_entities in &assignment.phases {
        let mut entity_plans = Vec::new();

        for entity_name in phase_entities {
            let entity = entity_map.get(entity_name.as_str()).ok_or_else(|| {
                PlanError::UnknownEntity {
                    name: entity_name.clone(),
                }
            })?;

            let row_count = row_counts.get(entity_name).copied().unwrap_or(1000);
            let entity_seed = rng_tree::derive_seed(model.seed, entity_name.as_bytes());
            let partitions = partition::compute_partitions(row_count, entity_seed);
            let num_partitions = partitions.len() as u32;

            let entity_fks = fk_fields.get(entity_name).cloned().unwrap_or_default();
            let field_plans = compile_field_plans(entity, &entity_fks, &row_counts, &index_strategy, &model.actor_relationships);

            let estimated_byte_size = estimate_byte_size(entity, row_count);

            let field_names: Vec<String> =
                entity.fields.iter().map(|f| f.name.clone()).collect();
            rng_entities.push((entity_name.clone(), field_names, num_partitions));

            total_partitions += partitions.len();
            estimated_total_rows += row_count;
            estimated_total_bytes += estimated_byte_size;

            // Find the primary-key field index explicitly from the source schema.
            let primary_key_field_index = entity
                .fields
                .iter()
                .position(|f| f.primary_key.unwrap_or(false));

            entity_plans.push(EntityPlan {
                entity_name: entity_name.clone(),
                partitions,
                field_plans,
                estimated_row_count: row_count,
                estimated_byte_size,
                primary_key_field_index,
            });
        }

        // Collect deferred refs for this phase.
        let phase_deferred: Vec<DeferredRef> = assignment
            .deferred_refs
            .iter()
            .filter(|dr| phase_entities.contains(&dr.from_entity))
            .cloned()
            .collect();

        phases.push(Phase {
            entity_plans,
            deferred_refs: phase_deferred,
        });
    }

    // 7. Build RNG tree.
    let rng_tree = rng_tree::build_rng_tree(model.seed, &rng_entities);

    // 8. Build actor pool plan.
    let actor_pool = compile_actor_pool(model, &row_counts);

    // 9. Build metadata.
    let deferred_ref_count = assignment.deferred_refs.len();
    let metadata = PlanMetadata {
        schema_name: model.name.clone(),
        total_entities: model.entities.len(),
        total_phases: phases.len(),
        total_partitions,
        estimated_total_rows,
        estimated_total_bytes,
        has_cycles: deferred_ref_count > 0,
        deferred_ref_count,
        actor_entity_count: model.entities.iter().filter(|e| e.actor).count(),
        persona_count: model.personas.len(),
        actor_relationship_count: model.actor_relationships.len(),
    };

    Ok(ExecutionPlan {
        phases,
        rng_tree,
        index_strategy,
        actor_pool,
        metadata,
    })
}

/// Compile the actor pool plan from personas and actor relationships.
fn compile_actor_pool(model: &DataModel, row_counts: &BTreeMap<String, u64>) -> ActorPoolPlan {
    let mut pools = Vec::new();
    let mut graph_plans = Vec::new();

    // Build persona lookup: group personas by the entity they belong to.
    // Since persona_distribution on an entity references "personas" globally,
    // we match personas to entities by name prefix (entity_personaName pattern
    // from schema emission) or assign all personas to actor entities.
    for entity in &model.entities {
        if !entity.actor {
            continue;
        }
        if entity.persona_distribution.is_none() && model.personas.is_empty() {
            continue;
        }

        let actor_count = row_counts
            .get(&entity.name)
            .copied()
            .unwrap_or(resolve_count_estimate(&entity.count));

        // Find personas belonging to this entity.
        // Convention: personas are prefixed with "{entity_name}_" from learn,
        // or all personas apply to the entity if not prefixed.
        let entity_prefix = format!("{}_", entity.name);
        let mut persona_weights: Vec<PersonaWeight> = model
            .personas
            .iter()
            .filter(|p| p.name.starts_with(&entity_prefix) || !has_entity_prefix(&p.name, &model.entities))
            .map(|p| PersonaWeight {
                name: p.name.clone(),
                weight: p.weight,
                traits: p.traits.clone(),
            })
            .collect();

        // Normalize weights if they don't sum to 1.0
        let total_weight: f64 = persona_weights.iter().map(|pw| pw.weight).sum();
        if total_weight <= 0.0 {
            // All-zero or negative weights — skip this entity
            continue;
        }
        if (total_weight - 1.0).abs() > 1e-6 {
            for pw in &mut persona_weights {
                pw.weight /= total_weight;
            }
        }

        if !persona_weights.is_empty() {
            pools.push(ActorEntityPool {
                entity_name: entity.name.clone(),
                actor_count,
                persona_weights,
            });
        }
    }

    // Compile graph plans from actor relationships
    for rel in &model.actor_relationships {
        let community_count = rel.community_count.as_ref().map(|cs| resolve_count_estimate(cs));

        graph_plans.push(GraphPlan {
            name: rel.name.clone(),
            from_entity: rel.from_entity.clone(),
            to_entity: rel.to_entity.clone(),
            graph_type: rel.graph_type.clone(),
            params: rel.params.clone(),
            community_count,
            hierarchy_depth: rel.hierarchy_depth,
        });
    }

    ActorPoolPlan { pools, graph_plans }
}

/// Check if a persona name is prefixed with any entity name.
fn has_entity_prefix(persona_name: &str, entities: &[Entity]) -> bool {
    entities.iter().any(|e| {
        let prefix = format!("{}_", e.name);
        persona_name.starts_with(&prefix)
    })
}

/// Resolve a CountSpec to a deterministic estimate (midpoint for ranges,
/// mean for distributions).
fn resolve_count_estimate(count: &knit_core::CountSpec) -> u64 {
    match count {
        knit_core::CountSpec::Fixed(n) => *n,
        knit_core::CountSpec::Range { min, max } => (min + max) / 2,
        knit_core::CountSpec::Distribution(spec) => {
            // Try common distribution parameters for mean estimate
            if let Some(&mean) = spec.params.get("mean").or_else(|| spec.params.get("mu")) {
                mean.max(1.0) as u64
            } else if let Some(&lambda) = spec.params.get("lambda") {
                lambda.max(1.0) as u64
            } else if let (Some(&min), Some(&max)) = (spec.params.get("min"), spec.params.get("max")) {
                ((min + max) / 2.0).max(1.0) as u64
            } else {
                1000
            }
        }
    }
}

/// Compile field plans for an entity.
fn compile_field_plans(
    entity: &Entity,
    entity_fks: &[(String, String)],
    row_counts: &BTreeMap<String, u64>,
    _index_strategy: &IndexStrategy,
    actor_relationships: &[knit_core::ActorRelationship],
) -> Vec<FieldPlan> {
    let fk_map: HashMap<&str, &str> = entity_fks
        .iter()
        .map(|(target, fk_field)| (fk_field.as_str(), target.as_str()))
        .collect();

    let mut plans: Vec<FieldPlan> = Vec::new();

    for field in &entity.fields {
        // Check if field has an explicit RelationshipRef generator — if so, it takes
        // precedence over the inferred FK path (graph-aware sampling vs uniform FK).
        let has_relationship_ref = matches!(
            &field.generator,
            Some(GeneratorSpec::RelationshipRef { .. })
        );

        // Check if this field is a foreign key.
        // Use FK generator for Int/Int32/Uuid/String typed fields,
        // unless the field explicitly uses relationship_ref.
        let generator_plan = if !has_relationship_ref {
            if let Some(&target_entity) = fk_map.get(field.name.as_str()) {
                let is_fk_compatible = matches!(
                    field.data_type,
                    knit_core::DataType::Int
                        | knit_core::DataType::Int32
                        | knit_core::DataType::Uuid
                        | knit_core::DataType::String
                );
                if is_fk_compatible {
                    let target_rows = row_counts.get(target_entity).copied().unwrap_or(1000);
                    let key_store_kind = select_key_store_kind(target_rows);
                    GeneratorPlan::ForeignKey {
                        target_entity: target_entity.to_string(),
                        target_field: "id".to_string(),
                        key_store_kind,
                    }
                } else {
                    compile_generator(field, &entity.fields)
                }
            } else {
                compile_generator(field, &entity.fields)
            }
        } else {
            compile_generator(field, &entity.fields)
        };

        // Resolve GraphTarget's from/to entities from actor_relationships
        let generator_plan = match generator_plan {
            GeneratorPlan::GraphTarget { graph_name, source_field, key_store_kind, .. } => {
                let ar = actor_relationships.iter().find(|ar| ar.name == graph_name);
                let from_entity = ar.map(|a| a.from_entity.clone()).unwrap_or_default();
                let target_entity = ar.map(|a| a.to_entity.clone()).unwrap_or_default();
                GeneratorPlan::GraphTarget {
                    graph_name,
                    source_field,
                    from_entity,
                    target_entity,
                    key_store_kind,
                }
            }
            other => other,
        };

        let null_plan = compile_null_plan(&field.nullable);
        let dependency_order = compute_dependency_order(field, &entity.fields);

        plans.push(FieldPlan {
            field_name: field.name.clone(),
            data_type: field.data_type.clone(),
            generator_plan,
            null_plan,
            dependency_order,
            precision: field.precision,
            actor_column: field.actor_column,
        });
    }

    // Sort by dependency_order for correct generation ordering.
    plans.sort_by_key(|fp| fp.dependency_order);
    plans
}

/// Convert a `GeneratorSpec` to a `GeneratorPlan`.
fn compile_generator(field: &Field, all_fields: &[Field]) -> GeneratorPlan {
    match &field.generator {
        Some(spec) => match spec {
            GeneratorSpec::Distribution { spec: dist_spec } => {
                // Auto-enable rounding when the field's declared data type is integer.
                // This ensures distribution generators produce integer values even when
                // the user doesn't explicitly set `round = true` in the schema.
                let round = dist_spec.round
                    || matches!(field.data_type, DataType::Int | DataType::Int32);
                GeneratorPlan::Distribution {
                    kind: dist_spec.kind.clone(),
                    params: dist_spec.params.clone(),
                    clamp_min: None,
                    clamp_max: None,
                    round,
                }
            }
            GeneratorSpec::Faker { method, args } => GeneratorPlan::Faker {
                category: method.clone(),
                locale: "en_US".to_string(),
                args: args.clone(),
            },
            GeneratorSpec::Sequence {
                start,
                step,
                prefix: _,
            } => GeneratorPlan::Sequence {
                start: *start,
                step: *step,
            },
            GeneratorSpec::OneOf { choices } => {
                let total_weight: f64 = choices.iter().map(|c| c.weight).sum();
                // Fall back to uniform weights if total is zero
                let effective_total = if total_weight == 0.0 {
                    choices.len() as f64
                } else {
                    total_weight
                };
                let mut cumulative = Vec::with_capacity(choices.len());
                let mut running = 0.0;
                for choice in choices {
                    let w = if total_weight == 0.0 { 1.0 } else { choice.weight };
                    running += w / effective_total;
                    cumulative.push(running);
                }
                GeneratorPlan::OneOf {
                    choices: choices.clone(),
                    cumulative_weights: cumulative,
                }
            }
            GeneratorSpec::Derived { expr } => {
                let depends_on = extract_dependencies(expr, all_fields);
                GeneratorPlan::Derived {
                    expr: expr.clone(),
                    depends_on,
                }
            }
            GeneratorSpec::Constant { value } => GeneratorPlan::Constant(value.clone()),
            GeneratorSpec::UuidGen { version: _ } => GeneratorPlan::Uuid,
            GeneratorSpec::Composite {
                template: _,
                generators,
            } => {
                if let Some((_, first_gen)) = generators.iter().next() {
                    let element = Box::new(compile_generator_from_spec(first_gen, all_fields, &field.data_type));
                    let length = Box::new(GeneratorPlan::Constant(knit_core::Value::Int(1)));
                    GeneratorPlan::Composite { element, length }
                } else {
                    GeneratorPlan::Constant(knit_core::Value::String(String::new()))
                }
            }
            GeneratorSpec::Lookup { entity, field } => GeneratorPlan::ForeignKey {
                target_entity: entity.clone(),
                target_field: field.clone(),
                key_store_kind: KeyStoreKind::InMemoryVec,
            },
            GeneratorSpec::Pattern { pattern } => {
                GeneratorPlan::Pattern { pattern: pattern.clone() }
            }
            GeneratorSpec::Unique { inner, max_retries } => {
                let inner_plan = compile_generator_from_spec(inner, all_fields, &field.data_type);
                GeneratorPlan::Unique {
                    inner: Box::new(inner_plan),
                    max_retries: *max_retries,
                }
            }
            GeneratorSpec::Relative { field, offset } => {
                let mut params = BTreeMap::new();
                // The RelativeGenerator reads offset_mean (seconds) and offset_std
                let offset_val = match offset {
                    Value::Int(n) => *n as f64,
                    Value::Float(f) => *f,
                    _ => 60.0, // default 60 seconds
                };
                params.insert("offset_mean".into(), offset_val);
                // Default std is 10% of offset or minimum 1.0
                params.insert("offset_std".into(), (offset_val.abs() * 0.1).max(1.0));
                GeneratorPlan::Temporal {
                    kind: TemporalKind::Relative,
                    params,
                    base_field: Some(field.clone()),
                }
            }
            GeneratorSpec::BusinessHours {
                start_hour,
                end_hour,
                exclude_weekends,
            } => {
                let mut params = BTreeMap::new();
                params.insert("start_hour".into(), *start_hour as f64);
                params.insert("end_hour".into(), *end_hour as f64);
                // Generator reads "weekdays_only" (1.0 = true, 0.0 = false)
                params.insert("weekdays_only".into(), if *exclude_weekends { 1.0 } else { 0.0 });
                GeneratorPlan::Temporal {
                    kind: TemporalKind::BusinessHours,
                    params,
                    base_field: None,
                }
            }
            GeneratorSpec::Conditional { field: cond_field, branches, default } => {
                // Compile each branch's generator recursively
                let compiled_branches: Vec<(Value, Box<GeneratorPlan>)> = branches
                    .iter()
                    .map(|b| {
                        let plan = compile_generator_from_spec(&b.generator, all_fields, &field.data_type);
                        (b.condition.clone(), Box::new(plan))
                    })
                    .collect();
                let default_plan = match default {
                    Some(gen) => Box::new(compile_generator_from_spec(gen, all_fields, &field.data_type)),
                    None => Box::new(GeneratorPlan::Constant(Value::Null)),
                };
                GeneratorPlan::Conditional {
                    field: cond_field.clone(),
                    branches: compiled_branches,
                    default: default_plan,
                }
            }
            GeneratorSpec::Dictionary { file, expansion } => {
                // Entries are loaded by the CLI layer after compilation,
                // which resolves the file path relative to the schema.
                GeneratorPlan::Dictionary {
                    entries: vec![],
                    expansion: expansion.clone(),
                    source_file: Some(file.clone()),
                }
            }
            // Behavioral modeling generators — placeholder plans until
            // the generation engine implements persona/graph-based generation.
            GeneratorSpec::ActorRef { entity } => {
                GeneratorPlan::ForeignKey {
                    target_entity: entity.clone(),
                    target_field: "id".to_string(),
                    key_store_kind: KeyStoreKind::InMemoryVec,
                }
            }
            GeneratorSpec::ActorTemporal { .. } => {
                GeneratorPlan::Temporal {
                    kind: TemporalKind::BusinessHours,
                    params: BTreeMap::new(),
                    base_field: None,
                }
            }
            GeneratorSpec::RelationshipRef { relationship, source_field } => {
                // Resolve source_field: use explicit value or auto-detect from
                // other actor_column fields in the entity.
                let resolved_source = source_field.clone().unwrap_or_else(|| {
                    all_fields
                        .iter()
                        .find(|f| f.actor_column && f.name != field.name)
                        .map(|f| f.name.clone())
                        .unwrap_or_default()
                });
                // Target entity is inferred from the relationship's FK target.
                // At this level we don't have the full model, so we use a
                // placeholder that the top-level compiler resolves.
                GeneratorPlan::GraphTarget {
                    graph_name: relationship.clone(),
                    source_field: resolved_source,
                    from_entity: String::new(), // resolved below
                    target_entity: String::new(), // resolved below
                    key_store_kind: KeyStoreKind::InMemoryVec,
                }
            }
            GeneratorSpec::PersonaField { trait_name } => {
                GeneratorPlan::Constant(Value::String(format!("{{persona:{}}}", trait_name)))
            }
        },
        None => {
            // No generator specified — provide a sensible default based on data_type.
            if field.primary_key.unwrap_or(false)
                && field.data_type == knit_core::DataType::Uuid
            {
                GeneratorPlan::Uuid
            } else if field.primary_key.unwrap_or(false) {
                GeneratorPlan::Sequence { start: 1, step: 1 }
            } else {
                default_generator_for_type(&field.data_type)
            }
        }
    }
}

/// Convert a `GeneratorSpec` directly (for nested generators).
///
/// `parent_data_type` carries the owning field's declared type so that nested
/// distribution generators can auto-enable rounding for integer fields.
fn compile_generator_from_spec(
    spec: &GeneratorSpec,
    all_fields: &[Field],
    parent_data_type: &knit_core::DataType,
) -> GeneratorPlan {
    // Create a dummy field to reuse compile_generator.
    let dummy_field = Field {
        name: String::new(),
        description: None,
        data_type: parent_data_type.clone(),
        generator: Some(spec.clone()),
        nullable: NullSpec::Never,
        primary_key: None,
        precision: None,
        actor_column: false,
    };
    compile_generator(&dummy_field, all_fields)
}

/// Provide a sensible default generator plan when no generator is specified.
///
/// This produces realistic random values for each data type:
/// - Bool → Bernoulli(0.5)
/// - Int/Int32 → Uniform(0..1000) rounded
/// - Float → Uniform(0..100)
/// - String → Faker word
/// - Uuid → UUID v4
/// - Date → Faker date
/// - Datetime/DatetimeUs → Faker datetime
/// - Time/Datetimetz/Duration/Bytes/Array/Map → constant null (no default generator)
fn default_generator_for_type(data_type: &knit_core::DataType) -> GeneratorPlan {
    use knit_core::DataType;

    match data_type {
        DataType::Bool => {
            let mut params = BTreeMap::new();
            params.insert("p".to_string(), 0.5);
            GeneratorPlan::Distribution {
                kind: DistributionKind::Bernoulli,
                params,
                clamp_min: None,
                clamp_max: None,
                round: false,
            }
        }
        DataType::Int | DataType::Int32 => {
            let mut params = BTreeMap::new();
            params.insert("min".to_string(), 0.0);
            params.insert("max".to_string(), 1000.0);
            GeneratorPlan::Distribution {
                kind: DistributionKind::Uniform,
                params,
                clamp_min: None,
                clamp_max: None,
                round: true,
            }
        }
        DataType::Float => {
            let mut params = BTreeMap::new();
            params.insert("min".to_string(), 0.0);
            params.insert("max".to_string(), 100.0);
            GeneratorPlan::Distribution {
                kind: DistributionKind::Uniform,
                params,
                clamp_min: None,
                clamp_max: None,
                round: false,
            }
        }
        DataType::String => GeneratorPlan::Faker {
            category: "word".to_string(),
            locale: "en_US".to_string(),
            args: vec![],
        },
        DataType::Uuid => GeneratorPlan::Uuid,
        DataType::Date => GeneratorPlan::Faker {
            category: "date".to_string(),
            locale: "en_US".to_string(),
            args: vec![],
        },
        DataType::Datetime | DataType::DatetimeUs => GeneratorPlan::Faker {
            category: "datetime".to_string(),
            locale: "en_US".to_string(),
            args: vec![],
        },
        // Time, Datetimetz, Duration, Bytes, Array, Map have no sensible default generator
        DataType::Time
        | DataType::Datetimetz
        | DataType::Duration
        | DataType::Bytes
        | DataType::Array
        | DataType::Map => GeneratorPlan::Constant(knit_core::Value::Null),
    }
}

/// Convert `NullSpec` to `NullPlan`.
fn compile_null_plan(null_spec: &NullSpec) -> NullPlan {
    match null_spec {
        NullSpec::Never => NullPlan::Never,
        NullSpec::Always => NullPlan::Always,
        NullSpec::Probability(p) => NullPlan::Probability(*p),
        NullSpec::Pattern { every_n } => NullPlan::Pattern {
            every_n: *every_n as usize,
        },
    }
}

/// Compute dependency order: non-derived fields get 0, derived fields get 1+
/// based on transitive dependencies.
fn compute_dependency_order(field: &Field, all_fields: &[Field]) -> u32 {
    match &field.generator {
        Some(GeneratorSpec::Derived { expr }) => {
            let deps = extract_dependencies(expr, all_fields);
            if deps.is_empty() {
                1
            } else {
                // Check if any dependency is itself derived.
                let max_dep_order = deps
                    .iter()
                    .filter_map(|dep_name| {
                        all_fields
                            .iter()
                            .find(|f| f.name == *dep_name)
                            .map(|f| compute_dependency_order(f, all_fields))
                    })
                    .max()
                    .unwrap_or(0);
                max_dep_order + 1
            }
        }
        // Relative depends on its base_field — must come after it
        Some(GeneratorSpec::Relative { field: base, .. }) => {
            let base_order = all_fields
                .iter()
                .find(|f| f.name == *base)
                .map(|f| compute_dependency_order(f, all_fields))
                .unwrap_or(0);
            base_order + 1
        }
        // Conditional depends on the field it branches on + any deps inside branches
        Some(GeneratorSpec::Conditional { field: ref_field, branches, default }) => {
            // Dependency on the reference field
            let ref_order = all_fields
                .iter()
                .find(|f| f.name == *ref_field)
                .map(|f| compute_dependency_order(f, all_fields))
                .unwrap_or(0);
            // Also check dependencies inside branch generators
            let branch_max = branches
                .iter()
                .map(|b| compute_generator_spec_deps(&b.generator, all_fields))
                .max()
                .unwrap_or(0);
            let default_max = default
                .as_ref()
                .map(|d| compute_generator_spec_deps(d, all_fields))
                .unwrap_or(0);
            ref_order.max(branch_max).max(default_max) + 1
        }
        // RelationshipRef depends on its source_field — must come after it
        Some(GeneratorSpec::RelationshipRef { source_field, .. }) => {
            if let Some(src) = source_field {
                let src_order = all_fields
                    .iter()
                    .find(|f| f.name == *src)
                    .map(|f| compute_dependency_order(f, all_fields))
                    .unwrap_or(0);
                src_order + 1
            } else {
                // Auto-detect: depends on any other actor_column field in the entity
                let max_actor_order = all_fields
                    .iter()
                    .filter(|f| f.actor_column && f.name != field.name)
                    .map(|f| compute_dependency_order(f, all_fields))
                    .max()
                    .unwrap_or(0);
                max_actor_order + 1
            }
        }
        _ => 0,
    }
}

/// Extract field names referenced in a derived expression.
/// Simple heuristic: look for field names from `all_fields` that appear in the expression.
fn extract_dependencies(expr: &str, all_fields: &[Field]) -> Vec<String> {
    // Strip ${param.*} references before tokenizing — they are not field deps.
    let stripped = strip_param_refs(expr);
    // Tokenize on non-alphanumeric/underscore boundaries for whole-word matching
    let tokens: Vec<&str> = stripped
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .collect();
    all_fields
        .iter()
        .filter(|f| tokens.iter().any(|t| *t == f.name))
        .map(|f| f.name.clone())
        .collect()
}

/// Remove `${param.key}` placeholders from an expression so they don't
/// interfere with field-dependency extraction.
fn strip_param_refs(expr: &str) -> String {
    let mut result = String::with_capacity(expr.len());
    let mut rest = expr;
    while let Some(start) = rest.find("${param.") {
        result.push_str(&rest[..start]);
        let after = &rest[start + 8..]; // skip "${param."
        if let Some(end) = after.find('}') {
            rest = &after[end + 1..];
        } else {
            // Malformed — keep the remainder as-is
            result.push_str(&rest[start..]);
            rest = "";
            break;
        }
    }
    result.push_str(rest);
    result
}

/// Compute the dependency order contributed by a GeneratorSpec (for nested generators).
fn compute_generator_spec_deps(spec: &GeneratorSpec, all_fields: &[Field]) -> u32 {
    // Create a temporary Field to reuse compute_dependency_order
    let tmp = Field {
        name: String::new(),
        description: None,
        data_type: knit_core::DataType::String,
        nullable: NullSpec::default(),
        generator: Some(spec.clone()),
        primary_key: None,
        precision: None,
        actor_column: false,
    };
    compute_dependency_order(&tmp, all_fields)
}

/// Estimate byte size for an entity based on field types and row count.
fn estimate_byte_size(entity: &Entity, row_count: u64) -> u64 {
    let bytes_per_row: u64 = entity
        .fields
        .iter()
        .map(|f| match f.data_type {
            knit_core::DataType::Bool => 1,
            knit_core::DataType::Int | knit_core::DataType::Int32 => 8,
            knit_core::DataType::Float => 8,
            knit_core::DataType::String => 64,
            knit_core::DataType::Uuid => 16,
            knit_core::DataType::Date => 4,
            knit_core::DataType::Time => 8,
            knit_core::DataType::Datetime | knit_core::DataType::DatetimeUs | knit_core::DataType::Datetimetz => 8,
            knit_core::DataType::Duration => 8,
            knit_core::DataType::Bytes => 128,
            knit_core::DataType::Array => 128,
            knit_core::DataType::Map => 256,
        })
        .sum();
    bytes_per_row * row_count
}

/// Build index strategy based on row counts.
fn build_index_strategy(row_counts: &BTreeMap<String, u64>) -> IndexStrategy {
    let per_entity: BTreeMap<String, KeyStoreKind> = row_counts
        .iter()
        .map(|(name, &count)| (name.clone(), select_key_store_kind(count)))
        .collect();
    IndexStrategy { per_entity }
}

/// Select key store kind based on row count thresholds.
fn select_key_store_kind(row_count: u64) -> KeyStoreKind {
    const MEMORY_MAPPED_THRESHOLD: u64 = 10_000_000;
    const SAMPLED_THRESHOLD: u64 = 100_000_000;
    const SAMPLE_SIZE: usize = 10_000_000;

    if row_count >= SAMPLED_THRESHOLD {
        KeyStoreKind::SampledSubset {
            sample_size: SAMPLE_SIZE,
        }
    } else if row_count >= MEMORY_MAPPED_THRESHOLD {
        KeyStoreKind::MemoryMapped
    } else {
        KeyStoreKind::InMemoryVec
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use knit_core::*;

    /// Helper to create a simple entity with an id field and given count.
    fn simple_entity(name: &str, count: u64) -> Entity {
        Entity {
            name: name.to_string(),
            description: None,
            count: CountSpec::Fixed(count),
            fields: vec![
                Field {
                    name: "id".to_string(),
                    description: None,
                    data_type: DataType::Int,
                    generator: Some(GeneratorSpec::Sequence {
                        start: 1,
                        step: 1,
                        prefix: None,
                    }),
                    nullable: NullSpec::Never,
                    primary_key: Some(true),
            precision: None,
        actor_column: false,
        },
                Field {
                    name: "name".to_string(),
                    description: None,
                    data_type: DataType::String,
                    generator: Some(GeneratorSpec::Faker {
                        method: "name".to_string(),
                        args: vec![],
                    }),
                    nullable: NullSpec::Never,
                    primary_key: None,
            precision: None,
        actor_column: false,
        },
            ],
            constraints: vec![],
            topology: None,
        actor: false,
        persona_distribution: None,
        }
    }

    fn simple_model(name: &str, entities: Vec<Entity>, relationships: Vec<Relationship>) -> DataModel {
        DataModel {
            name: name.to_string(),
            description: None,
            seed: 42,
            locale: "en_US".to_string(),
            timezone: "UTC".to_string(),
            entities,
            relationships,
            noise_profiles: vec![],
            correlations: vec![],
            params: BTreeMap::new(),
            schema_version: "1.0".to_string(),
        personas: Vec::new(),
        actor_relationships: Vec::new(),
        }
    }

    #[test]
    fn test_determinism() {
        let model = simple_model(
            "test",
            vec![simple_entity("users", 1000), simple_entity("orders", 5000)],
            vec![Relationship {
                name: "user_orders".to_string(),
                from: "orders".to_string(),
                to: "users".to_string(),
                kind: RelationshipKind::ManyToOne,
                foreign_key: Some("user_id".to_string()),
                cardinality: None,
            }],
        );

        let plan1 = compile(&model).unwrap();
        let plan2 = compile(&model).unwrap();

        let json1 = serde_json::to_string(&plan1).unwrap();
        let json2 = serde_json::to_string(&plan2).unwrap();
        assert_eq!(json1, json2, "same model must produce identical plans");
    }

    #[test]
    fn test_linear_dependencies() {
        // user → order → line_item (line_item depends on order, order depends on user)
        let model = simple_model(
            "linear",
            vec![
                simple_entity("user", 1000),
                simple_entity("order", 5000),
                simple_entity("line_item", 20000),
            ],
            vec![
                Relationship {
                    name: "order_user".to_string(),
                    from: "order".to_string(),
                    to: "user".to_string(),
                    kind: RelationshipKind::ManyToOne,
                    foreign_key: Some("user_id".to_string()),
                    cardinality: None,
                },
                Relationship {
                    name: "line_item_order".to_string(),
                    from: "line_item".to_string(),
                    to: "order".to_string(),
                    kind: RelationshipKind::ManyToOne,
                    foreign_key: Some("order_id".to_string()),
                    cardinality: None,
                },
            ],
        );

        let plan = compile(&model).unwrap();
        assert_eq!(plan.phases.len(), 3, "should have 3 phases");

        // Phase 0: user (no deps)
        assert!(plan.phases[0]
            .entity_plans
            .iter()
            .any(|ep| ep.entity_name == "user"));
        // Phase 1: order (depends on user)
        assert!(plan.phases[1]
            .entity_plans
            .iter()
            .any(|ep| ep.entity_name == "order"));
        // Phase 2: line_item (depends on order)
        assert!(plan.phases[2]
            .entity_plans
            .iter()
            .any(|ep| ep.entity_name == "line_item"));
    }

    #[test]
    fn test_independent_entities() {
        let model = simple_model(
            "independent",
            vec![
                simple_entity("user", 1000),
                simple_entity("product", 500),
            ],
            vec![],
        );

        let plan = compile(&model).unwrap();
        assert_eq!(plan.phases.len(), 1, "independent entities share one phase");
        assert_eq!(plan.phases[0].entity_plans.len(), 2);
    }

    #[test]
    fn test_self_referential() {
        let mut employee = simple_entity("employee", 1000);
        employee.fields.push(Field {
            name: "manager_id".to_string(),
            description: None,
            data_type: DataType::Int,
            generator: None,
            nullable: NullSpec::Probability(0.1),
            primary_key: None,
            precision: None,
        actor_column: false,
        });

        let model = simple_model(
            "self_ref",
            vec![employee],
            vec![Relationship {
                name: "employee_manager".to_string(),
                from: "employee".to_string(),
                to: "employee".to_string(),
                kind: RelationshipKind::ManyToOne,
                foreign_key: Some("manager_id".to_string()),
                cardinality: None,
            }],
        );

        let plan = compile(&model).unwrap();
        assert!(plan.metadata.has_cycles, "self-ref should be flagged");
        assert_eq!(plan.metadata.deferred_ref_count, 1);

        let deferred = &plan.phases[0].deferred_refs;
        assert_eq!(deferred.len(), 1);
        assert_eq!(deferred[0].from_entity, "employee");
        assert_eq!(deferred[0].to_entity, "employee");
        assert!(matches!(
            deferred[0].strategy,
            DeferralStrategy::SelfReference { .. }
        ));
    }

    #[test]
    fn test_mutual_cycle() {
        let mut entity_a = simple_entity("a", 1000);
        entity_a.fields.push(Field {
            name: "b_id".to_string(),
            description: None,
            data_type: DataType::Int,
            generator: None,
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
        actor_column: false,
        });

        let mut entity_b = simple_entity("b", 1000);
        entity_b.fields.push(Field {
            name: "a_id".to_string(),
            description: None,
            data_type: DataType::Int,
            generator: None,
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
        actor_column: false,
        });

        let model = simple_model(
            "mutual_cycle",
            vec![entity_a, entity_b],
            vec![
                Relationship {
                    name: "a_to_b".to_string(),
                    from: "a".to_string(),
                    to: "b".to_string(),
                    kind: RelationshipKind::ManyToOne,
                    foreign_key: Some("b_id".to_string()),
                    cardinality: None,
                },
                Relationship {
                    name: "b_to_a".to_string(),
                    from: "b".to_string(),
                    to: "a".to_string(),
                    kind: RelationshipKind::ManyToOne,
                    foreign_key: Some("a_id".to_string()),
                    cardinality: None,
                },
            ],
        );

        let plan = compile(&model).unwrap();
        assert!(plan.metadata.has_cycles);
        assert_eq!(plan.metadata.deferred_ref_count, 2);
        // Both should be in the same phase.
        assert_eq!(plan.phases.len(), 1);
        assert_eq!(plan.phases[0].entity_plans.len(), 2);
    }

    #[test]
    fn test_partition_planning_large() {
        let model = simple_model("partitions", vec![simple_entity("big", 5_000_000)], vec![]);
        let plan = compile(&model).unwrap();
        let ep = &plan.phases[0].entity_plans[0];
        assert_eq!(ep.partitions.len(), 5);
    }

    #[test]
    fn test_partition_planning_small() {
        let model = simple_model("partitions", vec![simple_entity("small", 500_000)], vec![]);
        let plan = compile(&model).unwrap();
        let ep = &plan.phases[0].entity_plans[0];
        assert_eq!(ep.partitions.len(), 1);
    }

    #[test]
    fn test_rng_tree_unique_seeds() {
        let model = simple_model(
            "rng",
            vec![simple_entity("users", 1000), simple_entity("orders", 2000)],
            vec![],
        );

        let plan = compile(&model).unwrap();
        let mut all_seeds = Vec::new();
        for node in plan.rng_tree.entity_nodes.values() {
            all_seeds.push(node.entity_seed);
            for fnode in node.field_seeds.values() {
                all_seeds.push(fnode.field_seed);
                all_seeds.extend(&fnode.partition_seeds);
            }
        }
        let count = all_seeds.len();
        all_seeds.sort();
        all_seeds.dedup();
        assert_eq!(all_seeds.len(), count, "all RNG seeds must be unique");
    }

    #[test]
    fn test_index_strategy_in_memory() {
        let model = simple_model("idx", vec![simple_entity("tiny", 1000)], vec![]);
        let plan = compile(&model).unwrap();
        assert!(matches!(
            plan.index_strategy.per_entity["tiny"],
            KeyStoreKind::InMemoryVec
        ));
    }

    #[test]
    fn test_index_strategy_memory_mapped() {
        let model = simple_model("idx", vec![simple_entity("medium", 50_000_000)], vec![]);
        let plan = compile(&model).unwrap();
        assert!(matches!(
            plan.index_strategy.per_entity["medium"],
            KeyStoreKind::MemoryMapped
        ));
    }

    #[test]
    fn test_index_strategy_sampled() {
        let model = simple_model("idx", vec![simple_entity("huge", 500_000_000)], vec![]);
        let plan = compile(&model).unwrap();
        assert!(matches!(
            plan.index_strategy.per_entity["huge"],
            KeyStoreKind::SampledSubset { sample_size: 10_000_000 }
        ));
    }

    #[test]
    fn test_field_plan_distribution() {
        let mut entity = simple_entity("test", 100);
        entity.fields.push(Field {
            name: "score".to_string(),
            description: None,
            data_type: DataType::Float,
            generator: Some(GeneratorSpec::Distribution {
                spec: DistributionSpec {
                    kind: DistributionKind::Normal,
                    params: {
                        let mut p = BTreeMap::new();
                        p.insert("mean".to_string(), 50.0);
                        p.insert("std_dev".to_string(), 10.0);
                        p
                    },
                    round: false,
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
        actor_column: false,
        });

        let model = simple_model("fields", vec![entity], vec![]);
        let plan = compile(&model).unwrap();
        let ep = &plan.phases[0].entity_plans[0];
        let score_plan = ep.field_plans.iter().find(|fp| fp.field_name == "score").unwrap();
        assert!(matches!(
            score_plan.generator_plan,
            GeneratorPlan::Distribution { .. }
        ));
    }

    #[test]
    fn test_distribution_auto_round_for_int_type() {
        // When data_type is Int, the compiler should auto-enable rounding
        // even if the schema doesn't explicitly set round = true.
        let mut entity = simple_entity("test", 10);
        entity.fields.push(Field {
            name: "age".to_string(),
            description: None,
            data_type: DataType::Int,
            generator: Some(GeneratorSpec::Distribution {
                spec: DistributionSpec {
                    kind: DistributionKind::Normal,
                    params: {
                        let mut p = BTreeMap::new();
                        p.insert("mean".to_string(), 35.0);
                        p.insert("std_dev".to_string(), 12.0);
                        p
                    },
                    round: false,
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
        actor_column: false,
        });

        let model = simple_model("autoround", vec![entity], vec![]);
        let plan = compile(&model).unwrap();
        let ep = &plan.phases[0].entity_plans[0];
        let age_plan = ep.field_plans.iter().find(|fp| fp.field_name == "age").unwrap();
        match &age_plan.generator_plan {
            GeneratorPlan::Distribution { round, .. } => {
                assert!(round, "round should be auto-enabled for data_type=Int");
            }
            other => panic!("expected Distribution, got {other:?}"),
        }
    }

    #[test]
    fn test_distribution_no_auto_round_for_float_type() {
        // When data_type is Float, round should stay as-is (false).
        let mut entity = simple_entity("test", 10);
        entity.fields.push(Field {
            name: "score".to_string(),
            description: None,
            data_type: DataType::Float,
            generator: Some(GeneratorSpec::Distribution {
                spec: DistributionSpec {
                    kind: DistributionKind::Normal,
                    params: {
                        let mut p = BTreeMap::new();
                        p.insert("mean".to_string(), 50.0);
                        p.insert("std_dev".to_string(), 10.0);
                        p
                    },
                    round: false,
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
        actor_column: false,
        });

        let model = simple_model("noround", vec![entity], vec![]);
        let plan = compile(&model).unwrap();
        let ep = &plan.phases[0].entity_plans[0];
        let score_plan = ep.field_plans.iter().find(|fp| fp.field_name == "score").unwrap();
        match &score_plan.generator_plan {
            GeneratorPlan::Distribution { round, .. } => {
                assert!(!round, "round should stay false for data_type=Float");
            }
            other => panic!("expected Distribution, got {other:?}"),
        }
    }

    #[test]
    fn test_field_plan_sequence() {
        let model = simple_model("fields", vec![simple_entity("test", 100)], vec![]);
        let plan = compile(&model).unwrap();
        let ep = &plan.phases[0].entity_plans[0];
        let id_plan = ep.field_plans.iter().find(|fp| fp.field_name == "id").unwrap();
        assert!(matches!(
            id_plan.generator_plan,
            GeneratorPlan::Sequence { start: 1, step: 1 }
        ));
    }

    #[test]
    fn test_field_plan_faker() {
        let model = simple_model("fields", vec![simple_entity("test", 100)], vec![]);
        let plan = compile(&model).unwrap();
        let ep = &plan.phases[0].entity_plans[0];
        let name_plan = ep.field_plans.iter().find(|fp| fp.field_name == "name").unwrap();
        assert!(matches!(name_plan.generator_plan, GeneratorPlan::Faker { .. }));
    }

    #[test]
    fn test_field_plan_one_of() {
        let mut entity = simple_entity("test", 100);
        entity.fields.push(Field {
            name: "status".to_string(),
            description: None,
            data_type: DataType::String,
            generator: Some(GeneratorSpec::OneOf {
                choices: vec![
                    WeightedChoice {
                        value: Value::String("active".to_string()),
                        weight: 3.0,
                    },
                    WeightedChoice {
                        value: Value::String("inactive".to_string()),
                        weight: 1.0,
                    },
                ],
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
        actor_column: false,
        });

        let model = simple_model("fields", vec![entity], vec![]);
        let plan = compile(&model).unwrap();
        let ep = &plan.phases[0].entity_plans[0];
        let status_plan = ep.field_plans.iter().find(|fp| fp.field_name == "status").unwrap();
        match &status_plan.generator_plan {
            GeneratorPlan::OneOf {
                choices,
                cumulative_weights,
            } => {
                assert_eq!(choices.len(), 2);
                assert_eq!(cumulative_weights.len(), 2);
                assert!((cumulative_weights[0] - 0.75).abs() < 1e-10);
                assert!((cumulative_weights[1] - 1.0).abs() < 1e-10);
            }
            other => panic!("expected OneOf, got {other:?}"),
        }
    }

    #[test]
    fn test_field_plan_uuid() {
        let mut entity = simple_entity("test", 100);
        entity.fields.push(Field {
            name: "uuid_field".to_string(),
            description: None,
            data_type: DataType::Uuid,
            generator: Some(GeneratorSpec::UuidGen { version: 4 }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
        actor_column: false,
        });

        let model = simple_model("fields", vec![entity], vec![]);
        let plan = compile(&model).unwrap();
        let ep = &plan.phases[0].entity_plans[0];
        let uuid_plan = ep.field_plans.iter().find(|fp| fp.field_name == "uuid_field").unwrap();
        assert!(matches!(uuid_plan.generator_plan, GeneratorPlan::Uuid));
    }

    #[test]
    fn test_field_plan_constant() {
        let mut entity = simple_entity("test", 100);
        entity.fields.push(Field {
            name: "version".to_string(),
            description: None,
            data_type: DataType::Int,
            generator: Some(GeneratorSpec::Constant {
                value: Value::Int(1),
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
        actor_column: false,
        });

        let model = simple_model("fields", vec![entity], vec![]);
        let plan = compile(&model).unwrap();
        let ep = &plan.phases[0].entity_plans[0];
        let version_plan = ep.field_plans.iter().find(|fp| fp.field_name == "version").unwrap();
        assert!(matches!(
            version_plan.generator_plan,
            GeneratorPlan::Constant(Value::Int(1))
        ));
    }

    #[test]
    fn test_field_plan_derived() {
        let mut entity = simple_entity("test", 100);
        entity.fields.push(Field {
            name: "price".to_string(),
            description: None,
            data_type: DataType::Float,
            generator: Some(GeneratorSpec::Distribution {
                spec: DistributionSpec {
                    kind: DistributionKind::Uniform,
                    params: {
                        let mut p = BTreeMap::new();
                        p.insert("min".to_string(), 1.0);
                        p.insert("max".to_string(), 100.0);
                        p
                    },
                    round: false,
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
        actor_column: false,
        });
        entity.fields.push(Field {
            name: "tax".to_string(),
            description: None,
            data_type: DataType::Float,
            generator: Some(GeneratorSpec::Derived {
                expr: "price * 0.1".to_string(),
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
        actor_column: false,
        });

        let model = simple_model("fields", vec![entity], vec![]);
        let plan = compile(&model).unwrap();
        let ep = &plan.phases[0].entity_plans[0];
        let tax_plan = ep.field_plans.iter().find(|fp| fp.field_name == "tax").unwrap();
        match &tax_plan.generator_plan {
            GeneratorPlan::Derived { expr, depends_on } => {
                assert_eq!(expr, "price * 0.1");
                assert!(depends_on.contains(&"price".to_string()));
            }
            other => panic!("expected Derived, got {other:?}"),
        }
        // Derived field should have higher dependency_order.
        let price_plan = ep.field_plans.iter().find(|fp| fp.field_name == "price").unwrap();
        assert!(tax_plan.dependency_order > price_plan.dependency_order);
    }

    #[test]
    fn test_param_refs_not_treated_as_dependencies() {
        // A field named "env" exists alongside a derived field using ${param.env}.
        // The ${param.env} should NOT create a dependency on the "env" field.
        let mut entity = simple_entity("test", 100);
        entity.fields.push(Field {
            name: "env".to_string(),
            description: None,
            data_type: DataType::String,
            generator: Some(GeneratorSpec::Constant {
                value: knit_core::Value::String("prod".into()),
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
        actor_column: false,
        });
        entity.fields.push(Field {
            name: "label".to_string(),
            description: None,
            data_type: DataType::String,
            generator: Some(GeneratorSpec::Derived {
                expr: "${param.env}-${id}".to_string(),
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
        actor_column: false,
        });

        let model = simple_model("params", vec![entity], vec![]);
        let plan = compile(&model).unwrap();
        let ep = &plan.phases[0].entity_plans[0];
        let label_plan = ep.field_plans.iter().find(|fp| fp.field_name == "label").unwrap();
        match &label_plan.generator_plan {
            GeneratorPlan::Derived { depends_on, .. } => {
                assert!(
                    depends_on.contains(&"id".to_string()),
                    "should depend on 'id'"
                );
                assert!(
                    !depends_on.contains(&"env".to_string()),
                    "should NOT depend on 'env' (it's a param ref, not a field ref)"
                );
                assert!(
                    !depends_on.contains(&"param".to_string()),
                    "should NOT depend on 'param'"
                );
            }
            other => panic!("expected Derived, got {other:?}"),
        }
    }

    #[test]
    fn test_null_plan_conversion() {
        assert!(matches!(compile_null_plan(&NullSpec::Never), NullPlan::Never));
        assert!(matches!(compile_null_plan(&NullSpec::Always), NullPlan::Always));
        assert!(matches!(
            compile_null_plan(&NullSpec::Probability(0.05)),
            NullPlan::Probability(p) if (p - 0.05).abs() < 1e-10
        ));
        assert!(matches!(
            compile_null_plan(&NullSpec::Pattern { every_n: 5 }),
            NullPlan::Pattern { every_n: 5 }
        ));
    }

    #[test]
    fn test_display() {
        let model = simple_model(
            "display_test",
            vec![simple_entity("users", 1000)],
            vec![],
        );
        let plan = compile(&model).unwrap();
        let display = format!("{plan}");
        assert!(display.contains("display_test"));
        assert!(display.contains("users"));
    }

    #[test]
    fn test_unknown_entity_error() {
        let model = simple_model(
            "error_test",
            vec![simple_entity("users", 1000)],
            vec![Relationship {
                name: "bad_rel".to_string(),
                from: "nonexistent".to_string(),
                to: "users".to_string(),
                kind: RelationshipKind::ManyToOne,
                foreign_key: None,
                cardinality: None,
            }],
        );
        let result = compile(&model);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PlanError::UnknownEntity { .. }
        ));
    }

    #[test]
    fn test_foreign_key_field_plan() {
        let mut orders = simple_entity("orders", 5000);
        orders.fields.push(Field {
            name: "user_id".to_string(),
            description: None,
            data_type: DataType::Int,
            generator: None,
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
        actor_column: false,
        });

        let model = simple_model(
            "fk_test",
            vec![simple_entity("users", 1000), orders],
            vec![Relationship {
                name: "order_user".to_string(),
                from: "orders".to_string(),
                to: "users".to_string(),
                kind: RelationshipKind::ManyToOne,
                foreign_key: Some("user_id".to_string()),
                cardinality: None,
            }],
        );

        let plan = compile(&model).unwrap();
        let orders_plan = plan
            .phases
            .iter()
            .flat_map(|p| &p.entity_plans)
            .find(|ep| ep.entity_name == "orders")
            .unwrap();
        let fk_plan = orders_plan
            .field_plans
            .iter()
            .find(|fp| fp.field_name == "user_id")
            .unwrap();
        match &fk_plan.generator_plan {
            GeneratorPlan::ForeignKey {
                target_entity,
                target_field,
                ..
            } => {
                assert_eq!(target_entity, "users");
                assert_eq!(target_field, "id");
            }
            other => panic!("expected ForeignKey, got {other:?}"),
        }
    }

    #[test]
    fn test_one_of_zero_weights() {
        let model = simple_model(
            "test",
            vec![Entity {
                name: "items".to_string(),
                description: None,
                count: CountSpec::Fixed(10),
                fields: vec![Field {
                    name: "color".to_string(),
                    description: None,
                    data_type: DataType::String,
                    generator: Some(GeneratorSpec::OneOf {
                        choices: vec![
                            WeightedChoice { value: Value::String("red".into()), weight: 0.0 },
                            WeightedChoice { value: Value::String("blue".into()), weight: 0.0 },
                        ],
                    }),
                    nullable: NullSpec::Never,
                    primary_key: None,
            precision: None,
        actor_column: false,
        }],
                constraints: vec![],
                topology: None,
            actor: false,
            persona_distribution: None,
            }],
            vec![],
        );
        let plan = compile(&model).unwrap();
        let ep = &plan.phases[0].entity_plans[0];
        let fp = ep.field_plans.iter().find(|f| f.field_name == "color").unwrap();
        match &fp.generator_plan {
            GeneratorPlan::OneOf { cumulative_weights, .. } => {
                // Should be uniform [0.5, 1.0], not [inf, inf]
                assert!(cumulative_weights.iter().all(|w| w.is_finite()));
                assert!((cumulative_weights.last().unwrap() - 1.0).abs() < 1e-9);
            }
            other => panic!("expected OneOf, got {other:?}"),
        }
    }

    #[test]
    fn test_extract_dependencies_no_substring_match() {
        let fields = vec![
            Field {
                name: "p".to_string(),
                description: None,
                data_type: DataType::Float,
                generator: None,
                nullable: NullSpec::Never,
                primary_key: None,
            precision: None,
        actor_column: false,
        },
            Field {
                name: "price".to_string(),
                description: None,
                data_type: DataType::Float,
                generator: None,
                nullable: NullSpec::Never,
                primary_key: None,
            precision: None,
        actor_column: false,
        },
        ];
        // "price * 2" should match only "price", not "p"
        let deps = extract_dependencies("price * 2", &fields);
        assert_eq!(deps, vec!["price".to_string()]);
        // "p + price" should match both
        let deps2 = extract_dependencies("p + price", &fields);
        assert_eq!(deps2.len(), 2);
    }

    #[test]
    fn test_unique_spec_compiles_to_unique_plan() {
        let inner_spec = GeneratorSpec::Sequence {
            start: 1,
            step: 1,
            prefix: None,
        };
        let spec = GeneratorSpec::Unique {
            inner: Box::new(inner_spec),
            max_retries: 50,
        };
        let plan = compile_generator_from_spec(&spec, &[], &DataType::String);
        match plan {
            GeneratorPlan::Unique { inner, max_retries } => {
                assert_eq!(max_retries, 50);
                assert!(matches!(*inner, GeneratorPlan::Sequence { start: 1, step: 1 }));
            }
            other => panic!("expected GeneratorPlan::Unique, got {other:?}"),
        }
    }

    #[test]
    fn test_business_hours_compiles_to_temporal() {
        let spec = GeneratorSpec::BusinessHours {
            start_hour: 9,
            end_hour: 17,
            exclude_weekends: true,
        };
        let plan = compile_generator_from_spec(&spec, &[], &DataType::String);
        match plan {
            GeneratorPlan::Temporal {
                kind: TemporalKind::BusinessHours,
                params,
                base_field,
            } => {
                assert_eq!(params["start_hour"], 9.0);
                assert_eq!(params["end_hour"], 17.0);
                assert_eq!(params["weekdays_only"], 1.0);
                assert!(base_field.is_none());
            }
            other => panic!("expected Temporal/BusinessHours, got {other:?}"),
        }
    }

    #[test]
    fn test_business_hours_weekends_allowed() {
        let spec = GeneratorSpec::BusinessHours {
            start_hour: 8,
            end_hour: 20,
            exclude_weekends: false,
        };
        let plan = compile_generator_from_spec(&spec, &[], &DataType::String);
        match plan {
            GeneratorPlan::Temporal {
                kind: TemporalKind::BusinessHours,
                params,
                ..
            } => {
                assert_eq!(params["weekdays_only"], 0.0);
            }
            other => panic!("expected Temporal/BusinessHours, got {other:?}"),
        }
    }

    #[test]
    fn test_relative_compiles_to_temporal() {
        let spec = GeneratorSpec::Relative {
            field: "start_date".to_string(),
            offset: Value::Int(86400),
        };
        let plan = compile_generator_from_spec(&spec, &[], &DataType::String);
        match plan {
            GeneratorPlan::Temporal {
                kind: TemporalKind::Relative,
                params,
                base_field,
            } => {
                assert_eq!(params["offset_mean"], 86400.0);
                assert!(params["offset_std"] > 0.0);
                assert_eq!(base_field, Some("start_date".to_string()));
            }
            other => panic!("expected Temporal/Relative, got {other:?}"),
        }
    }

    #[test]
    fn metadata_includes_behavioral_counts() {
        let mut entity = simple_entity("users", 100);
        entity.actor = true;
        let mut model = simple_model("test", vec![entity], vec![]);
        model.personas.push(Persona {
            name: "power_user".to_string(),
            weight: 0.3,
            traits: BTreeMap::new(),
        });
        model.personas.push(Persona {
            name: "casual".to_string(),
            weight: 0.7,
            traits: BTreeMap::new(),
        });
        model.actor_relationships.push(ActorRelationship {
            name: "network".to_string(),
            from_entity: "users".to_string(),
            to_entity: "users".to_string(),
            graph_type: GraphType::default(),
            params: BTreeMap::new(),
            community_count: None,
            hierarchy_depth: None,
        });
        let plan = compile(&model).unwrap();
        assert_eq!(plan.metadata.actor_entity_count, 1);
        assert_eq!(plan.metadata.persona_count, 2);
        assert_eq!(plan.metadata.actor_relationship_count, 1);
    }

    #[test]
    fn metadata_behavioral_counts_zero_when_no_actors() {
        let model = simple_model("test", vec![simple_entity("items", 50)], vec![]);
        let plan = compile(&model).unwrap();
        assert_eq!(plan.metadata.actor_entity_count, 0);
        assert_eq!(plan.metadata.persona_count, 0);
        assert_eq!(plan.metadata.actor_relationship_count, 0);
    }

    #[test]
    fn test_relative_dependency_ordering() {
        // Relative field should have higher order than its base field
        let fields = vec![
            Field {
                name: "end_date".to_string(),
                description: None,
                data_type: DataType::Datetime,
                nullable: NullSpec::default(),
                generator: Some(GeneratorSpec::Relative {
                    field: "start_date".to_string(),
                    offset: Value::Int(3600),
                }),
                primary_key: None,
            precision: None,
        actor_column: false,
        },
            Field {
                name: "start_date".to_string(),
                description: None,
                data_type: DataType::Datetime,
                nullable: NullSpec::default(),
                generator: Some(GeneratorSpec::Distribution {
                    spec: DistributionSpec {
                        kind: DistributionKind::Uniform,
                        params: Default::default(),
                        round: false,
                    },
                }),
                primary_key: None,
            precision: None,
        actor_column: false,
        },
        ];
        let order_end = compute_dependency_order(&fields[0], &fields);
        let order_start = compute_dependency_order(&fields[1], &fields);
        assert!(order_end > order_start, "relative field should come after base field");
    }

    #[test]
    fn default_generator_int_produces_distribution() {
        let plan = default_generator_for_type(&DataType::Int);
        match plan {
            GeneratorPlan::Distribution { kind, round, .. } => {
                assert!(matches!(kind, DistributionKind::Uniform));
                assert!(round, "int default should be rounded");
            }
            _ => panic!("Expected Distribution for Int, got {:?}", plan),
        }
    }

    #[test]
    fn default_generator_bool_produces_bernoulli() {
        let plan = default_generator_for_type(&DataType::Bool);
        match plan {
            GeneratorPlan::Distribution { kind, .. } => {
                assert!(matches!(kind, DistributionKind::Bernoulli));
            }
            _ => panic!("Expected Distribution for Bool, got {:?}", plan),
        }
    }

    #[test]
    fn default_generator_string_produces_faker() {
        let plan = default_generator_for_type(&DataType::String);
        match plan {
            GeneratorPlan::Faker { category, .. } => {
                assert_eq!(category, "word");
            }
            _ => panic!("Expected Faker for String, got {:?}", plan),
        }
    }

    #[test]
    fn default_generator_datetime_produces_faker() {
        let plan = default_generator_for_type(&DataType::Datetime);
        match plan {
            GeneratorPlan::Faker { category, .. } => {
                assert_eq!(category, "datetime");
            }
            _ => panic!("Expected Faker for Datetime, got {:?}", plan),
        }
    }

    #[test]
    fn default_generator_uuid_produces_uuid() {
        let plan = default_generator_for_type(&DataType::Uuid);
        assert!(matches!(plan, GeneratorPlan::Uuid));
    }

    #[test]
    fn default_generator_date_produces_faker_date() {
        let plan = default_generator_for_type(&DataType::Date);
        match plan {
            GeneratorPlan::Faker { category, .. } => {
                assert_eq!(category, "date");
            }
            _ => panic!("Expected Faker for Date, got {:?}", plan),
        }
    }

    #[test]
    fn default_generator_time_produces_null() {
        let plan = default_generator_for_type(&DataType::Time);
        assert!(matches!(plan, GeneratorPlan::Constant(Value::Null)));
    }

    // ── Actor pool compilation tests ─────────────────────────────────

    #[test]
    fn actor_pool_populated_from_personas() {
        let mut entity = simple_entity("users", 100);
        entity.actor = true;
        entity.persona_distribution = Some("personas".into());
        let mut model = simple_model("test", vec![entity], vec![]);
        model.personas.push(Persona {
            name: "users_power_user".to_string(),
            weight: 0.3,
            traits: BTreeMap::from([("activity_rate".to_string(), Value::Float(25.0))]),
        });
        model.personas.push(Persona {
            name: "users_casual".to_string(),
            weight: 0.7,
            traits: BTreeMap::new(),
        });

        let plan = compile(&model).unwrap();

        assert_eq!(plan.actor_pool.pools.len(), 1);
        let pool = &plan.actor_pool.pools[0];
        assert_eq!(pool.entity_name, "users");
        assert_eq!(pool.actor_count, 100);
        assert_eq!(pool.persona_weights.len(), 2);
        assert_eq!(pool.persona_weights[0].name, "users_power_user");
        assert_eq!(pool.persona_weights[0].weight, 0.3);
        assert_eq!(pool.persona_weights[1].name, "users_casual");
        assert_eq!(pool.persona_weights[1].weight, 0.7);
    }

    #[test]
    fn actor_pool_graph_plans_from_relationships() {
        let mut entity = simple_entity("users", 50);
        entity.actor = true;
        let mut model = simple_model("test", vec![entity], vec![]);
        model.actor_relationships.push(ActorRelationship {
            name: "email_network".to_string(),
            from_entity: "users".to_string(),
            to_entity: "users".to_string(),
            graph_type: GraphType::SmallWorld,
            params: BTreeMap::from([("avg_degree".to_string(), 8.0)]),
            community_count: Some(CountSpec::Fixed(3)),
            hierarchy_depth: None,
        });

        let plan = compile(&model).unwrap();

        assert_eq!(plan.actor_pool.graph_plans.len(), 1);
        let gp = &plan.actor_pool.graph_plans[0];
        assert_eq!(gp.name, "email_network");
        assert_eq!(gp.graph_type, GraphType::SmallWorld);
        assert_eq!(gp.params.get("avg_degree"), Some(&8.0));
        assert_eq!(gp.community_count, Some(3));
    }

    #[test]
    fn actor_pool_empty_when_no_actors() {
        let model = simple_model("test", vec![simple_entity("items", 100)], vec![]);
        let plan = compile(&model).unwrap();
        assert!(plan.actor_pool.pools.is_empty());
        assert!(plan.actor_pool.graph_plans.is_empty());
    }

    #[test]
    fn actor_pool_normalizes_weights() {
        let mut entity = simple_entity("users", 200);
        entity.actor = true;
        entity.persona_distribution = Some("personas".into());
        let mut model = simple_model("test", vec![entity], vec![]);
        // Weights don't sum to 1.0 — should be normalized
        model.personas.push(Persona {
            name: "users_a".to_string(),
            weight: 2.0,
            traits: BTreeMap::new(),
        });
        model.personas.push(Persona {
            name: "users_b".to_string(),
            weight: 3.0,
            traits: BTreeMap::new(),
        });

        let plan = compile(&model).unwrap();
        let pool = &plan.actor_pool.pools[0];
        let total: f64 = pool.persona_weights.iter().map(|pw| pw.weight).sum();
        assert!((total - 1.0).abs() < 1e-10);
        assert!((pool.persona_weights[0].weight - 0.4).abs() < 1e-10);
        assert!((pool.persona_weights[1].weight - 0.6).abs() < 1e-10);
    }
}

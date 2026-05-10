//! Main compilation logic: [`DataModel`] → [`ExecutionPlan`].
//!
//! This module contains the [`compile()`] function — the primary entry point for
//! `knit-plan`. It orchestrates dependency analysis, partition planning, RNG tree
//! construction, and field plan compilation into a single coherent execution plan.

use std::collections::{BTreeMap, HashMap};

use crate::core::{
    DataModel, DataType, DistributionKind, Entity, Field, GeneratorSpec, NullSpec, RelativeOffset,
    Value,
};

use crate::plan::error::PlanError;
use crate::plan::graph;
use crate::plan::partition;
use crate::plan::rng_tree;
use crate::plan::types::*;

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

    // 2. Resolve row counts (static).
    let mut row_counts = graph::resolve_row_counts(model)
        .map_err(PlanError::Other)?;

    // 2b. Build actor pool plan early to compute dynamic row counts.
    let actor_pool = compile_actor_pool(model, &row_counts);

    // 2c. Override row counts for entities with activity_count specification.
    apply_activity_counts(model, &actor_pool, &mut row_counts);

    // 3. Build entity lookup.
    let entity_map: HashMap<&str, &Entity> = model
        .entities
        .iter()
        .map(|e| (e.name.as_str(), e))
        .collect();

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
            let entity =
                entity_map
                    .get(entity_name.as_str())
                    .ok_or_else(|| PlanError::UnknownEntity {
                        name: entity_name.clone(),
                    })?;

            let row_count = row_counts.get(entity_name).copied().unwrap_or(1000);
            let entity_seed = rng_tree::derive_seed(model.seed, entity_name.as_bytes());
            let partitions = {
                let computed = partition::compute_partitions(row_count, entity_seed);
                // Force single partition for entities with thread_ref (requires ordered generation)
                let has_thread_ref = entity
                    .fields
                    .iter()
                    .any(|f| matches!(&f.generator, Some(GeneratorSpec::ThreadRef { .. })));
                if has_thread_ref && computed.len() > 1 {
                    vec![PartitionRange {
                        partition_id: 0,
                        start_row: 0,
                        end_row: row_count,
                        seed: entity_seed,
                    }]
                } else {
                    computed
                }
            };
            let num_partitions = partitions.len() as u32;

            let entity_fks = fk_fields.get(entity_name).cloned().unwrap_or_default();
            let mut field_plans = compile_field_plans(
                entity,
                &entity_fks,
                &row_counts,
                &index_strategy,
                &model.actor_relationships,
                model,
            );

            // Apply conditional distribution correlations: override the
            // dependent field's generator with a Conditional plan that
            // branches on the given field using per-branch distributions.
            apply_conditional_distribution_overrides(
                entity_name,
                &mut field_plans,
                &model.correlations,
            );

            // Append edge property fields from relationships where this entity
            // is the FK-holding side (from == entity_name).
            for rel in &model.relationships {
                if rel.from == *entity_name && !rel.properties.is_empty() {
                    for ep in &rel.properties {
                        let gen_plan = compile_edge_property_generator(ep, &entity.fields);
                        let null_plan = compile_null_plan(&ep.nullable);
                        field_plans.push(FieldPlan {
                            field_name: ep.name.clone(),
                            data_type: ep.data_type.clone(),
                            generator_plan: gen_plan,
                            null_plan,
                            dependency_order: 0,
                            schema_position: field_plans.len(),
                            precision: None,
                            actor_column: false,
                            sub_field_plans: vec![],
                        });
                    }
                }
            }

            let estimated_byte_size = estimate_byte_size(entity, row_count);

            // Include edge property names in the RNG tree
            let mut field_names: Vec<String> =
                entity.fields.iter().map(|f| f.name.clone()).collect();
            for rel in &model.relationships {
                if rel.from == *entity_name {
                    for ep in &rel.properties {
                        field_names.push(ep.name.clone());
                    }
                }
            }
            rng_entities.push((entity_name.clone(), field_names, num_partitions));

            total_partitions += partitions.len();
            estimated_total_rows += row_count;
            estimated_total_bytes += estimated_byte_size;

            // Find primary-key field index in the dependency-sorted field_plans,
            // not the original entity.fields order.
            let primary_key_field_index = {
                let pk_name = entity
                    .fields
                    .iter()
                    .find(|f| f.primary_key.unwrap_or(false))
                    .map(|f| &f.name);
                pk_name.and_then(|name| field_plans.iter().position(|fp| &fp.field_name == name))
            };

            entity_plans.push(EntityPlan {
                entity_name: entity_name.clone(),
                partitions,
                field_plans,
                estimated_row_count: row_count,
                estimated_byte_size,
                primary_key_field_index,
                copula_plans: compile_copula_plans(entity_name, entity, &model.correlations),
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

    // 8. Build metadata.
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

/// Override row counts for entities with `activity_count` specification.
///
/// For each entity with `activity_count`, computes the expected total rows
/// as: Σ(persona.weight × actor_count × trait_value) across all personas.
/// This gives the expected sum of per-actor activity rates.
fn apply_activity_counts(
    model: &DataModel,
    actor_pool: &ActorPoolPlan,
    row_counts: &mut BTreeMap<String, u64>,
) {
    for entity in &model.entities {
        let ac = match &entity.activity_count {
            Some(ac) => ac,
            None => continue,
        };

        // Find the actor entity referenced by the actor_field FK.
        // Look up the relationship to find which entity the FK points to.
        let actor_entity = model
            .relationships
            .iter()
            .find(|r| {
                r.from == entity.name
                    && r.foreign_key.as_deref().unwrap_or(&format!("{}_id", r.to)) == ac.actor_field
            })
            .map(|r| r.to.as_str());

        let actor_entity = match actor_entity {
            Some(e) => e,
            None => {
                // Could not resolve actor entity from FK field — skip.
                continue;
            }
        };

        // Find the actor entity's pool in the actor pool plan.
        let pool = actor_pool
            .pools
            .iter()
            .find(|p| p.entity_name == actor_entity);

        let pool = match pool {
            Some(p) => p,
            None => {
                // Actor entity has no pool — fall back to static count.
                continue;
            }
        };

        // Compute expected total: Σ(persona.weight × actor_count × trait_value)
        let mut expected_total: f64 = 0.0;
        for pw in &pool.persona_weights {
            let trait_value = match pw.traits.get(&ac.trait_name) {
                Some(Value::Float(f)) => *f,
                Some(Value::Int(i)) => *i as f64,
                _ => {
                    // Trait not found or not numeric — treat as 0.
                    0.0
                }
            };
            expected_total += pw.weight * pool.actor_count as f64 * trait_value;
        }

        let dynamic_count = expected_total.round().max(0.0) as u64;
        row_counts.insert(entity.name.clone(), dynamic_count);
    }
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
            .filter(|p| {
                p.name.starts_with(&entity_prefix) || !has_entity_prefix(&p.name, &model.entities)
            })
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
        let community_count = rel.community_count.as_ref().map(resolve_count_estimate);

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
fn resolve_count_estimate(count: &crate::core::CountSpec) -> u64 {
    match count {
        crate::core::CountSpec::Fixed(n) => *n,
        crate::core::CountSpec::Range { min, max } => (min + max) / 2,
        crate::core::CountSpec::Expression { .. } => {
            // Expression counts are resolved at plan time with params.
            // For internal estimation without params, use a reasonable default.
            1000
        }
        crate::core::CountSpec::Distribution(spec) => {
            // Try common distribution parameters for mean estimate
            if let Some(&mean) = spec.params.get("mean").or_else(|| spec.params.get("mu")) {
                mean.max(1.0) as u64
            } else if let Some(&lambda) = spec.params.get("lambda") {
                lambda.max(1.0) as u64
            } else if let (Some(&min), Some(&max)) =
                (spec.params.get("min"), spec.params.get("max"))
            {
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
    actor_relationships: &[crate::core::ActorRelationship],
    model: &DataModel,
) -> Vec<FieldPlan> {
    let fk_map: HashMap<&str, &str> = entity_fks
        .iter()
        .map(|(target, fk_field)| (fk_field.as_str(), target.as_str()))
        .collect();

    let mut plans: Vec<FieldPlan> = Vec::new();

    for (schema_pos, field) in entity.fields.iter().enumerate() {
        // Check if field has an explicit RelationshipRef or ThreadRef generator — if so, it takes
        // precedence over the inferred FK path (graph-aware sampling vs uniform FK).
        let has_relationship_ref = matches!(
            &field.generator,
            Some(GeneratorSpec::RelationshipRef { .. }) | Some(GeneratorSpec::ThreadRef { .. })
        );

        // Check if this field is a foreign key.
        // Use FK generator for Int/Int32/Uuid/String typed fields,
        // unless the field explicitly uses relationship_ref.
        let generator_plan = if !has_relationship_ref {
            if let Some(&target_entity) = fk_map.get(field.name.as_str()) {
                let is_fk_compatible = matches!(
                    field.data_type,
                    crate::core::DataType::Int
                        | crate::core::DataType::Int32
                        | crate::core::DataType::Uuid
                        | crate::core::DataType::String
                );
                if is_fk_compatible {
                    let target_rows = row_counts.get(target_entity).copied().unwrap_or(1000);
                    let key_store_kind = select_key_store_kind(target_rows);
                    // Look up degree distribution from the relationship definition.
                    let rel_match = model
                        .relationships
                        .iter()
                        .find(|r| {
                            r.from == entity.name
                                && r.to == target_entity
                                && r.foreign_key
                                    .as_deref()
                                    .unwrap_or(&format!("{}_id", r.to))
                                    == field.name
                        });
                    let degree = rel_match
                        .and_then(|r| r.degree.as_ref())
                        .map(|spec| {
                            crate::plan::DegreePlan {
                                kind: spec.kind.clone(),
                                params: spec.params.clone(),
                                parent_count: target_rows,
                            }
                        });
                    let selection = rel_match
                        .and_then(|r| r.selection.as_ref())
                        .and_then(|s| {
                            use crate::core::{
                                SelectionStrategy, SimpleSelection, ParameterizedSelection,
                            };
                            match s {
                                SelectionStrategy::Simple(SimpleSelection::Uniform) => None,
                                SelectionStrategy::Simple(SimpleSelection::Sequential) => {
                                    Some(SelectionPlan::Sequential)
                                }
                                SelectionStrategy::Parameterized(
                                    ParameterizedSelection::Clustered { cluster_size },
                                ) => Some(SelectionPlan::Clustered {
                                    cluster_size: *cluster_size,
                                }),
                                SelectionStrategy::Parameterized(
                                    ParameterizedSelection::Weighted { .. },
                                ) => {
                                    // Weighted selection deferred — fall back to uniform
                                    tracing::warn!(
                                        entity = %entity.name,
                                        field = %field.name,
                                        "weighted selection strategy not yet implemented — falling back to uniform"
                                    );
                                    None
                                }
                            }
                        });
                    GeneratorPlan::ForeignKey {
                        target_entity: target_entity.to_string(),
                        target_field: "id".to_string(),
                        key_store_kind,
                        degree,
                        selection,
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
        // and PersonaField/ActorTemporal actor_entity from FK map
        let generator_plan = match generator_plan {
            GeneratorPlan::GraphTarget {
                graph_name,
                source_field,
                key_store_kind,
                ..
            } => {
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
            GeneratorPlan::PersonaField {
                trait_name,
                actor_field,
                ..
            } => {
                let actor_entity = fk_map
                    .get(actor_field.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                GeneratorPlan::PersonaField {
                    trait_name,
                    actor_entity,
                    actor_field,
                }
            }
            GeneratorPlan::ActorTemporal {
                trait_name,
                actor_field,
                temporal_after,
                burst,
                ..
            } => {
                let actor_entity = fk_map
                    .get(actor_field.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                // Auto-detect temporal_start_field: prefer a datetime field
                // with a creation/signup-like name. Fall back to the sole
                // datetime field only when exactly one exists.
                let temporal_start_field = if !actor_entity.is_empty() {
                    let actor_ent = model.entities.iter().find(|e| e.name == actor_entity);
                    actor_ent.and_then(|e| {
                        let dt_fields: Vec<&str> = e
                            .fields
                            .iter()
                            .filter(|f| {
                                matches!(
                                    f.data_type,
                                    crate::core::DataType::Datetime
                                        | crate::core::DataType::DatetimeUs
                                        | crate::core::DataType::Datetimetz
                                        | crate::core::DataType::Date
                                )
                            })
                            .map(|f| f.name.as_str())
                            .collect();
                        // Try to find a creation/signup-like field first
                        let creation_names =
                            ["signup", "created", "registered", "joined", "creation"];
                        let creation_field = dt_fields.iter().find(|name| {
                            let lower = name.to_lowercase();
                            creation_names.iter().any(|kw| lower.contains(kw))
                        });
                        if let Some(field) = creation_field {
                            Some(field.to_string())
                        } else if dt_fields.len() == 1 {
                            Some(dt_fields[0].to_string())
                        } else {
                            None // ambiguous or none — skip auto-detection
                        }
                    })
                } else {
                    None
                };
                GeneratorPlan::ActorTemporal {
                    trait_name,
                    actor_entity,
                    actor_field,
                    temporal_start_field,
                    min_event_gap_ms: None,
                    temporal_after,
                    burst,
                }
            }
            other => other,
        };

        let null_plan = compile_null_plan(&field.nullable);
        let dependency_order = compute_dependency_order(field, &entity.fields);

        // For object fields, recursively compile sub-field plans
        let (generator_plan, sub_field_plans) = if field.data_type == crate::core::DataType::Object
        {
            let sub_plans = compile_object_sub_fields(&field.fields);
            (GeneratorPlan::Struct, sub_plans)
        } else {
            (generator_plan, vec![])
        };

        plans.push(FieldPlan {
            field_name: field.name.clone(),
            data_type: field.data_type.clone(),
            generator_plan,
            null_plan,
            dependency_order,
            schema_position: schema_pos,
            precision: field.precision,
            actor_column: field.actor_column,
            sub_field_plans,
        });
    }

    // Sort by dependency_order for correct generation ordering.
    plans.sort_by_key(|fp| fp.dependency_order);
    plans
}

/// Recursively compile sub-fields for a nested object field.
fn compile_object_sub_fields(fields: &[Field]) -> Vec<FieldPlan> {
    fields
        .iter()
        .map(|sub| {
            let generator_plan = if sub.data_type == crate::core::DataType::Object {
                GeneratorPlan::Struct
            } else {
                compile_generator(sub, fields)
            };
            let null_plan = compile_null_plan(&sub.nullable);
            let sub_field_plans = if sub.data_type == crate::core::DataType::Object {
                compile_object_sub_fields(&sub.fields)
            } else {
                vec![]
            };
            FieldPlan {
                field_name: sub.name.clone(),
                data_type: sub.data_type.clone(),
                generator_plan,
                null_plan,
                dependency_order: 0,
                schema_position: 0,
                precision: sub.precision,
                actor_column: false,
                sub_field_plans,
            }
        })
        .collect()
}

/// Convert a `GeneratorSpec` to a `GeneratorPlan`.
fn compile_generator(field: &Field, all_fields: &[Field]) -> GeneratorPlan {
    match &field.generator {
        Some(spec) => match spec {
            GeneratorSpec::Distribution { spec: dist_spec } => {
                // Auto-enable rounding when the field's declared data type is integer.
                // This ensures distribution generators produce integer values even when
                // the user doesn't explicitly set `round = true` in the schema.
                let round =
                    dist_spec.round || matches!(field.data_type, DataType::Int | DataType::Int32);
                GeneratorPlan::Distribution {
                    kind: dist_spec.kind.clone(),
                    params: dist_spec.params.clone(),
                    array_params: dist_spec.array_params.clone(),
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
                values,
                cycle: _,
                jitter,
            } => {
                if let Some(vals) = values {
                    GeneratorPlan::CyclicValues {
                        values: vals.clone(),
                    }
                } else {
                    let start_ms = resolve_int_or_string_to_i64(start);
                    let step_ms = resolve_step_to_i64(step);
                    let jitter_ms = jitter.as_deref().map(|s| {
                        crate::gen::generators::event_stream::parse_duration_ms(s)
                    });
                    GeneratorPlan::Sequence {
                        start: start_ms,
                        step: step_ms,
                        jitter_ms,
                    }
                }
            }
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
                    let w = if total_weight == 0.0 {
                        1.0
                    } else {
                        choice.weight
                    };
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
                    let element = Box::new(compile_generator_from_spec(
                        first_gen,
                        all_fields,
                        &field.data_type,
                    ));
                    let length = Box::new(GeneratorPlan::Constant(crate::core::Value::Int(1)));
                    GeneratorPlan::Composite { element, length }
                } else {
                    GeneratorPlan::Constant(crate::core::Value::String(String::new()))
                }
            }
            GeneratorSpec::Lookup { entity, field } => GeneratorPlan::ForeignKey {
                target_entity: entity.clone(),
                target_field: field.clone(),
                key_store_kind: KeyStoreKind::InMemoryVec,
                degree: None,
                selection: None,
            },
            GeneratorSpec::Pattern { pattern } => GeneratorPlan::Pattern {
                pattern: pattern.clone(),
            },
            GeneratorSpec::Unique { inner, max_retries } => {
                let inner_plan = compile_generator_from_spec(inner, all_fields, &field.data_type);
                GeneratorPlan::Unique {
                    inner: Box::new(inner_plan),
                    max_retries: *max_retries,
                }
            }
            GeneratorSpec::Relative { anchor, offset } => {
                compile_relative_offset(anchor, offset)
            }
            GeneratorSpec::BusinessHours {
                start_hour,
                end_hour,
                exclude_weekends,
                timezone,
                timezone_field,
                date_range,
                exclude_dates,
                days,
            } => {
                let mut params = BTreeMap::new();
                params.insert("start_hour".into(), *start_hour as f64);
                params.insert("end_hour".into(), *end_hour as f64);

                // days bitmask: bit 0=Mon, bit 1=Tue, ..., bit 6=Sun
                let days_mask = if let Some(day_list) = days {
                    day_list.iter().fold(0u8, |mask, d| {
                        mask | match d.to_lowercase().as_str() {
                            "mon" | "monday" => 0x01,
                            "tue" | "tuesday" => 0x02,
                            "wed" | "wednesday" => 0x04,
                            "thu" | "thursday" => 0x08,
                            "fri" | "friday" => 0x10,
                            "sat" | "saturday" => 0x20,
                            "sun" | "sunday" => 0x40,
                            _ => 0,
                        }
                    })
                } else if *exclude_weekends {
                    0x1F // Mon-Fri
                } else {
                    0x7F // All days
                };
                params.insert("days_mask".into(), days_mask as f64);

                // Date range as epoch-day offsets
                if let Some(dr) = date_range {
                    if let Ok(d) = chrono::NaiveDate::parse_from_str(&dr.min, "%Y-%m-%d") {
                        params.insert(
                            "date_range_min_ms".into(),
                            d.and_hms_opt(0, 0, 0)
                                .unwrap()
                                .and_utc()
                                .timestamp_millis() as f64,
                        );
                    }
                    if let Ok(d) = chrono::NaiveDate::parse_from_str(&dr.max, "%Y-%m-%d") {
                        params.insert(
                            "date_range_max_ms".into(),
                            d.and_hms_opt(0, 0, 0)
                                .unwrap()
                                .and_utc()
                                .timestamp_millis() as f64,
                        );
                    }
                }

                let mut string_params = BTreeMap::new();
                if let Some(tz) = timezone {
                    string_params.insert("timezone".into(), tz.clone());
                }
                if let Some(tz_field) = timezone_field {
                    string_params.insert("timezone_field".into(), tz_field.clone());
                }
                if !exclude_dates.is_empty() {
                    string_params.insert("exclude_dates".into(), exclude_dates.join(","));
                }

                GeneratorPlan::Temporal {
                    kind: TemporalKind::BusinessHours,
                    params,
                    base_field: timezone_field.clone(),
                    string_params,
                }
            }
            GeneratorSpec::Conditional {
                field: cond_field,
                branches,
                default,
            } => {
                // Compile each branch's generator recursively
                let compiled_branches: Vec<(Value, Box<GeneratorPlan>)> = branches
                    .iter()
                    .map(|b| {
                        let plan =
                            compile_generator_from_spec(&b.generator, all_fields, &field.data_type);
                        (b.condition.clone(), Box::new(plan))
                    })
                    .collect();
                let default_plan = match default {
                    Some(gen) => Box::new(compile_generator_from_spec(
                        gen,
                        all_fields,
                        &field.data_type,
                    )),
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
            GeneratorSpec::ExternalLookup {
                source,
                column,
                format,
                sampling,
                weight_column,
            } => {
                // Entries are loaded by the CLI layer after compilation,
                // which resolves the file path relative to the schema.
                GeneratorPlan::ExternalLookup {
                    entries: vec![],
                    weights: None,
                    sampling: sampling.clone(),
                    source_file: Some(source.clone()),
                    source_column: Some(column.clone()),
                    weight_column: weight_column.clone(),
                    source_format: Some(format.clone()),
                }
            }
            // Behavioral modeling generators — placeholder plans until
            // the generation engine implements persona/graph-based generation.
            GeneratorSpec::ActorRef { entity } => GeneratorPlan::ForeignKey {
                target_entity: entity.clone(),
                target_field: "id".to_string(),
                key_store_kind: KeyStoreKind::InMemoryVec,
                degree: None,
                selection: None,
            },
            GeneratorSpec::ActorTemporal {
                trait_name,
                temporal_after,
                burst,
            } => {
                // Auto-detect the actor FK field from actor_column fields.
                let actor_field = all_fields
                    .iter()
                    .find(|f| f.actor_column && f.name != field.name)
                    .map(|f| f.name.clone())
                    .unwrap_or_default();
                let ta_plan = temporal_after
                    .as_ref()
                    .map(|ta| crate::plan::types::TemporalAfter {
                        entity: ta.entity.clone(),
                        field: ta.field.clone(),
                        fk: ta.fk.clone(),
                    });
                let burst_plan = burst.as_ref().map(|b| crate::plan::types::BurstPlan {
                    avg_events: b.avg_events,
                    avg_gap_ms: (b.avg_gap_minutes * 60_000.0) as i64,
                    avg_idle_ms: (b.avg_idle_hours * 3_600_000.0) as i64,
                });
                GeneratorPlan::ActorTemporal {
                    trait_name: trait_name.clone(),
                    actor_entity: String::new(), // resolved in compile_field_plans
                    actor_field,
                    temporal_start_field: None, // resolved in compile_field_plans
                    min_event_gap_ms: None,     // uses default
                    temporal_after: ta_plan,
                    burst: burst_plan,
                }
            }
            GeneratorSpec::RelationshipRef {
                relationship,
                source_field,
            } => {
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
                    from_entity: String::new(),   // resolved below
                    target_entity: String::new(), // resolved below
                    key_store_kind: KeyStoreKind::InMemoryVec,
                }
            }
            GeneratorSpec::PersonaField { trait_name } => {
                // Auto-detect the actor FK field from actor_column fields.
                let actor_field = all_fields
                    .iter()
                    .find(|f| f.actor_column && f.name != field.name)
                    .map(|f| f.name.clone())
                    .unwrap_or_default();
                GeneratorPlan::PersonaField {
                    trait_name: trait_name.clone(),
                    actor_entity: String::new(), // resolved in compile_field_plans
                    actor_field,
                }
            }
            GeneratorSpec::ThreadRef {
                reply_probability,
                max_depth,
                reply_window,
            } => {
                // Find the PK field in this entity for self-referential threading.
                let pk_field = all_fields
                    .iter()
                    .find(|f| f.primary_key.unwrap_or(false))
                    .map(|f| f.name.clone())
                    .unwrap_or_else(|| "id".to_string());
                GeneratorPlan::ThreadRef {
                    reply_probability: *reply_probability,
                    max_depth: *max_depth,
                    reply_window: *reply_window,
                    pk_field,
                }
            }
            GeneratorSpec::Plugin { name, params } => GeneratorPlan::Plugin {
                name: name.clone(),
                params: params.clone(),
            },
            GeneratorSpec::TimeSeries {
                baseline,
                components,
                min,
                max,
                timestamp_field,
            } => {
                let needs_sequential = components.iter().any(|c| {
                    matches!(
                        c,
                        crate::core::TimeSeriesComponent::Autoregressive { .. }
                            | crate::core::TimeSeriesComponent::LevelShift { .. }
                            | crate::core::TimeSeriesComponent::Spike { .. }
                            | crate::core::TimeSeriesComponent::MeanReversion { .. }
                    )
                });
                GeneratorPlan::NumericTimeSeries {
                    baseline: *baseline,
                    components: components.clone(),
                    min: *min,
                    max: *max,
                    timestamp_field: timestamp_field.clone(),
                    needs_sequential,
                }
            },
            GeneratorSpec::EventStream {
                start,
                arrival,
                components,
            } => {
                // Parse start time to epoch milliseconds.
                let start_ms = chrono::DateTime::parse_from_rfc3339(start)
                    .map(|dt| dt.timestamp_millis())
                    .unwrap_or_else(|_| {
                        // Try naive datetime (no timezone) — assume UTC.
                        chrono::NaiveDateTime::parse_from_str(start, "%Y-%m-%dT%H:%M:%S")
                            .map(|ndt| ndt.and_utc().timestamp_millis())
                            .unwrap_or(0)
                    });

                // Convert arrival rate to events per millisecond.
                let lambda_raw = match arrival.params.get("lambda") {
                    Some(crate::core::Value::Float(f)) => *f,
                    Some(crate::core::Value::Int(i)) => *i as f64,
                    _ => 1.0,
                };
                let unit_ms = match arrival.unit.as_str() {
                    "second" | "seconds" | "s" => 1_000.0,
                    "minute" | "minutes" | "m" => 60_000.0,
                    "hour" | "hours" | "h" => 3_600_000.0,
                    "day" | "days" | "d" => 86_400_000.0,
                    "millisecond" | "milliseconds" | "ms" => 1.0,
                    _ => 1_000.0,
                };
                let lambda_per_ms = lambda_raw / unit_ms;

                GeneratorPlan::EventStream {
                    start_ms,
                    lambda_per_ms,
                    components: components.clone(),
                }
            },
        },
        None => {
            // No generator specified — provide a sensible default based on data_type.
            if field.primary_key.unwrap_or(false) && field.data_type == crate::core::DataType::Uuid {
                GeneratorPlan::Uuid
            } else if field.primary_key.unwrap_or(false) {
                GeneratorPlan::Sequence { start: 1, step: 1, jitter_ms: None }
            } else {
                default_generator_for_type(&field.data_type)
            }
        }
    }
}

/// Compile a `RelativeOffset` into a `GeneratorPlan::Temporal`.
///
/// Handles three offset variants:
/// - `Simple(Value)` — backward compatible, uses Normal distribution
/// - `Distribution { ... }` — configurable distribution with min/max clamping
/// - `Constant { value }` — fixed offset, no randomness
fn compile_relative_offset(anchor: &str, offset: &RelativeOffset) -> GeneratorPlan {
    use crate::gen::generators::event_stream::parse_duration_ms;

    let mut params = BTreeMap::new();
    let mut string_params = BTreeMap::new();

    match offset {
        RelativeOffset::Simple(val) => {
            let offset_val = match val {
                Value::Int(n) => *n as f64,
                Value::Float(f) => *f,
                _ => 60.0,
            };
            params.insert("offset_mean".into(), offset_val);
            params.insert("offset_std".into(), (offset_val.abs() * 0.1).max(1.0));
            // mode 0 = legacy Normal
            params.insert("offset_mode".into(), 0.0);
        }
        RelativeOffset::Distribution {
            distribution,
            params: dist_params,
            min,
            max,
            unit,
        } => {
            // mode 1 = distribution
            params.insert("offset_mode".into(), 1.0);
            // Forward distribution params
            for (k, v) in dist_params {
                params.insert(k.clone(), *v);
            }
            // Encode distribution kind as string
            string_params.insert(
                "distribution".into(),
                serde_json::to_string(distribution).unwrap_or_else(|_| "\"normal\"".into()),
            );
            // Parse min/max as durations → milliseconds
            if let Some(min_str) = min {
                let min_ms = parse_duration_ms(min_str);
                params.insert("min_ms".into(), min_ms as f64);
            }
            if let Some(max_str) = max {
                let max_ms = parse_duration_ms(max_str);
                params.insert("max_ms".into(), max_ms as f64);
            }
            // Unit for scaling distribution output
            if let Some(u) = unit {
                let unit_val = match u.to_lowercase().as_str() {
                    "second" | "seconds" | "s" => 0.0,
                    "minute" | "minutes" | "m" => 1.0,
                    "hour" | "hours" | "h" => 2.0,
                    "day" | "days" | "d" => 3.0,
                    _ => 0.0,
                };
                params.insert("unit".into(), unit_val);
            }
        }
        RelativeOffset::Constant {
            offset_type: _,
            value,
        } => {
            // mode 2 = constant
            params.insert("offset_mode".into(), 2.0);
            let constant_ms = parse_duration_ms(value);
            params.insert("constant_ms".into(), constant_ms as f64);
        }
    }

    GeneratorPlan::Temporal {
        kind: TemporalKind::Relative,
        params,
        base_field: Some(anchor.to_string()),
        string_params,
    }
}

/// Convert a `GeneratorSpec` directly (for nested generators).
///
/// `parent_data_type` carries the owning field's declared type so that nested
/// distribution generators can auto-enable rounding for integer fields.
fn compile_generator_from_spec(
    spec: &GeneratorSpec,
    all_fields: &[Field],
    parent_data_type: &crate::core::DataType,
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
        fields: vec![],
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
fn default_generator_for_type(data_type: &crate::core::DataType) -> GeneratorPlan {
    use crate::core::DataType;

    match data_type {
        DataType::Bool => {
            let mut params = BTreeMap::new();
            params.insert("p".to_string(), 0.5);
            GeneratorPlan::Distribution {
                kind: DistributionKind::Bernoulli,
                params,
                array_params: BTreeMap::new(),
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
                array_params: BTreeMap::new(),
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
                array_params: BTreeMap::new(),
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
        | DataType::Map
        | DataType::Object => GeneratorPlan::Constant(crate::core::Value::Null),
        DataType::Custom(ref name) => {
            unreachable!("custom type '{}' should be resolved before planning", name)
        }
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

/// Compile a generator plan for an edge property.
/// Uses the same compilation pipeline as regular entity fields.
fn compile_edge_property_generator(
    prop: &crate::core::EdgeProperty,
    entity_fields: &[Field],
) -> GeneratorPlan {
    match &prop.generator {
        Some(spec) => {
            compile_generator_from_spec(spec, entity_fields, &prop.data_type)
        }
        None => default_generator_for_type(&prop.data_type),
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
        Some(GeneratorSpec::Relative { anchor: base, .. }) => {
            let base_order = all_fields
                .iter()
                .find(|f| f.name == *base)
                .map(|f| compute_dependency_order(f, all_fields))
                .unwrap_or(0);
            base_order + 1
        }
        // Conditional depends on the field it branches on + any deps inside branches
        Some(GeneratorSpec::Conditional {
            field: ref_field,
            branches,
            default,
        }) => {
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
        // ActorTemporal/PersonaField depend on the actor FK field
        Some(GeneratorSpec::ActorTemporal { temporal_after, .. }) => {
            // Depends on any actor_column field in the entity
            let max_actor_order = all_fields
                .iter()
                .filter(|f| f.actor_column && f.name != field.name)
                .map(|f| compute_dependency_order(f, all_fields))
                .max()
                .unwrap_or(0);
            // Also depends on the causal FK field if temporal_after is set
            let causal_order = temporal_after
                .as_ref()
                .and_then(|ta| {
                    all_fields
                        .iter()
                        .find(|f| f.name == ta.fk)
                        .map(|f| compute_dependency_order(f, all_fields))
                })
                .unwrap_or(0);
            max_actor_order.max(causal_order) + 1
        }
        Some(GeneratorSpec::PersonaField { .. }) => {
            // Depends on any actor_column field in the entity
            let max_actor_order = all_fields
                .iter()
                .filter(|f| f.actor_column && f.name != field.name)
                .map(|f| compute_dependency_order(f, all_fields))
                .max()
                .unwrap_or(0);
            max_actor_order + 1
        }
        Some(GeneratorSpec::ThreadRef { .. }) => {
            // Depends on the PK field (must read generated PKs from batch_columns)
            let pk_order = all_fields
                .iter()
                .find(|f| f.primary_key.unwrap_or(false))
                .map(|f| compute_dependency_order(f, all_fields))
                .unwrap_or(0);
            pk_order + 1
        }
        Some(GeneratorSpec::TimeSeries {
            timestamp_field, ..
        }) => {
            // Depends on the timestamp field if specified (must be generated first
            // so calendar-aware components can read it from batch_columns).
            if let Some(ts_name) = timestamp_field {
                let ts_order = all_fields
                    .iter()
                    .find(|f| f.name == *ts_name)
                    .map(|f| compute_dependency_order(f, all_fields))
                    .unwrap_or(0);
                ts_order + 1
            } else {
                0
            }
        }
        // BusinessHours with timezone_field depends on the timezone field
        Some(GeneratorSpec::BusinessHours {
            timezone_field: Some(ref tz_f),
            ..
        }) => {
            let tz_order = all_fields
                .iter()
                .find(|f| f.name == *tz_f)
                .map(|f| compute_dependency_order(f, all_fields))
                .unwrap_or(0);
            tz_order + 1
        }
        _ => 0,
    }
}

/// Extract field names referenced in a derived expression.
/// Simple heuristic: look for field names from `all_fields` that appear in the expression.
fn extract_dependencies(expr: &str, all_fields: &[Field]) -> Vec<String> {
    // Try AST-based extraction first (more accurate)
    if let Ok(ast) = crate::gen::expr::parser::parse(expr) {
        let refs = crate::gen::expr::ast::extract_field_refs(&ast);
        let field_names: std::collections::HashSet<&str> =
            all_fields.iter().map(|f| f.name.as_str()).collect();
        return refs
            .into_iter()
            .filter(|r| field_names.contains(r.as_str()))
            .collect();
    }

    // Fallback: legacy string heuristic
    let stripped = strip_param_refs(expr);
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
        data_type: crate::core::DataType::String,
        nullable: NullSpec::default(),
        generator: Some(spec.clone()),
        primary_key: None,
        precision: None,
        actor_column: false,
        fields: vec![],
    };
    compute_dependency_order(&tmp, all_fields)
}

/// Estimate byte size for an entity based on field types and row count.
fn estimate_byte_size(entity: &Entity, row_count: u64) -> u64 {
    let bytes_per_row: u64 = entity
        .fields
        .iter()
        .map(|f| match f.data_type {
            crate::core::DataType::Bool => 1,
            crate::core::DataType::Int | crate::core::DataType::Int32 => 8,
            crate::core::DataType::Float => 8,
            crate::core::DataType::String => 64,
            crate::core::DataType::Uuid => 16,
            crate::core::DataType::Date => 4,
            crate::core::DataType::Time => 8,
            crate::core::DataType::Datetime
            | crate::core::DataType::DatetimeUs
            | crate::core::DataType::Datetimetz => 8,
            crate::core::DataType::Duration => 8,
            crate::core::DataType::Bytes => 128,
            crate::core::DataType::Array => 128,
            crate::core::DataType::Map => 256,
            crate::core::DataType::Object => 256,
            crate::core::DataType::Custom(_) => 64, // resolved before planning, but estimate conservatively
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

/// Compile copula plans for an entity from the model's correlations.
fn compile_copula_plans(
    entity_name: &str,
    entity: &Entity,
    correlations: &[crate::core::Correlation],
) -> Vec<CopulaPlan> {
    correlations
        .iter()
        .filter(|c| c.entity == entity_name && c.copula.is_some())
        .filter_map(|c| {
            let copula = c.copula.as_ref().unwrap();
            let n = c.fields.len();

            // Build marginal info from field distribution generators
            let marginals: Vec<MarginalInfo> = c
                .fields
                .iter()
                .filter_map(|field_name| {
                    let field = entity.fields.iter().find(|f| &f.name == field_name)?;
                    match &field.generator {
                        Some(GeneratorSpec::Distribution { spec }) => Some(MarginalInfo {
                            kind: spec.kind.clone(),
                            params: spec.params.clone(),
                            round: spec.round,
                        }),
                        _ => None,
                    }
                })
                .collect();

            // Skip if not all fields have distribution generators
            if marginals.len() != n {
                return None;
            }

            let cholesky_l = if copula.family == crate::core::CopulaFamily::Gaussian {
                let result = cholesky_decompose(&c.matrix);
                if result.is_none() {
                    tracing::warn!(
                        entity = %entity.name,
                        fields = ?c.fields,
                        "Cholesky decomposition failed for Gaussian copula — \
                         correlation matrix may be singular; copula will use \
                         identity (independent) fallback"
                    );
                }
                result
            } else {
                None
            };

            let theta = copula.params.get("theta").copied();

            Some(CopulaPlan {
                fields: c.fields.clone(),
                family: copula.family,
                cholesky_l,
                theta,
                marginals,
            })
        })
        .collect()
}

/// Cholesky decomposition of a symmetric positive-definite matrix.
/// Returns the lower-triangular matrix L such that A = L·Lᵀ.
fn cholesky_decompose(matrix: &[Vec<f64>]) -> Option<Vec<Vec<f64>>> {
    let n = matrix.len();
    let mut l = vec![vec![0.0f64; n]; n];

    for i in 0..n {
        for j in 0..=i {
            let sum: f64 = l[i].iter().zip(l[j].iter()).take(j).map(|(a, b)| a * b).sum();
            if i == j {
                let diag = matrix[i][i] - sum;
                if diag < -1e-10 {
                    return None;
                }
                l[i][j] = diag.max(0.0).sqrt();
            } else if l[j][j].abs() < 1e-15 {
                return None;
            } else {
                l[i][j] = (matrix[i][j] - sum) / l[j][j];
            }
        }
    }
    Some(l)
}

/// Apply conditional distribution correlations by replacing the dependent
/// field's generator plan with a `Conditional` plan that branches on the
/// `given` field and samples from per-branch distributions.
fn apply_conditional_distribution_overrides(
    entity_name: &str,
    field_plans: &mut [FieldPlan],
    correlations: &[crate::core::Correlation],
) {
    for corr in correlations {
        let is_cond_dist = corr
            .correlation_type
            .as_deref()
            .map(|t| t == "conditional_distribution")
            .unwrap_or(false);
        if !is_cond_dist || corr.entity != entity_name {
            continue;
        }
        let dependent = match &corr.dependent {
            Some(d) => d,
            None => continue,
        };
        let given = match &corr.given {
            Some(g) => g,
            None => continue,
        };
        if corr.distributions.is_empty() {
            continue;
        }

        // Build branch plans from each distribution spec
        let branches: Vec<(Value, Box<GeneratorPlan>)> = corr
            .distributions
            .iter()
            .map(|b| {
                let plan = GeneratorPlan::Distribution {
                    kind: b.distribution.clone(),
                    params: b.params.clone(),
                    array_params: BTreeMap::new(),
                    clamp_min: None,
                    clamp_max: None,
                    round: b.round,
                };
                (b.condition.clone(), Box::new(plan))
            })
            .collect();

        // Build default plan: use explicit default, or replicate the first
        // branch's distribution to ensure type consistency (avoids Constant(Null)
        // which would cause Arrow type mismatch and string fallback).
        let default_plan = match &corr.default {
            Some(spec) => Box::new(GeneratorPlan::Distribution {
                kind: spec.kind.clone(),
                params: spec.params.clone(),
                array_params: spec.array_params.clone(),
                clamp_min: None,
                clamp_max: None,
                round: spec.round,
            }),
            None => {
                let first = &corr.distributions[0];
                Box::new(GeneratorPlan::Distribution {
                    kind: first.distribution.clone(),
                    params: first.params.clone(),
                    array_params: BTreeMap::new(),
                    clamp_min: None,
                    clamp_max: None,
                    round: first.round,
                })
            }
        };

        let conditional_plan = GeneratorPlan::Conditional {
            field: given.clone(),
            branches,
            default: default_plan,
        };

        // Find and override the dependent field's generator plan.
        // Also ensure the dependent field is generated after the given field
        // by bumping its dependency_order.
        let given_order = field_plans
            .iter()
            .find(|fp| fp.field_name == *given)
            .map(|fp| fp.dependency_order)
            .unwrap_or(0);
        if let Some(fp) = field_plans.iter_mut().find(|fp| fp.field_name == *dependent) {
            fp.generator_plan = conditional_plan;
            if fp.dependency_order <= given_order {
                fp.dependency_order = given_order + 1;
            }
        }

        // Re-sort by dependency order after override
        field_plans.sort_by_key(|fp| fp.dependency_order);
    }
}

/// Resolve an `IntOrString` start value to an i64.
///
/// - `Int(v)` → returns `v` directly
/// - `Str(s)` → parses as datetime ("2024-01-01T00:00:00") or date ("2024-01-01")
///   and returns epoch milliseconds
fn resolve_int_or_string_to_i64(v: &crate::core::IntOrString) -> i64 {
    match v {
        crate::core::IntOrString::Int(n) => *n,
        crate::core::IntOrString::Str(s) => parse_datetime_to_epoch_ms(s),
    }
}

/// Resolve an `IntOrString` step value to an i64.
///
/// - `Int(v)` → returns `v` directly
/// - `Str(s)` → parses as a duration string (e.g. "1d", "1h", "30m")
///   and returns milliseconds
fn resolve_step_to_i64(v: &crate::core::IntOrString) -> i64 {
    match v {
        crate::core::IntOrString::Int(n) => *n,
        crate::core::IntOrString::Str(s) => {
            crate::gen::generators::event_stream::parse_duration_ms(s)
        }
    }
}

/// Parse a date or datetime string to epoch milliseconds.
///
/// Supported formats:
/// - `"2024-01-01"` — date-only, interpreted as midnight UTC
/// - `"2024-01-01T00:00:00"` — naive datetime, interpreted as UTC
/// - `"2024-01-01T00:00:00Z"` — explicit UTC
/// - `"2024-01-01T00:00:00+05:00"` — offset-aware datetime
fn parse_datetime_to_epoch_ms(s: &str) -> i64 {
    use chrono::{NaiveDate, NaiveDateTime, DateTime};

    let s = s.trim();

    // Try RFC 3339 / offset-aware first: "2024-01-01T00:00:00Z" or "2024-01-01T00:00:00+05:00"
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return dt.timestamp_millis();
    }

    // Try naive datetime: "2024-01-01T00:00:00"
    if let Ok(ndt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
        return ndt.and_utc().timestamp_millis();
    }

    // Try naive datetime with fractional seconds: "2024-01-01T00:00:00.000"
    if let Ok(ndt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f") {
        return ndt.and_utc().timestamp_millis();
    }

    // Try date-only: "2024-01-01"
    if let Ok(nd) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let ndt = nd.and_hms_opt(0, 0, 0).expect("midnight is always valid");
        return ndt.and_utc().timestamp_millis();
    }

    // Fallback: try parsing as plain integer
    s.parse::<i64>().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::*;

    /// Helper to create a simple entity with an id field and given count.
    fn simple_entity(name: &str, count: u64) -> Entity {
        Entity {
            name: name.to_string(),
            description: None,
            tags: Vec::new(),
            count: CountSpec::Fixed(count),
            fields: vec![
                Field {
                    name: "id".to_string(),
                    description: None,
                    data_type: DataType::Int,
                    generator: Some(GeneratorSpec::Sequence {
                        start: IntOrString::Int(1),
                        step: IntOrString::Int(1),
                        prefix: None,
                    values: None,
                    cycle: None,
                    jitter: None,
                    }),
                    nullable: NullSpec::Never,
                    primary_key: Some(true),
                    precision: None,
                    actor_column: false,
                    fields: vec![],
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
                    fields: vec![],
                },
            ],
            constraints: vec![],
            topology: None,
            actor: false,
            persona_distribution: None,
            activity_count: None,
                mixin_refs: None,
        output: None,
        }
    }

    fn simple_model(
        name: &str,
        entities: Vec<Entity>,
        relationships: Vec<Relationship>,
    ) -> DataModel {
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
            custom_types: Vec::new(),
            mixins: Vec::new(),
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
                degree: None,

                selection: None,
                nullable: None,
                acyclic: None,
                root_probability: None,
                max_depth: None,
                properties: vec![],
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
                    degree: None,

                    selection: None,
                    nullable: None,
                    acyclic: None,
                    root_probability: None,
                    max_depth: None,
                    properties: vec![],
                },
                Relationship {
                    name: "line_item_order".to_string(),
                    from: "line_item".to_string(),
                    to: "order".to_string(),
                    kind: RelationshipKind::ManyToOne,
                    foreign_key: Some("order_id".to_string()),
                    cardinality: None,
                    degree: None,

                    selection: None,
                    nullable: None,
                    acyclic: None,
                    root_probability: None,
                    max_depth: None,
                    properties: vec![],
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
            vec![simple_entity("user", 1000), simple_entity("product", 500)],
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
            fields: vec![],
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
                degree: None,

                selection: None,
                nullable: None,
                acyclic: None,
                root_probability: None,
                max_depth: None,
                properties: vec![],
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
            fields: vec![],
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
            fields: vec![],
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
                    degree: None,

                    selection: None,
                    nullable: None,
                    acyclic: None,
                    root_probability: None,
                    max_depth: None,
                    properties: vec![],
                },
                Relationship {
                    name: "b_to_a".to_string(),
                    from: "b".to_string(),
                    to: "a".to_string(),
                    kind: RelationshipKind::ManyToOne,
                    foreign_key: Some("a_id".to_string()),
                    cardinality: None,
                    degree: None,

                    selection: None,
                    nullable: None,
                    acyclic: None,
                    root_probability: None,
                    max_depth: None,
                    properties: vec![],
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
            KeyStoreKind::SampledSubset {
                sample_size: 10_000_000
            }
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
                    array_params: BTreeMap::new(),
                    round: false,
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
        });

        let model = simple_model("fields", vec![entity], vec![]);
        let plan = compile(&model).unwrap();
        let ep = &plan.phases[0].entity_plans[0];
        let score_plan = ep
            .field_plans
            .iter()
            .find(|fp| fp.field_name == "score")
            .unwrap();
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
                    array_params: BTreeMap::new(),
                    round: false,
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
        });

        let model = simple_model("autoround", vec![entity], vec![]);
        let plan = compile(&model).unwrap();
        let ep = &plan.phases[0].entity_plans[0];
        let age_plan = ep
            .field_plans
            .iter()
            .find(|fp| fp.field_name == "age")
            .unwrap();
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
                    array_params: BTreeMap::new(),
                    round: false,
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
        });

        let model = simple_model("noround", vec![entity], vec![]);
        let plan = compile(&model).unwrap();
        let ep = &plan.phases[0].entity_plans[0];
        let score_plan = ep
            .field_plans
            .iter()
            .find(|fp| fp.field_name == "score")
            .unwrap();
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
        let id_plan = ep
            .field_plans
            .iter()
            .find(|fp| fp.field_name == "id")
            .unwrap();
        assert!(matches!(
            id_plan.generator_plan,
            GeneratorPlan::Sequence { start: 1, step: 1, jitter_ms: None }
        ));
    }

    #[test]
    fn test_field_plan_faker() {
        let model = simple_model("fields", vec![simple_entity("test", 100)], vec![]);
        let plan = compile(&model).unwrap();
        let ep = &plan.phases[0].entity_plans[0];
        let name_plan = ep
            .field_plans
            .iter()
            .find(|fp| fp.field_name == "name")
            .unwrap();
        assert!(matches!(
            name_plan.generator_plan,
            GeneratorPlan::Faker { .. }
        ));
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
            fields: vec![],
        });

        let model = simple_model("fields", vec![entity], vec![]);
        let plan = compile(&model).unwrap();
        let ep = &plan.phases[0].entity_plans[0];
        let status_plan = ep
            .field_plans
            .iter()
            .find(|fp| fp.field_name == "status")
            .unwrap();
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
            fields: vec![],
        });

        let model = simple_model("fields", vec![entity], vec![]);
        let plan = compile(&model).unwrap();
        let ep = &plan.phases[0].entity_plans[0];
        let uuid_plan = ep
            .field_plans
            .iter()
            .find(|fp| fp.field_name == "uuid_field")
            .unwrap();
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
            fields: vec![],
        });

        let model = simple_model("fields", vec![entity], vec![]);
        let plan = compile(&model).unwrap();
        let ep = &plan.phases[0].entity_plans[0];
        let version_plan = ep
            .field_plans
            .iter()
            .find(|fp| fp.field_name == "version")
            .unwrap();
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
                    array_params: BTreeMap::new(),
                    round: false,
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
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
            fields: vec![],
        });

        let model = simple_model("fields", vec![entity], vec![]);
        let plan = compile(&model).unwrap();
        let ep = &plan.phases[0].entity_plans[0];
        let tax_plan = ep
            .field_plans
            .iter()
            .find(|fp| fp.field_name == "tax")
            .unwrap();
        match &tax_plan.generator_plan {
            GeneratorPlan::Derived { expr, depends_on } => {
                assert_eq!(expr, "price * 0.1");
                assert!(depends_on.contains(&"price".to_string()));
            }
            other => panic!("expected Derived, got {other:?}"),
        }
        // Derived field should have higher dependency_order.
        let price_plan = ep
            .field_plans
            .iter()
            .find(|fp| fp.field_name == "price")
            .unwrap();
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
                value: crate::core::Value::String("prod".into()),
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
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
            fields: vec![],
        });

        let model = simple_model("params", vec![entity], vec![]);
        let plan = compile(&model).unwrap();
        let ep = &plan.phases[0].entity_plans[0];
        let label_plan = ep
            .field_plans
            .iter()
            .find(|fp| fp.field_name == "label")
            .unwrap();
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
        assert!(matches!(
            compile_null_plan(&NullSpec::Never),
            NullPlan::Never
        ));
        assert!(matches!(
            compile_null_plan(&NullSpec::Always),
            NullPlan::Always
        ));
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
        let model = simple_model("display_test", vec![simple_entity("users", 1000)], vec![]);
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
                degree: None,

                selection: None,
                nullable: None,
                acyclic: None,
                root_probability: None,
                max_depth: None,
                properties: vec![],
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
            fields: vec![],
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
                degree: None,

                selection: None,
                nullable: None,
                acyclic: None,
                root_probability: None,
                max_depth: None,
                properties: vec![],
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
                tags: Vec::new(),
                count: CountSpec::Fixed(10),
                fields: vec![Field {
                    name: "color".to_string(),
                    description: None,
                    data_type: DataType::String,
                    generator: Some(GeneratorSpec::OneOf {
                        choices: vec![
                            WeightedChoice {
                                value: Value::String("red".into()),
                                weight: 0.0,
                            },
                            WeightedChoice {
                                value: Value::String("blue".into()),
                                weight: 0.0,
                            },
                        ],
                    }),
                    nullable: NullSpec::Never,
                    primary_key: None,
                    precision: None,
                    actor_column: false,
                    fields: vec![],
                }],
                constraints: vec![],
                topology: None,
                actor: false,
                persona_distribution: None,
                activity_count: None,
                mixin_refs: None,
        output: None,
            }],
            vec![],
        );
        let plan = compile(&model).unwrap();
        let ep = &plan.phases[0].entity_plans[0];
        let fp = ep
            .field_plans
            .iter()
            .find(|f| f.field_name == "color")
            .unwrap();
        match &fp.generator_plan {
            GeneratorPlan::OneOf {
                cumulative_weights, ..
            } => {
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
                fields: vec![],
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
                fields: vec![],
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
            start: IntOrString::Int(1),
            step: IntOrString::Int(1),
            prefix: None,
        values: None,
        cycle: None,
        jitter: None,
        };
        let spec = GeneratorSpec::Unique {
            inner: Box::new(inner_spec),
            max_retries: 50,
        };
        let plan = compile_generator_from_spec(&spec, &[], &DataType::String);
        match plan {
            GeneratorPlan::Unique { inner, max_retries } => {
                assert_eq!(max_retries, 50);
                assert!(matches!(
                    *inner,
                    GeneratorPlan::Sequence { start: 1, step: 1, jitter_ms: None }
                ));
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
            timezone: None,
            timezone_field: None,
            date_range: None,
            exclude_dates: vec![],
            days: None,
        };
        let plan = compile_generator_from_spec(&spec, &[], &DataType::String);
        match plan {
            GeneratorPlan::Temporal {
                kind: TemporalKind::BusinessHours,
                params,
                base_field,
                ..
            } => {
                assert_eq!(params["start_hour"], 9.0);
                assert_eq!(params["end_hour"], 17.0);
                assert_eq!(params["days_mask"], 0x1F as f64); // Mon-Fri
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
            timezone: None,
            timezone_field: None,
            date_range: None,
            exclude_dates: vec![],
            days: None,
        };
        let plan = compile_generator_from_spec(&spec, &[], &DataType::String);
        match plan {
            GeneratorPlan::Temporal {
                kind: TemporalKind::BusinessHours,
                params,
                ..
            } => {
                assert_eq!(params["days_mask"], 0x7F as f64); // All days
            }
            other => panic!("expected Temporal/BusinessHours, got {other:?}"),
        }
    }

    #[test]
    fn test_relative_compiles_to_temporal() {
        let spec = GeneratorSpec::Relative {
            anchor: "start_date".to_string(),
            offset: RelativeOffset::Simple(Value::Int(86400)),
        };
        let plan = compile_generator_from_spec(&spec, &[], &DataType::String);
        match plan {
            GeneratorPlan::Temporal {
                kind: TemporalKind::Relative,
                params,
                base_field,
                ..
            } => {
                assert_eq!(params["offset_mean"], 86400.0);
                assert!(params["offset_std"] > 0.0);
                assert_eq!(base_field, Some("start_date".to_string()));
            }
            other => panic!("expected Temporal/Relative, got {other:?}"),
        }
    }

    #[test]
    fn test_relative_distribution_compiles() {
        let spec = GeneratorSpec::Relative {
            anchor: "order_date".to_string(),
            offset: RelativeOffset::Distribution {
                distribution: crate::core::DistributionKind::LogNormal,
                params: {
                    let mut p = BTreeMap::new();
                    p.insert("mu".into(), 1.5);
                    p.insert("sigma".into(), 0.8);
                    p
                },
                min: Some("1d".into()),
                max: Some("14d".into()),
                unit: Some("day".into()),
            },
        };
        let plan = compile_generator_from_spec(&spec, &[], &DataType::Datetime);
        match plan {
            GeneratorPlan::Temporal {
                kind: TemporalKind::Relative,
                params,
                base_field,
                string_params,
            } => {
                assert_eq!(base_field, Some("order_date".to_string()));
                assert_eq!(params["offset_mode"], 1.0);
                assert_eq!(params["mu"], 1.5);
                assert_eq!(params["sigma"], 0.8);
                assert_eq!(params["min_ms"], 86_400_000.0); // 1 day
                assert_eq!(params["max_ms"], 86_400_000.0 * 14.0); // 14 days
                assert_eq!(params["unit"], 3.0); // days
                assert!(string_params.contains_key("distribution"));
            }
            other => panic!("expected Temporal/Relative, got {other:?}"),
        }
    }

    #[test]
    fn test_relative_constant_compiles() {
        let spec = GeneratorSpec::Relative {
            anchor: "issue_date".to_string(),
            offset: RelativeOffset::Constant {
                offset_type: "constant".into(),
                value: "365d".into(),
            },
        };
        let plan = compile_generator_from_spec(&spec, &[], &DataType::Datetime);
        match plan {
            GeneratorPlan::Temporal {
                kind: TemporalKind::Relative,
                params,
                base_field,
                ..
            } => {
                assert_eq!(base_field, Some("issue_date".to_string()));
                assert_eq!(params["offset_mode"], 2.0);
                // 365 days in ms
                let expected_ms = 365.0 * 86_400_000.0;
                assert_eq!(params["constant_ms"], expected_ms);
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
                    anchor: "start_date".to_string(),
                    offset: RelativeOffset::Simple(Value::Int(3600)),
                }),
                primary_key: None,
                precision: None,
                actor_column: false,
                fields: vec![],
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
                        array_params: BTreeMap::new(),
                        round: false,
                    },
                }),
                primary_key: None,
                precision: None,
                actor_column: false,
                fields: vec![],
            },
        ];
        let order_end = compute_dependency_order(&fields[0], &fields);
        let order_start = compute_dependency_order(&fields[1], &fields);
        assert!(
            order_end > order_start,
            "relative field should come after base field"
        );
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

    #[test]
    fn activity_count_overrides_row_count() {
        // Set up: actor entity "users" with 100 actors, two personas,
        // and a "posts" entity with activity_count referencing users.
        let mut users = simple_entity("users", 100);
        users.actor = true;
        users.persona_distribution = Some("personas".into());

        let mut posts = simple_entity("posts", 9999); // static fallback
        posts.fields.push(Field {
            name: "author_id".to_string(),
            description: None,
            data_type: DataType::Int,
            generator: None,
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: true,
            fields: vec![],
        });
        posts.activity_count = Some(ActivityCount {
            actor_field: "author_id".to_string(),
            trait_name: "post_rate".to_string(),
        });

        let mut model = simple_model(
            "activity_test",
            vec![users, posts],
            vec![Relationship {
                name: "post_author".to_string(),
                from: "posts".to_string(),
                to: "users".to_string(),
                kind: RelationshipKind::ManyToOne,
                foreign_key: Some("author_id".to_string()),
                cardinality: None,
                degree: None,

                selection: None,
                nullable: None,
                acyclic: None,
                root_probability: None,
                max_depth: None,
                properties: vec![],
            }],
        );

        // Personas: heavy (weight 0.3, post_rate=20), light (weight 0.7, post_rate=2)
        model.personas.push(Persona {
            name: "users_heavy".to_string(),
            weight: 0.3,
            traits: BTreeMap::from([("post_rate".to_string(), Value::Float(20.0))]),
        });
        model.personas.push(Persona {
            name: "users_light".to_string(),
            weight: 0.7,
            traits: BTreeMap::from([("post_rate".to_string(), Value::Float(2.0))]),
        });

        let plan = compile(&model).unwrap();

        // Expected: 0.3 * 100 * 20 + 0.7 * 100 * 2 = 600 + 140 = 740
        let posts_plan = plan
            .phases
            .iter()
            .flat_map(|p| &p.entity_plans)
            .find(|ep| ep.entity_name == "posts")
            .unwrap();
        let total: u64 = posts_plan
            .partitions
            .iter()
            .map(|p| p.end_row - p.start_row)
            .sum();
        assert_eq!(
            total, 740,
            "dynamic count should be 740, not the static 9999"
        );
    }

    #[test]
    fn activity_count_uses_fallback_when_no_pool() {
        // Entity with activity_count but no actor pool (no personas defined).
        let mut posts = simple_entity("posts", 500);
        posts.fields.push(Field {
            name: "author_id".to_string(),
            description: None,
            data_type: DataType::Int,
            generator: None,
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: true,
            fields: vec![],
        });
        posts.activity_count = Some(ActivityCount {
            actor_field: "author_id".to_string(),
            trait_name: "post_rate".to_string(),
        });

        let model = simple_model(
            "fallback_test",
            vec![simple_entity("users", 100), posts],
            vec![Relationship {
                name: "post_author".to_string(),
                from: "posts".to_string(),
                to: "users".to_string(),
                kind: RelationshipKind::ManyToOne,
                foreign_key: Some("author_id".to_string()),
                cardinality: None,
                degree: None,

                selection: None,
                nullable: None,
                acyclic: None,
                root_probability: None,
                max_depth: None,
                properties: vec![],
            }],
        );

        let plan = compile(&model).unwrap();

        // No personas → no pool → static count preserved.
        let posts_plan = plan
            .phases
            .iter()
            .flat_map(|p| &p.entity_plans)
            .find(|ep| ep.entity_name == "posts")
            .unwrap();
        let total: u64 = posts_plan
            .partitions
            .iter()
            .map(|p| p.end_row - p.start_row)
            .sum();
        assert_eq!(total, 500, "should use static fallback when no actor pool");
    }

    #[test]
    fn test_parse_datetime_to_epoch_ms_date_only() {
        // 2024-01-01 midnight UTC = 1704067200000 ms
        let ms = parse_datetime_to_epoch_ms("2024-01-01");
        assert_eq!(ms, 1_704_067_200_000);
    }

    #[test]
    fn test_parse_datetime_to_epoch_ms_naive_datetime() {
        // 2024-01-01T08:00:00 UTC = 1704067200000 + 8*3600*1000
        let ms = parse_datetime_to_epoch_ms("2024-01-01T08:00:00");
        assert_eq!(ms, 1_704_096_000_000);
    }

    #[test]
    fn test_parse_datetime_to_epoch_ms_rfc3339() {
        let ms = parse_datetime_to_epoch_ms("2024-01-01T00:00:00Z");
        assert_eq!(ms, 1_704_067_200_000);
    }

    #[test]
    fn test_parse_datetime_to_epoch_ms_with_offset() {
        // 2024-01-01T00:00:00-05:00 = 2024-01-01T05:00:00Z
        let ms = parse_datetime_to_epoch_ms("2024-01-01T00:00:00-05:00");
        assert_eq!(ms, 1_704_067_200_000 + 5 * 3_600_000);
    }

    #[test]
    fn test_resolve_temporal_sequence_values() {
        // Test resolve_int_or_string_to_i64 with temporal strings
        let start = IntOrString::Str("2024-01-01".into());
        let start_ms = resolve_int_or_string_to_i64(&start);
        assert_eq!(start_ms, 1_704_067_200_000); // 2024-01-01 epoch ms

        // Test resolve_step_to_i64 with duration string
        let step = IntOrString::Str("1d".into());
        let step_ms = resolve_step_to_i64(&step);
        assert_eq!(step_ms, 86_400_000); // 1 day in ms

        // Test integer passthrough
        let start_int = IntOrString::Int(42);
        assert_eq!(resolve_int_or_string_to_i64(&start_int), 42);

        let step_int = IntOrString::Int(100);
        assert_eq!(resolve_step_to_i64(&step_int), 100);

        // Test datetime string
        let dt = IntOrString::Str("2024-01-01T08:00:00".into());
        assert_eq!(resolve_int_or_string_to_i64(&dt), 1_704_096_000_000);

        // Test hour duration
        let h = IntOrString::Str("1h".into());
        assert_eq!(resolve_step_to_i64(&h), 3_600_000);
    }
}
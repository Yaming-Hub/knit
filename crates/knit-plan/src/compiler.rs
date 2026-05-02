//! Main compilation logic: DataModel → ExecutionPlan.

use std::collections::{BTreeMap, HashMap};

use knit_core::{DataModel, Entity, Field, GeneratorSpec, NullSpec};

use crate::error::PlanError;
use crate::graph;
use crate::partition;
use crate::rng_tree;
use crate::types::*;

/// Compile a validated `DataModel` into an `ExecutionPlan`.
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
            let field_plans = compile_field_plans(entity, &entity_fks, &row_counts, &index_strategy);

            let estimated_byte_size = estimate_byte_size(entity, row_count);

            let field_names: Vec<String> =
                entity.fields.iter().map(|f| f.name.clone()).collect();
            rng_entities.push((entity_name.clone(), field_names, num_partitions));

            total_partitions += partitions.len();
            estimated_total_rows += row_count;
            estimated_total_bytes += estimated_byte_size;

            entity_plans.push(EntityPlan {
                entity_name: entity_name.clone(),
                partitions,
                field_plans,
                estimated_row_count: row_count,
                estimated_byte_size,
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
    };

    Ok(ExecutionPlan {
        phases,
        rng_tree,
        index_strategy,
        metadata,
    })
}

/// Compile field plans for an entity.
fn compile_field_plans(
    entity: &Entity,
    entity_fks: &[(String, String)],
    row_counts: &BTreeMap<String, u64>,
    index_strategy: &IndexStrategy,
) -> Vec<FieldPlan> {
    let fk_map: HashMap<&str, &str> = entity_fks
        .iter()
        .map(|(target, fk_field)| (fk_field.as_str(), target.as_str()))
        .collect();

    let mut plans: Vec<FieldPlan> = Vec::new();

    for field in &entity.fields {
        // Check if this field is a foreign key.
        let generator_plan = if let Some(&target_entity) = fk_map.get(field.name.as_str()) {
            let target_rows = row_counts.get(target_entity).copied().unwrap_or(1000);
            let key_store_kind = select_key_store_kind(target_rows);
            GeneratorPlan::ForeignKey {
                target_entity: target_entity.to_string(),
                target_field: "id".to_string(),
                key_store_kind,
            }
        } else {
            compile_generator(field, &entity.fields)
        };

        let null_plan = compile_null_plan(&field.nullable);
        let dependency_order = compute_dependency_order(field, &entity.fields);

        plans.push(FieldPlan {
            field_name: field.name.clone(),
            generator_plan,
            null_plan,
            dependency_order,
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
            GeneratorSpec::Distribution { spec: dist_spec } => GeneratorPlan::Distribution {
                kind: dist_spec.kind.clone(),
                params: dist_spec.params.clone(),
                clamp_min: None,
                clamp_max: None,
            },
            GeneratorSpec::Faker { method, args: _ } => GeneratorPlan::Faker {
                category: method.clone(),
                locale: "en_US".to_string(),
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
                let mut cumulative = Vec::with_capacity(choices.len());
                let mut running = 0.0;
                for choice in choices {
                    running += choice.weight / total_weight;
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
                    let element = Box::new(compile_generator_from_spec(first_gen, all_fields));
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
            GeneratorSpec::Pattern { pattern: _ }
            | GeneratorSpec::Conditional { .. }
            | GeneratorSpec::Unique { .. }
            | GeneratorSpec::Relative { .. }
            | GeneratorSpec::BusinessHours { .. } => {
                // Fallback: treat as a constant placeholder.
                GeneratorPlan::Constant(knit_core::Value::Null)
            }
        },
        None => {
            // No generator specified. Check if primary_key with UUID type.
            if field.primary_key.unwrap_or(false)
                && field.data_type == knit_core::DataType::Uuid
            {
                GeneratorPlan::Uuid
            } else if field.primary_key.unwrap_or(false) {
                GeneratorPlan::Sequence { start: 1, step: 1 }
            } else {
                GeneratorPlan::Constant(knit_core::Value::Null)
            }
        }
    }
}

/// Convert a `GeneratorSpec` directly (for nested generators).
fn compile_generator_from_spec(spec: &GeneratorSpec, all_fields: &[Field]) -> GeneratorPlan {
    // Create a dummy field to reuse compile_generator.
    let dummy_field = Field {
        name: String::new(),
        description: None,
        data_type: knit_core::DataType::String,
        generator: Some(spec.clone()),
        nullable: NullSpec::Never,
        primary_key: None,
    };
    compile_generator(&dummy_field, all_fields)
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
        _ => 0,
    }
}

/// Extract field names referenced in a derived expression.
/// Simple heuristic: look for field names from `all_fields` that appear in the expression.
fn extract_dependencies(expr: &str, all_fields: &[Field]) -> Vec<String> {
    all_fields
        .iter()
        .filter(|f| expr.contains(&f.name))
        .map(|f| f.name.clone())
        .collect()
}

/// Estimate byte size for an entity based on field types and row count.
fn estimate_byte_size(entity: &Entity, row_count: u64) -> u64 {
    let bytes_per_row: u64 = entity
        .fields
        .iter()
        .map(|f| match f.data_type {
            knit_core::DataType::Bool => 1,
            knit_core::DataType::Int => 8,
            knit_core::DataType::Float => 8,
            knit_core::DataType::String => 64,
            knit_core::DataType::Uuid => 16,
            knit_core::DataType::Date => 4,
            knit_core::DataType::Time => 8,
            knit_core::DataType::Datetime | knit_core::DataType::Datetimetz => 8,
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
                },
            ],
            constraints: vec![],
            topology: None,
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
        });

        let mut entity_b = simple_entity("b", 1000);
        entity_b.fields.push(Field {
            name: "a_id".to_string(),
            description: None,
            data_type: DataType::Int,
            generator: None,
            nullable: NullSpec::Never,
            primary_key: None,
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
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
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
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
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
}

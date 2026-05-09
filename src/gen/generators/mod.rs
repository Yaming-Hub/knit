//! Concrete [`FieldGenerator`] implementations and factory.
//!
//! Each sub-module provides a generator for one category of synthetic values.
//! The [`create_generator`] factory maps a [`GeneratorPlan`] variant to the
//! appropriate concrete type, serving as the single construction point used by
//! the batch-generation loop.

pub mod actor_fk;
pub mod actor_temporal;
pub mod composite;
pub mod conditional;
pub mod constant;
pub mod copula;
pub mod correlation;
pub mod derived;
pub mod dictionary;
pub mod distribution;
pub mod external_lookup;
pub mod faker;
pub mod fk;
pub mod graph_fk;
pub mod one_of;
pub mod pattern;
pub mod persona_field;
pub mod sequence;
pub mod string_fk;
pub mod struct_gen;
pub mod temporal;
pub mod thread_ref;
pub mod topology;
pub mod unique;
pub mod uuid_gen;

use crate::plan::GeneratorPlan;

use crate::gen::traits::FieldGenerator;

/// Shared seen-set for cross-partition uniqueness enforcement.
///
/// When present, `Unique` generators use the shared set instead of creating
/// a private one so that uniqueness is enforced globally across partitions.
pub type SharedSeen = std::sync::Arc<parking_lot::Mutex<std::collections::HashSet<String>>>;

/// Create a boxed [`FieldGenerator`] from a compiled [`GeneratorPlan`].
///
/// This is the factory function invoked once per field during engine
/// initialisation. The `ForeignKey` variant is normally handled directly by
/// the engine (which owns the key-store). If a nested generator (e.g.
/// `Unique` or `Conditional`) wraps a `ForeignKey`, this factory falls back
/// to a null-constant generator with a warning, since there is no key-store
/// available at the factory level.
pub fn create_generator(plan: &GeneratorPlan) -> Box<dyn FieldGenerator> {
    create_generator_with_seen(plan, None)
}

/// Like [`create_generator`], but threads a shared seen-set through to any
/// `Unique` sub-generators so uniqueness spans all partitions.
pub fn create_generator_with_seen(
    plan: &GeneratorPlan,
    shared_seen: Option<&SharedSeen>,
) -> Box<dyn FieldGenerator> {
    match plan {
        GeneratorPlan::Distribution {
            kind,
            params,
            clamp_min,
            clamp_max,
            round,
        } => Box::new(distribution::DistributionGenerator::new(
            kind.clone(),
            params.clone(),
            *clamp_min,
            *clamp_max,
            *round,
        )),
        GeneratorPlan::Sequence { start, step } => {
            Box::new(sequence::SequenceGenerator::new(*start, *step))
        }
        GeneratorPlan::Constant(value) => Box::new(constant::ConstantGenerator::new(value.clone())),
        GeneratorPlan::Uuid => Box::new(uuid_gen::UuidGenerator),
        GeneratorPlan::OneOf {
            choices,
            cumulative_weights: _,
        } => Box::new(one_of::OneOfGenerator::new(choices.clone())),
        GeneratorPlan::Pattern { pattern } => {
            Box::new(pattern::PatternGenerator::new(pattern.clone()))
        }
        GeneratorPlan::Derived { expr, depends_on } => Box::new(derived::DerivedGenerator::new(
            expr.clone(),
            depends_on.clone(),
        )),
        GeneratorPlan::Composite { element, length } => {
            Box::new(composite::CompositeGenerator::new_with_seen(
                element,
                length,
                shared_seen,
            ))
        }
        GeneratorPlan::Faker {
            category,
            locale,
            args,
        } => Box::new(faker::FakerGenerator::new(
            category.clone(),
            locale.clone(),
            args.clone(),
        )),
        // FK generators are created directly by the engine (which has access
        // to the key-store). If we reach here it means an FK was nested inside
        // another generator (e.g. Unique or Conditional) — fall back to null
        // with a warning since we lack key-store context.
        GeneratorPlan::ForeignKey { .. } => {
            tracing::warn!(
                "ForeignKey inside nested generator: no key-store available, emitting nulls"
            );
            Box::new(constant::ConstantGenerator::new(crate::core::Value::Null))
        }
        GeneratorPlan::Temporal {
            kind,
            params,
            base_field,
        } => match kind {
            crate::plan::TemporalKind::Relative => {
                let base = base_field.clone().unwrap_or_default();
                Box::new(temporal::RelativeGenerator::new(base, params))
            }
            crate::plan::TemporalKind::TimeSeries => {
                Box::new(temporal::TimeSeriesGenerator::new(params))
            }
            crate::plan::TemporalKind::BusinessHours => {
                Box::new(temporal::BusinessHoursGenerator::new(params))
            }
        },
        GeneratorPlan::Correlated {
            target_field,
            correlation,
        } => Box::new(correlation::CorrelatedGenerator::new(
            target_field.clone(),
            *correlation,
        )),
        GeneratorPlan::Unique { inner, max_retries } => {
            let inner_gen = create_generator_with_seen(inner, shared_seen);
            if let Some(seen) = shared_seen {
                Box::new(unique::UniqueGenerator::with_shared_seen(
                    inner_gen,
                    *max_retries,
                    std::sync::Arc::clone(seen),
                ))
            } else {
                Box::new(unique::UniqueGenerator::new(inner_gen, *max_retries))
            }
        }
        GeneratorPlan::Topology { model, params } => match model {
            crate::plan::TopologyModel::BarabasiAlbert => {
                Box::new(topology::BarabasiAlbertGenerator::new(params))
            }
            crate::plan::TopologyModel::Tree => Box::new(topology::TreeGenerator::new(params)),
            crate::plan::TopologyModel::WattsStrogatz => {
                Box::new(topology::WattsStrogatzGenerator::new(params))
            }
            crate::plan::TopologyModel::ErdosRenyi => {
                Box::new(topology::ErdosRenyiGenerator::new(params))
            }
            crate::plan::TopologyModel::StochasticBlock => {
                Box::new(topology::StochasticBlockGenerator::new(params))
            }
            crate::plan::TopologyModel::Configuration => {
                Box::new(topology::ConfigurationGenerator::new(params))
            }
            crate::plan::TopologyModel::Complete => {
                Box::new(topology::CompleteGenerator::new(params))
            }
        },
        GeneratorPlan::Conditional {
            field,
            branches,
            default,
        } => Box::new(conditional::ConditionalGenerator::new_with_seen(
            field.clone(),
            branches
                .iter()
                .map(|(v, p)| (v.clone(), (**p).clone()))
                .collect(),
            (**default).clone(),
            shared_seen,
        )),
        GeneratorPlan::Dictionary {
            entries, expansion, ..
        } => Box::new(dictionary::DictionaryGenerator::new(
            entries.clone(),
            expansion.clone(),
        )),
        // GraphTarget generators are created by the engine (which has graphs +
        // key stores). If nested, fall back to null.
        GeneratorPlan::GraphTarget { .. } => {
            tracing::warn!(
                "GraphTarget inside nested generator: no graph/key-store available, emitting nulls"
            );
            Box::new(constant::ConstantGenerator::new(crate::core::Value::Null))
        }
        // PersonaField/ActorTemporal are created by the engine (which has the
        // actor pool + PK reverse maps). If nested, fall back to null.
        GeneratorPlan::PersonaField { .. } => {
            tracing::warn!(
                "PersonaField inside nested generator: no actor pool available, emitting nulls"
            );
            Box::new(constant::ConstantGenerator::new(crate::core::Value::Null))
        }
        GeneratorPlan::ActorTemporal { .. } => {
            tracing::warn!(
                "ActorTemporal inside nested generator: no actor pool available, emitting nulls"
            );
            Box::new(constant::ConstantGenerator::new(crate::core::Value::Null))
        }
        GeneratorPlan::ThreadRef {
            reply_probability,
            max_depth,
            reply_window,
            pk_field,
        } => Box::new(thread_ref::ThreadRefGenerator::new(
            *reply_probability,
            *max_depth,
            *reply_window,
            pk_field.clone(),
        )),
        GeneratorPlan::Plugin { name, params } => {
            match crate::gen::plugin::registry().create(name, params) {
                Some(Ok(gen)) => gen,
                Some(Err(e)) => {
                    tracing::error!(plugin = %name, error = %e, "plugin creation failed — using null constant");
                    Box::new(crate::gen::generators::constant::ConstantGenerator::new(
                        crate::core::Value::Null,
                    ))
                }
                None => {
                    tracing::error!(
                        plugin = %name,
                        "plugin not found in registry — using null constant; register it before generation"
                    );
                    Box::new(crate::gen::generators::constant::ConstantGenerator::new(
                        crate::core::Value::Null,
                    ))
                }
            }
        }
        GeneratorPlan::ExternalLookup {
            entries,
            weights,
            sampling,
            ..
        } => Box::new(external_lookup::ExternalLookupGenerator::new(
            entries.clone(),
            weights.clone(),
            sampling.clone(),
        )),
        GeneratorPlan::Struct => {
            // StructGenerator is created by the engine which builds child generators
            // from sub_field_plans. If we reach here (e.g. nested in Conditional),
            // fall back to null.
            tracing::warn!(
                "Struct inside nested generator: no sub-field plans available, emitting nulls"
            );
            Box::new(constant::ConstantGenerator::new(crate::core::Value::Null))
        }
    }
}

/// Recursively check whether a [`GeneratorPlan`] contains a `Unique` node
/// at any nesting depth (e.g. inside `Conditional` or `Composite`).
pub fn plan_contains_unique(plan: &GeneratorPlan) -> bool {
    match plan {
        GeneratorPlan::Unique { .. } => true,
        GeneratorPlan::Conditional {
            branches, default, ..
        } => {
            branches.iter().any(|(_, p)| plan_contains_unique(p))
                || plan_contains_unique(default)
        }
        GeneratorPlan::Composite { element, length } => {
            plan_contains_unique(element) || plan_contains_unique(length)
        }
        _ => false,
    }
}

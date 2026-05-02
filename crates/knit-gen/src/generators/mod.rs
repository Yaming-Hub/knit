//! Concrete [`FieldGenerator`](crate::FieldGenerator) implementations and factory.
//!
//! Each sub-module provides a generator for one category of synthetic values.
//! The [`create_generator`] factory maps a [`GeneratorPlan`] variant to the
//! appropriate concrete type, serving as the single construction point used by
//! the batch-generation loop.

pub mod composite;
pub mod constant;
pub mod correlation;
pub mod derived;
pub mod distribution;
pub mod fk;
pub mod one_of;
pub mod pattern;
pub mod sequence;
pub mod temporal;
pub mod topology;
pub mod uuid_gen;

use knit_plan::GeneratorPlan;

use crate::traits::FieldGenerator;

/// Create a boxed [`FieldGenerator`] from a compiled [`GeneratorPlan`].
///
/// This is the factory function invoked once per field during engine
/// initialisation. The `Faker` and `ForeignKey` variants are not yet
/// implemented and return placeholder null generators with a `tracing::warn`.
///
/// # Panics
///
/// Does not panic. Invalid distribution parameters are handled gracefully
/// with fallback defaults and warning logs.
pub fn create_generator(plan: &GeneratorPlan) -> Box<dyn FieldGenerator> {
    match plan {
        GeneratorPlan::Distribution {
            kind,
            params,
            clamp_min,
            clamp_max,
        } => Box::new(distribution::DistributionGenerator::new(
            kind.clone(),
            params.clone(),
            *clamp_min,
            *clamp_max,
        )),
        GeneratorPlan::Sequence { start, step } => {
            Box::new(sequence::SequenceGenerator::new(*start, *step))
        }
        GeneratorPlan::Constant(value) => {
            Box::new(constant::ConstantGenerator::new(value.clone()))
        }
        GeneratorPlan::Uuid => Box::new(uuid_gen::UuidGenerator),
        GeneratorPlan::OneOf {
            choices,
            cumulative_weights: _,
        } => Box::new(one_of::OneOfGenerator::new(choices.clone())),
        GeneratorPlan::Pattern { pattern } => {
            Box::new(pattern::PatternGenerator::new(pattern.clone()))
        }
        GeneratorPlan::Derived { expr, depends_on } => {
            Box::new(derived::DerivedGenerator::new(expr.clone(), depends_on.clone()))
        }
        GeneratorPlan::Composite { element, length } => {
            Box::new(composite::CompositeGenerator::new(element, length))
        }
        // Placeholders for future PRs.
        GeneratorPlan::Faker { .. } | GeneratorPlan::ForeignKey { .. } => {
            tracing::warn!(plan = %format!("{plan:?}"), "using placeholder null generator");
            Box::new(constant::ConstantGenerator::new(knit_core::Value::Null))
        }
        GeneratorPlan::Temporal {
            kind,
            params,
            base_field,
        } => match kind {
            knit_plan::TemporalKind::Relative => {
                let base = base_field.clone().unwrap_or_default();
                Box::new(temporal::RelativeGenerator::new(base, params))
            }
            knit_plan::TemporalKind::TimeSeries => {
                Box::new(temporal::TimeSeriesGenerator::new(params))
            }
            knit_plan::TemporalKind::BusinessHours => {
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
        GeneratorPlan::Topology { model, params } => match model {
            knit_plan::TopologyModel::BarabasiAlbert => {
                Box::new(topology::BarabasiAlbertGenerator::new(params))
            }
            knit_plan::TopologyModel::Tree => {
                Box::new(topology::TreeGenerator::new(params))
            }
            knit_plan::TopologyModel::WattsStrogatz | knit_plan::TopologyModel::ErdosRenyi => {
                tracing::warn!(model = ?model, "topology model not yet implemented, using placeholder");
                Box::new(constant::ConstantGenerator::new(knit_core::Value::Null))
            }
        },
    }
}

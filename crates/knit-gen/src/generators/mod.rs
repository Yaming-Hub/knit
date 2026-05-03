//! Concrete [`FieldGenerator`] implementations and factory.
//!
//! Each sub-module provides a generator for one category of synthetic values.
//! The [`create_generator`] factory maps a [`GeneratorPlan`] variant to the
//! appropriate concrete type, serving as the single construction point used by
//! the batch-generation loop.

pub mod composite;
pub mod conditional;
pub mod constant;
pub mod correlation;
pub mod derived;
pub mod distribution;
pub mod faker;
pub mod fk;
pub mod one_of;
pub mod pattern;
pub mod sequence;
pub mod temporal;
pub mod topology;
pub mod unique;
pub mod uuid_gen;

use knit_plan::GeneratorPlan;

use crate::traits::FieldGenerator;

/// Create a boxed [`FieldGenerator`] from a compiled [`GeneratorPlan`].
///
/// This is the factory function invoked once per field during engine
/// initialisation. The `ForeignKey` variant is handled directly by the
/// engine (which owns the key-store) and will panic if reached here.
///
/// # Panics
///
/// Panics if called with a `ForeignKey` variant — those are instantiated
/// by the engine, not by this factory.
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
        GeneratorPlan::Faker { category, locale } => {
            Box::new(faker::FakerGenerator::new(category.clone(), locale.clone()))
        }
        // FK generators are created directly by the engine (which has access
        // to the key-store). The factory is never called for this variant.
        GeneratorPlan::ForeignKey { .. } => {
            unreachable!("ForeignKey generators are instantiated by the engine, not the factory")
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
        GeneratorPlan::Unique { inner, max_retries } => {
            let inner_gen = create_generator(inner);
            Box::new(unique::UniqueGenerator::new(inner_gen, *max_retries))
        }
        GeneratorPlan::Topology { model, params } => match model {
            knit_plan::TopologyModel::BarabasiAlbert => {
                Box::new(topology::BarabasiAlbertGenerator::new(params))
            }
            knit_plan::TopologyModel::Tree => {
                Box::new(topology::TreeGenerator::new(params))
            }
            knit_plan::TopologyModel::WattsStrogatz => {
                Box::new(topology::WattsStrogatzGenerator::new(params))
            }
            knit_plan::TopologyModel::ErdosRenyi => {
                Box::new(topology::ErdosRenyiGenerator::new(params))
            }
        },
        GeneratorPlan::Conditional {
            field,
            branches,
            default,
        } => Box::new(conditional::ConditionalGenerator::new(
            field.clone(),
            branches.iter().map(|(v, p)| (v.clone(), (**p).clone())).collect(),
            (**default).clone(),
        )),
    }
}

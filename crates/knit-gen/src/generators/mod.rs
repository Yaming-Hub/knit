//! Concrete [`FieldGenerator`](crate::FieldGenerator) implementations and factory.

pub mod constant;
pub mod distribution;
pub mod sequence;
pub mod uuid_gen;

use knit_plan::GeneratorPlan;

use crate::traits::FieldGenerator;

/// Create a [`FieldGenerator`] from a [`GeneratorPlan`].
///
/// Variants not yet implemented (Faker, OneOf, Derived, Composite, ForeignKey)
/// return a placeholder that produces null arrays.
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
        // Placeholders for future PRs.
        GeneratorPlan::Faker { .. }
        | GeneratorPlan::OneOf { .. }
        | GeneratorPlan::Derived { .. }
        | GeneratorPlan::Composite { .. }
        | GeneratorPlan::ForeignKey { .. } => {
            tracing::warn!(plan = %format!("{plan:?}"), "using placeholder null generator");
            Box::new(constant::ConstantGenerator::new(knit_core::Value::Null))
        }
    }
}

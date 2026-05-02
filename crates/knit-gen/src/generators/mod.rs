//! Concrete [`FieldGenerator`](crate::FieldGenerator) implementations and factory.
//!
//! Each sub-module provides a generator for one category of synthetic values.
//! The [`create_generator`] factory maps a [`GeneratorPlan`] variant to the
//! appropriate concrete type, serving as the single construction point used by
//! the batch-generation loop.

pub mod constant;
pub mod distribution;
pub mod sequence;
pub mod uuid_gen;

use knit_plan::GeneratorPlan;

use crate::traits::FieldGenerator;

/// Create a boxed [`FieldGenerator`] from a compiled [`GeneratorPlan`].
///
/// This is the factory function invoked once per field during engine
/// initialisation. Variants not yet implemented (Faker, OneOf, Derived,
/// Composite, ForeignKey) return a placeholder that produces null arrays
/// and emits a `tracing::warn` event.
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

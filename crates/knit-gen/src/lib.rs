//! # knit-gen — Synthetic data generation engine
//!
//! This crate provides the core generation engine for knit. It consumes an
//! [`ExecutionPlan`](knit_plan::ExecutionPlan) produced by `knit-plan` and
//! materialises synthetic data as Arrow [`RecordBatch`](arrow::record_batch::RecordBatch)es.
//!
//! ## Key components
//!
//! - [`FieldGenerator`] — trait implemented by every column generator.
//! - [`GenContext`] — per-batch context passed to generators.
//! - [`KeyStore`] / [`InMemoryKeyStore`] — primary-key storage for FK sampling.
//! - [`apply_null_mask`] — applies [`NullPlan`](knit_plan::NullPlan) to generated arrays.
//! - [`assemble_batch`] — combines column arrays into a `RecordBatch`.
//! - [`create_generator`] — factory that maps a [`GeneratorPlan`](knit_plan::GeneratorPlan)
//!   to a concrete `FieldGenerator`.

pub mod batch;
pub mod context;
pub mod engine;
pub mod error;
pub mod generators;
pub mod keystore;
pub mod null_mask;
pub mod traits;

pub use batch::assemble_batch;
pub use context::GenContext;
pub use engine::GenerationEngine;
pub use error::GenError;
pub use generators::create_generator;
pub use generators::fk::ForeignKeyGenerator;
pub use keystore::InMemoryKeyStore;
pub use null_mask::apply_null_mask;
pub use traits::{FieldGenerator, KeyStore};

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::*;
    use arrow::datatypes::DataType;
    use knit_core::{DistributionKind, Value};
    use knit_plan::{GeneratorPlan, NullPlan};
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;
    use std::collections::{BTreeMap, HashMap};

    fn make_ctx() -> GenContext<'static> {
        // Leak a HashMap so we get a &'static reference for testing.
        let map: &'static HashMap<String, arrow::array::ArrayRef> =
            Box::leak(Box::new(HashMap::new()));
        GenContext {
            batch_columns: map,
            row_offset: 0,
            partition_index: 0,
            partition_count: 1,
            entity_name: "test",
        }
    }

    fn make_rng() -> ChaCha8Rng {
        ChaCha8Rng::seed_from_u64(42)
    }

    // ── Distribution tests ──────────────────────────────────────────

    #[test]
    fn normal_distribution_mean_stddev() {
        let mut params = BTreeMap::new();
        params.insert("mean".into(), 100.0);
        params.insert("std_dev".into(), 10.0);
        let plan = GeneratorPlan::Distribution {
            kind: DistributionKind::Normal,
            params,
            clamp_min: None,
            clamp_max: None,
        };
        let gen = create_generator(&plan);
        let ctx = make_ctx();
        let arr = gen.generate(&mut make_rng(), 100_000, &ctx);
        let f64_arr = arr.as_any().downcast_ref::<Float64Array>().unwrap();

        let sum: f64 = f64_arr.values().iter().sum();
        let mean = sum / 100_000.0;
        assert!(
            (mean - 100.0).abs() < 1.0,
            "mean {mean} not within ±1 of 100"
        );

        let var: f64 = f64_arr
            .values()
            .iter()
            .map(|v| (v - mean).powi(2))
            .sum::<f64>()
            / 100_000.0;
        let std_dev = var.sqrt();
        assert!(
            (std_dev - 10.0).abs() < 1.0,
            "std_dev {std_dev} not within ±1 of 10"
        );
    }

    #[test]
    fn uniform_bounds() {
        let mut params = BTreeMap::new();
        params.insert("min".into(), 5.0);
        params.insert("max".into(), 15.0);
        let plan = GeneratorPlan::Distribution {
            kind: DistributionKind::Uniform,
            params,
            clamp_min: None,
            clamp_max: None,
        };
        let gen = create_generator(&plan);
        let ctx = make_ctx();
        let arr = gen.generate(&mut make_rng(), 10_000, &ctx);
        let f64_arr = arr.as_any().downcast_ref::<Float64Array>().unwrap();

        for v in f64_arr.values().iter() {
            assert!(*v >= 5.0 && *v < 15.0, "value {v} out of [5, 15)");
        }
    }

    // ── Sequence tests ──────────────────────────────────────────────

    #[test]
    fn sequence_correctness() {
        let plan = GeneratorPlan::Sequence { start: 10, step: 3 };
        let gen = create_generator(&plan);
        let ctx = make_ctx();
        let arr = gen.generate(&mut make_rng(), 5, &ctx);
        let i64_arr = arr.as_any().downcast_ref::<Int64Array>().unwrap();

        let expected: Vec<i64> = vec![10, 13, 16, 19, 22];
        let actual: Vec<i64> = i64_arr.values().to_vec();
        assert_eq!(actual, expected);
    }

    #[test]
    fn sequence_with_offset() {
        let plan = GeneratorPlan::Sequence {
            start: 0,
            step: 1,
        };
        let gen = create_generator(&plan);
        let map: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(HashMap::new()));
        let ctx = GenContext {
            batch_columns: map,
            row_offset: 100,
            partition_index: 1,
            partition_count: 2,
            entity_name: "test",
        };
        let arr = gen.generate(&mut make_rng(), 5, &ctx);
        let i64_arr = arr.as_any().downcast_ref::<Int64Array>().unwrap();

        let expected: Vec<i64> = vec![100, 101, 102, 103, 104];
        let actual: Vec<i64> = i64_arr.values().to_vec();
        assert_eq!(actual, expected);
    }

    // ── Constant tests ──────────────────────────────────────────────

    #[test]
    fn constant_produces_identical_values() {
        let plan = GeneratorPlan::Constant(Value::Int(42));
        let gen = create_generator(&plan);
        let ctx = make_ctx();
        let arr = gen.generate(&mut make_rng(), 100, &ctx);
        let i64_arr = arr.as_any().downcast_ref::<Int64Array>().unwrap();

        for v in i64_arr.values().iter() {
            assert_eq!(*v, 42);
        }
    }

    // ── UUID tests ──────────────────────────────────────────────────

    #[test]
    fn uuid_format() {
        let plan = GeneratorPlan::Uuid;
        let gen = create_generator(&plan);
        let ctx = make_ctx();
        let arr = gen.generate(&mut make_rng(), 100, &ctx);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();

        for i in 0..str_arr.len() {
            let s = str_arr.value(i);
            let parsed = uuid::Uuid::parse_str(s);
            assert!(parsed.is_ok(), "invalid UUID: {s}");
            assert_eq!(
                parsed.unwrap().get_version_num(),
                4,
                "not UUID v4: {s}"
            );
        }
    }

    #[test]
    fn uuid_uniqueness() {
        let plan = GeneratorPlan::Uuid;
        let gen = create_generator(&plan);
        let ctx = make_ctx();
        let arr = gen.generate(&mut make_rng(), 10_000, &ctx);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();

        let mut set = std::collections::HashSet::new();
        for i in 0..str_arr.len() {
            assert!(set.insert(str_arr.value(i).to_string()), "duplicate UUID");
        }
    }

    // ── Determinism ─────────────────────────────────────────────────

    #[test]
    fn deterministic_output() {
        let plan = GeneratorPlan::Distribution {
            kind: DistributionKind::Normal,
            params: {
                let mut m = BTreeMap::new();
                m.insert("mean".into(), 0.0);
                m.insert("std_dev".into(), 1.0);
                m
            },
            clamp_min: None,
            clamp_max: None,
        };
        let gen = create_generator(&plan);
        let ctx = make_ctx();

        let a = gen.generate(&mut make_rng(), 1000, &ctx);
        let b = gen.generate(&mut make_rng(), 1000, &ctx);

        let a_f = a.as_any().downcast_ref::<Float64Array>().unwrap();
        let b_f = b.as_any().downcast_ref::<Float64Array>().unwrap();
        assert_eq!(a_f.values(), b_f.values());
    }

    // ── Null mask tests ─────────────────────────────────────────────

    #[test]
    fn null_mask_probability() {
        let plan = GeneratorPlan::Constant(Value::Int(1));
        let gen = create_generator(&plan);
        let ctx = make_ctx();
        let arr = gen.generate(&mut make_rng(), 10_000, &ctx);

        let null_plan = NullPlan::Probability(0.3);
        let masked = apply_null_mask(arr, &null_plan, &mut make_rng(), 10_000);

        let null_count = masked.null_count();
        let ratio = null_count as f64 / 10_000.0;
        assert!(
            (ratio - 0.3).abs() < 0.05,
            "null ratio {ratio} not ≈ 0.3"
        );
    }

    #[test]
    fn null_mask_pattern() {
        let plan = GeneratorPlan::Constant(Value::Int(1));
        let gen = create_generator(&plan);
        let ctx = make_ctx();
        let arr = gen.generate(&mut make_rng(), 20, &ctx);

        let null_plan = NullPlan::Pattern { every_n: 5 };
        let masked = apply_null_mask(arr, &null_plan, &mut make_rng(), 20);

        // Indices 0, 5, 10, 15 should be null.
        for i in 0..20 {
            if i % 5 == 0 {
                assert!(masked.is_null(i), "index {i} should be null");
            } else {
                assert!(masked.is_valid(i), "index {i} should be valid");
            }
        }
    }

    // ── Batch assembly ──────────────────────────────────────────────

    #[test]
    fn batch_assembly() {
        let names = vec!["id".to_string(), "value".to_string()];
        let id_arr: ArrayRef = std::sync::Arc::new(Int64Array::from(vec![1, 2, 3]));
        let val_arr: ArrayRef =
            std::sync::Arc::new(Float64Array::from(vec![1.1, 2.2, 3.3]));

        let batch = assemble_batch(&names, vec![id_arr, val_arr]).unwrap();
        assert_eq!(batch.num_columns(), 2);
        assert_eq!(batch.num_rows(), 3);
        assert_eq!(
            *batch.schema().field(0).data_type(),
            DataType::Int64
        );
        assert_eq!(
            *batch.schema().field(1).data_type(),
            DataType::Float64
        );
    }

    // ── KeyStore tests ──────────────────────────────────────────────

    #[test]
    fn keystore_insert_and_sample() {
        let store = InMemoryKeyStore::new();
        assert!(store.is_empty());

        for i in 0..1000 {
            store.insert(i);
        }
        assert_eq!(store.len(), 1000);

        let mut rng = make_rng();
        for _ in 0..100 {
            let key = store.sample(&mut rng).expect("store not empty");
            assert!((0..1000).contains(&key));
        }
    }

    // ── Clamping test ───────────────────────────────────────────────

    #[test]
    fn clamping_normal() {
        let mut params = BTreeMap::new();
        params.insert("mean".into(), 100.0);
        params.insert("std_dev".into(), 50.0);
        let plan = GeneratorPlan::Distribution {
            kind: DistributionKind::Normal,
            params,
            clamp_min: Some(0.0),
            clamp_max: Some(200.0),
        };
        let gen = create_generator(&plan);
        let ctx = make_ctx();
        let arr = gen.generate(&mut make_rng(), 100_000, &ctx);
        let f64_arr = arr.as_any().downcast_ref::<Float64Array>().unwrap();

        for v in f64_arr.values().iter() {
            assert!(
                *v >= 0.0 && *v <= 200.0,
                "clamped value {v} out of [0, 200]"
            );
        }
    }
}

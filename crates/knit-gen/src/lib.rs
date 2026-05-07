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

#![warn(missing_docs)]

pub mod actor_pool;
pub mod batch;
pub mod context;
pub mod graph;
pub mod interaction;
pub mod engine;
pub mod error;
pub mod generators;
pub mod keystore;
pub mod null_mask;
pub mod plugin;
pub mod sampled_key_store;
pub mod string_keystore;
pub mod temporal_store;
pub mod traits;

pub use actor_pool::ActorPool;
pub use batch::assemble_batch;
pub use graph::{generate_graph, GeneratedGraph, Edge};
pub use interaction::{generate_interactions, InteractionGenerator, InteractionConfig, InteractionRecord};
pub use context::GenContext;
pub use engine::GenerationEngine;
pub use error::GenError;
pub use generators::create_generator;
pub use generators::fk::ForeignKeyGenerator;
pub use generators::string_fk::StringForeignKeyGenerator;
pub use keystore::InMemoryKeyStore;
pub use null_mask::apply_null_mask;
pub use plugin::{registry, GeneratorPlugin, Registry};
pub use sampled_key_store::SampledKeyStore;
pub use string_keystore::InMemoryStringKeyStore;
pub use traits::{FieldGenerator, KeyStore, StringKeyStore};

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
        GenContext::new(map, 0, 0, 1, "test")
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
            round: false,
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
            round: false,
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
        let ctx = GenContext::new(map, 100, 1, 2, "test");
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
            round: false,
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
        let masked = apply_null_mask(arr, &null_plan, &mut make_rng(), 10_000).unwrap();

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
        let masked = apply_null_mask(arr, &null_plan, &mut make_rng(), 20).unwrap();

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
            round: false,
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

    // ── Integer distribution tests ──────────────────────────────────

    #[test]
    fn poisson_produces_int64() {
        let mut params = BTreeMap::new();
        params.insert("lambda".into(), 5.0);
        let plan = GeneratorPlan::Distribution {
            kind: DistributionKind::Poisson,
            params,
            clamp_min: None,
            clamp_max: None,
            round: false,
        };
        let gen = create_generator(&plan);
        let ctx = make_ctx();
        let arr = gen.generate(&mut make_rng(), 1000, &ctx);
        assert_eq!(*arr.data_type(), DataType::Int64);
        let i64_arr = arr.as_any().downcast_ref::<Int64Array>().unwrap();
        // Poisson(5) should have mean ≈ 5
        let sum: i64 = i64_arr.values().iter().sum();
        let mean = sum as f64 / 1000.0;
        assert!((mean - 5.0).abs() < 1.0, "poisson mean {mean} not ≈ 5");
    }

    #[test]
    fn bernoulli_produces_int64_binary() {
        let mut params = BTreeMap::new();
        params.insert("p".into(), 0.3);
        let plan = GeneratorPlan::Distribution {
            kind: DistributionKind::Bernoulli,
            params,
            clamp_min: None,
            clamp_max: None,
            round: false,
        };
        let gen = create_generator(&plan);
        let ctx = make_ctx();
        let arr = gen.generate(&mut make_rng(), 10_000, &ctx);
        assert_eq!(*arr.data_type(), DataType::Int64);
        let i64_arr = arr.as_any().downcast_ref::<Int64Array>().unwrap();
        // Values should only be 0 or 1
        for v in i64_arr.values().iter() {
            assert!(*v == 0 || *v == 1, "bernoulli value {v} not 0 or 1");
        }
        // Mean should be ≈ 0.3
        let sum: i64 = i64_arr.values().iter().sum();
        let mean = sum as f64 / 10_000.0;
        assert!((mean - 0.3).abs() < 0.05, "bernoulli mean {mean} not ≈ 0.3");
    }

    #[test]
    fn geometric_produces_positive_int64() {
        let mut params = BTreeMap::new();
        params.insert("p".into(), 0.2);
        let plan = GeneratorPlan::Distribution {
            kind: DistributionKind::Geometric,
            params,
            clamp_min: None,
            clamp_max: None,
            round: false,
        };
        let gen = create_generator(&plan);
        let ctx = make_ctx();
        let arr = gen.generate(&mut make_rng(), 1000, &ctx);
        assert_eq!(*arr.data_type(), DataType::Int64);
        let i64_arr = arr.as_any().downcast_ref::<Int64Array>().unwrap();
        for v in i64_arr.values().iter() {
            assert!(*v >= 0, "geometric value {v} is negative");
        }
    }

    #[test]
    fn zipf_produces_bounded_int64() {
        let mut params = BTreeMap::new();
        params.insert("s".into(), 1.5);
        params.insert("n".into(), 100.0);
        let plan = GeneratorPlan::Distribution {
            kind: DistributionKind::Zipf,
            params,
            clamp_min: None,
            clamp_max: None,
            round: false,
        };
        let gen = create_generator(&plan);
        let ctx = make_ctx();
        let arr = gen.generate(&mut make_rng(), 1000, &ctx);
        assert_eq!(*arr.data_type(), DataType::Int64);
        let i64_arr = arr.as_any().downcast_ref::<Int64Array>().unwrap();
        for v in i64_arr.values().iter() {
            assert!(*v >= 1 && *v <= 100, "zipf value {v} out of [1,100]");
        }
    }

    // ── OneOf tests ─────────────────────────────────────────────────

    #[test]
    fn one_of_string_choices() {
        use knit_core::WeightedChoice;
        let plan = GeneratorPlan::OneOf {
            choices: vec![
                WeightedChoice { value: Value::String("a".into()), weight: 1.0 },
                WeightedChoice { value: Value::String("b".into()), weight: 1.0 },
                WeightedChoice { value: Value::String("c".into()), weight: 1.0 },
            ],
            cumulative_weights: vec![1.0 / 3.0, 2.0 / 3.0, 1.0],
        };
        let gen = create_generator(&plan);
        let ctx = make_ctx();
        let arr = gen.generate(&mut make_rng(), 1000, &ctx);
        assert_eq!(*arr.data_type(), DataType::Utf8);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();
        for i in 0..str_arr.len() {
            let v = str_arr.value(i);
            assert!(v == "a" || v == "b" || v == "c", "unexpected value: {v}");
        }
    }

    #[test]
    fn one_of_int_choices() {
        use knit_core::WeightedChoice;
        let plan = GeneratorPlan::OneOf {
            choices: vec![
                WeightedChoice { value: Value::Int(10), weight: 1.0 },
                WeightedChoice { value: Value::Int(20), weight: 1.0 },
                WeightedChoice { value: Value::Int(30), weight: 1.0 },
            ],
            cumulative_weights: vec![1.0 / 3.0, 2.0 / 3.0, 1.0],
        };
        let gen = create_generator(&plan);
        let ctx = make_ctx();
        let arr = gen.generate(&mut make_rng(), 1000, &ctx);
        assert_eq!(*arr.data_type(), DataType::Int64);
        let i64_arr = arr.as_any().downcast_ref::<Int64Array>().unwrap();
        for v in i64_arr.values().iter() {
            assert!(*v == 10 || *v == 20 || *v == 30, "unexpected value: {v}");
        }
    }

    #[test]
    fn one_of_weighted_distribution() {
        use knit_core::WeightedChoice;
        // 90% weight on "common", 10% on "rare"
        let plan = GeneratorPlan::OneOf {
            choices: vec![
                WeightedChoice { value: Value::String("common".into()), weight: 9.0 },
                WeightedChoice { value: Value::String("rare".into()), weight: 1.0 },
            ],
            cumulative_weights: vec![0.9, 1.0],
        };
        let gen = create_generator(&plan);
        let ctx = make_ctx();
        let arr = gen.generate(&mut make_rng(), 10_000, &ctx);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();
        let common_count = (0..str_arr.len())
            .filter(|i| str_arr.value(*i) == "common")
            .count();
        let ratio = common_count as f64 / 10_000.0;
        assert!(
            (ratio - 0.9).abs() < 0.05,
            "common ratio {ratio} not ≈ 0.9"
        );
    }

    // ── Pattern tests ───────────────────────────────────────────────

    #[test]
    fn pattern_digit_placeholder() {
        let plan = GeneratorPlan::Pattern {
            pattern: "#####".into(),
        };
        let gen = create_generator(&plan);
        let ctx = make_ctx();
        let arr = gen.generate(&mut make_rng(), 100, &ctx);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();
        for i in 0..str_arr.len() {
            let s = str_arr.value(i);
            assert_eq!(s.len(), 5, "pattern length wrong: {s}");
            assert!(
                s.chars().all(|c| c.is_ascii_digit()),
                "non-digit in pattern output: {s}"
            );
        }
    }

    #[test]
    fn pattern_letter_placeholder() {
        let plan = GeneratorPlan::Pattern {
            pattern: "???".into(),
        };
        let gen = create_generator(&plan);
        let ctx = make_ctx();
        let arr = gen.generate(&mut make_rng(), 100, &ctx);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();
        for i in 0..str_arr.len() {
            let s = str_arr.value(i);
            assert_eq!(s.len(), 3);
            assert!(
                s.chars().all(|c| c.is_ascii_lowercase()),
                "non-lowercase in pattern output: {s}"
            );
        }
    }

    #[test]
    fn pattern_mixed_with_literals() {
        let plan = GeneratorPlan::Pattern {
            pattern: "ID-###-??".into(),
        };
        let gen = create_generator(&plan);
        let ctx = make_ctx();
        let arr = gen.generate(&mut make_rng(), 50, &ctx);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();
        for i in 0..str_arr.len() {
            let s = str_arr.value(i);
            assert!(s.starts_with("ID-"), "missing prefix: {s}");
            assert_eq!(s.len(), 9, "wrong length: {s}");
            assert_eq!(&s[6..7], "-", "missing separator: {s}");
        }
    }

    // ── Faker tests ─────────────────────────────────────────────────

    #[test]
    fn faker_first_name_produces_strings() {
        let plan = GeneratorPlan::Faker {
            category: "first_name".into(),
            locale: "en_US".into(),
            args: vec![],
        };
        let gen = create_generator(&plan);
        let ctx = make_ctx();
        let arr = gen.generate(&mut make_rng(), 100, &ctx);
        assert_eq!(*arr.data_type(), DataType::Utf8);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();
        for i in 0..str_arr.len() {
            let s = str_arr.value(i);
            assert!(!s.is_empty(), "faker produced empty string");
        }
    }

    #[test]
    fn faker_email_contains_at_sign() {
        let plan = GeneratorPlan::Faker {
            category: "email".into(),
            locale: "en_US".into(),
            args: vec![],
        };
        let gen = create_generator(&plan);
        let ctx = make_ctx();
        let arr = gen.generate(&mut make_rng(), 100, &ctx);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();
        for i in 0..str_arr.len() {
            let s = str_arr.value(i);
            assert!(s.contains('@'), "email missing @: {s}");
        }
    }

    // ── Constant variant tests ──────────────────────────────────────

    #[test]
    fn constant_string() {
        let plan = GeneratorPlan::Constant(Value::String("hello".into()));
        let gen = create_generator(&plan);
        let ctx = make_ctx();
        let arr = gen.generate(&mut make_rng(), 10, &ctx);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();
        for i in 0..str_arr.len() {
            assert_eq!(str_arr.value(i), "hello");
        }
    }

    #[test]
    fn constant_float() {
        let plan = GeneratorPlan::Constant(Value::Float(1.234));
        let gen = create_generator(&plan);
        let ctx = make_ctx();
        let arr = gen.generate(&mut make_rng(), 10, &ctx);
        let f64_arr = arr.as_any().downcast_ref::<Float64Array>().unwrap();
        for v in f64_arr.values().iter() {
            assert_eq!(*v, 1.234);
        }
    }

    #[test]
    fn constant_bool() {
        let plan = GeneratorPlan::Constant(Value::Bool(true));
        let gen = create_generator(&plan);
        let ctx = make_ctx();
        let arr = gen.generate(&mut make_rng(), 10, &ctx);
        let bool_arr = arr.as_any().downcast_ref::<BooleanArray>().unwrap();
        for i in 0..bool_arr.len() {
            assert!(bool_arr.value(i));
        }
    }

    #[test]
    fn constant_null() {
        let plan = GeneratorPlan::Constant(Value::Null);
        let gen = create_generator(&plan);
        let ctx = make_ctx();
        let arr = gen.generate(&mut make_rng(), 10, &ctx);
        assert_eq!(*arr.data_type(), DataType::Null);
        assert_eq!(arr.len(), 10);
    }

    // ── Null mask edge cases ────────────────────────────────────────

    #[test]
    fn null_mask_never() {
        let plan = GeneratorPlan::Constant(Value::Int(1));
        let gen = create_generator(&plan);
        let ctx = make_ctx();
        let arr = gen.generate(&mut make_rng(), 100, &ctx);
        let masked = apply_null_mask(arr, &NullPlan::Never, &mut make_rng(), 100).unwrap();
        assert_eq!(masked.null_count(), 0);
    }

    #[test]
    fn null_mask_always() {
        let plan = GeneratorPlan::Constant(Value::Int(1));
        let gen = create_generator(&plan);
        let ctx = make_ctx();
        let arr = gen.generate(&mut make_rng(), 100, &ctx);
        let masked = apply_null_mask(arr, &NullPlan::Always, &mut make_rng(), 100).unwrap();
        // NullPlan::Always produces a NullArray (DataType::Null)
        assert_eq!(*masked.data_type(), DataType::Null);
        assert_eq!(masked.len(), 100);
    }

    // ── SampledKeyStore tests ───────────────────────────────────────

    #[test]
    fn sampled_keystore_basic() {
        let store = SampledKeyStore::new(100, 42);
        assert!(store.is_empty());
        for i in 0..50 {
            store.insert(i);
        }
        assert_eq!(store.len(), 50);
        let mut rng = make_rng();
        for _ in 0..100 {
            let key = store.sample(&mut rng).expect("store not empty");
            assert!((0..50).contains(&key));
        }
    }
}

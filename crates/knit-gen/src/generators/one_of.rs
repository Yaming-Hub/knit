//! Weighted random choice generator using the alias method.
//!
//! Implements Vose's alias method for O(1) sampling from a weighted discrete
//! distribution. The pre-computation step runs in O(n) time during construction.

use std::sync::Arc;

use arrow::array::{ArrayRef, BooleanArray, Float64Array, Int64Array, StringArray};
use arrow::datatypes::DataType;
use rand::RngCore;

use knit_core::{Value, WeightedChoice};

use crate::context::GenContext;
use crate::traits::FieldGenerator;

/// Generate values by weighted random choice using the alias method.
///
/// Each call to [`generate`](FieldGenerator::generate) picks one of the
/// pre-configured choices per row, with probability proportional to its weight.
/// Uses Vose's alias method for O(1) per-sample after O(n) construction.
///
/// # Output type
///
/// Inferred from the first non-null choice value:
/// - All `Value::String` → `Utf8`
/// - All `Value::Int` → `Int64`
/// - All `Value::Float` → `Float64`
/// - All `Value::Bool` → `Boolean`
/// - Otherwise → `Utf8` (debug-formatted)
pub struct OneOfGenerator {
    choices: Vec<Value>,
    /// Alias table: probability threshold for each slot.
    prob: Vec<f64>,
    /// Alias table: alternative index for each slot.
    alias: Vec<usize>,
    output_type: DataType,
}

impl OneOfGenerator {
    /// Build a new weighted-choice generator from plan data.
    ///
    /// `choices` are the weighted values, `cumulative_weights` from the plan
    /// are ignored — we rebuild the alias table from raw weights for correctness.
    pub fn new(choices: Vec<WeightedChoice>) -> Self {
        if choices.is_empty() {
            tracing::warn!("OneOfGenerator created with empty choices, will produce nulls");
            return Self {
                choices: vec![],
                prob: vec![],
                alias: vec![],
                output_type: DataType::Utf8,
            };
        }

        let output_type = infer_output_type(&choices);
        let values: Vec<Value> = choices.iter().map(|c| c.value.clone()).collect();
        let weights: Vec<f64> = choices.iter().map(|c| c.weight.max(0.0)).collect();

        let (prob, alias) = build_alias_table(&weights);

        Self {
            choices: values,
            prob,
            alias,
            output_type,
        }
    }

    /// Sample a single index using the alias method.
    fn sample_index(&self, rng: &mut dyn RngCore) -> usize {
        let n = self.choices.len();
        if n == 0 {
            return 0;
        }
        // Generate two uniform random values from the RNG.
        let u1 = next_f64(rng);
        let i = (u1 * n as f64) as usize;
        let i = i.min(n - 1);
        let u2 = next_f64(rng);
        if u2 < self.prob[i] {
            i
        } else {
            self.alias[i]
        }
    }
}

/// Generate a uniform f64 in [0, 1) from an RNG.
fn next_f64(rng: &mut dyn RngCore) -> f64 {
    let bits = rng.next_u64();
    // Use top 53 bits for a double in [0, 1).
    (bits >> 11) as f64 / (1u64 << 53) as f64
}

/// Infer Arrow output type from the first non-null choice.
fn infer_output_type(choices: &[WeightedChoice]) -> DataType {
    for c in choices {
        match &c.value {
            Value::String(_) => return DataType::Utf8,
            Value::Int(_) => return DataType::Int64,
            Value::Float(_) => return DataType::Float64,
            Value::Bool(_) => return DataType::Boolean,
            Value::Null => continue,
            _ => return DataType::Utf8,
        }
    }
    DataType::Utf8
}

/// Build Vose's alias table from a weight vector.
///
/// Returns `(prob, alias)` arrays of length `n`. Each slot `i` represents a
/// mixture of outcome `i` (with probability `prob[i]`) and outcome `alias[i]`.
fn build_alias_table(weights: &[f64]) -> (Vec<f64>, Vec<usize>) {
    let n = weights.len();
    if n == 0 {
        return (vec![], vec![]);
    }

    let total: f64 = weights.iter().sum();
    let avg = if total > 0.0 { total / n as f64 } else { 1.0 };

    let mut prob = vec![0.0; n];
    let mut alias = vec![0usize; n];
    let mut small = Vec::with_capacity(n);
    let mut large = Vec::with_capacity(n);

    let mut scaled: Vec<f64> = weights.iter().map(|w| w / avg).collect();

    for (i, s) in scaled.iter().enumerate() {
        if *s < 1.0 {
            small.push(i);
        } else {
            large.push(i);
        }
    }

    while let (Some(s), Some(&l)) = (small.pop(), large.last()) {
        prob[s] = scaled[s];
        alias[s] = l;
        scaled[l] = (scaled[l] + scaled[s]) - 1.0;
        if scaled[l] < 1.0 {
            large.pop();
            small.push(l);
        }
    }

    // Remaining entries have probability 1.0 (within floating point).
    for &l in &large {
        prob[l] = 1.0;
    }
    for &s in &small {
        prob[s] = 1.0;
    }

    (prob, alias)
}

impl FieldGenerator for OneOfGenerator {
    fn generate(&self, rng: &mut dyn RngCore, count: usize, _ctx: &GenContext) -> ArrayRef {
        if self.choices.is_empty() {
            return Arc::new(StringArray::from(vec![""; count]));
        }

        match self.output_type {
            DataType::Utf8 => {
                let values: Vec<String> = (0..count)
                    .map(|_| {
                        let idx = self.sample_index(rng);
                        match &self.choices[idx] {
                            Value::String(s) => s.clone(),
                            other => format!("{other:?}"),
                        }
                    })
                    .collect();
                Arc::new(StringArray::from(
                    values.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                ))
            }
            DataType::Int64 => {
                let values: Vec<i64> = (0..count)
                    .map(|_| {
                        let idx = self.sample_index(rng);
                        match &self.choices[idx] {
                            Value::Int(v) => *v,
                            _ => 0,
                        }
                    })
                    .collect();
                Arc::new(Int64Array::from(values))
            }
            DataType::Float64 => {
                let values: Vec<f64> = (0..count)
                    .map(|_| {
                        let idx = self.sample_index(rng);
                        match &self.choices[idx] {
                            Value::Float(v) => *v,
                            Value::Int(v) => *v as f64,
                            _ => 0.0,
                        }
                    })
                    .collect();
                Arc::new(Float64Array::from(values))
            }
            DataType::Boolean => {
                let values: Vec<bool> = (0..count)
                    .map(|_| {
                        let idx = self.sample_index(rng);
                        match &self.choices[idx] {
                            Value::Bool(v) => *v,
                            _ => false,
                        }
                    })
                    .collect();
                Arc::new(BooleanArray::from(values))
            }
            _ => {
                let values: Vec<String> = (0..count)
                    .map(|_| {
                        let idx = self.sample_index(rng);
                        format!("{:?}", self.choices[idx])
                    })
                    .collect();
                Arc::new(StringArray::from(
                    values.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                ))
            }
        }
    }

    fn output_type(&self) -> DataType {
        self.output_type.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::Array;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;
    use std::collections::HashMap;

    fn make_ctx() -> GenContext<'static> {
        let map: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(HashMap::new()));
        GenContext::new(map, 0, 0, 1, "test")
    }

    #[test]
    fn weighted_choice_respects_weights() {
        let choices = vec![
            WeightedChoice {
                value: Value::String("rare".into()),
                weight: 1.0,
            },
            WeightedChoice {
                value: Value::String("common".into()),
                weight: 99.0,
            },
        ];
        let gen = OneOfGenerator::new(choices);
        let ctx = make_ctx();
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, 10_000, &ctx);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();

        let common_count = (0..str_arr.len())
            .filter(|&i| str_arr.value(i) == "common")
            .count();
        let ratio = common_count as f64 / 10_000.0;
        assert!(
            ratio > 0.95,
            "expected ~99% common, got {:.1}%",
            ratio * 100.0
        );
    }

    #[test]
    fn int_choices() {
        let choices = vec![
            WeightedChoice {
                value: Value::Int(10),
                weight: 1.0,
            },
            WeightedChoice {
                value: Value::Int(20),
                weight: 1.0,
            },
        ];
        let gen = OneOfGenerator::new(choices);
        assert_eq!(gen.output_type(), DataType::Int64);
        let ctx = make_ctx();
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, 100, &ctx);
        let i64_arr = arr.as_any().downcast_ref::<Int64Array>().unwrap();
        for v in i64_arr.values().iter() {
            assert!(*v == 10 || *v == 20);
        }
    }

    #[test]
    fn empty_choices_does_not_panic() {
        let gen = OneOfGenerator::new(vec![]);
        let ctx = make_ctx();
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, 5, &ctx);
        assert_eq!(arr.len(), 5);
    }
}

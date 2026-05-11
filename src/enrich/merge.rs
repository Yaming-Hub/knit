//! Distribution merging — combine reference statistics with existing model generators.

use std::collections::BTreeMap;

use tracing::{debug, info, warn};

use crate::core::types::*;
use crate::core::Field;
use crate::enrich::extract::FieldEnrichment;
use crate::learn::fitting::Distribution;

/// Merge enrichment data into a field's generator.
///
/// Returns true if the field was successfully enriched, false if skipped.
pub fn merge_enrichment(field: &mut Field, enrichment: &FieldEnrichment, ref_row_count: u64) -> bool {
    let Some(ref mut gen) = field.generator else {
        debug!(field = %field.name, "no generator to enrich");
        return false;
    };

    match gen {
        GeneratorSpec::Distribution { spec } => {
            merge_distribution(field.name.as_str(), spec, enrichment, ref_row_count)
        }
        GeneratorSpec::OneOf { choices } => {
            merge_oneof(field.name.as_str(), choices, enrichment, ref_row_count)
        }
        _ => {
            debug!(field = %field.name, gen_type = gen.type_name(), "skipping non-statistical generator");
            false
        }
    }
}

/// Merge a numeric distribution with reference data.
fn merge_distribution(
    field_name: &str,
    spec: &mut DistributionSpec,
    enrichment: &FieldEnrichment,
    ref_row_count: u64,
) -> bool {
    let Some(ref fit) = enrichment.distribution else {
        debug!(field = %field_name, "no distribution fit from reference");
        return false;
    };

    // Map the fit result to a DistributionKind using canonical param names
    let (ref_kind, ref_params) = match &fit.best.distribution {
        Distribution::Normal(mean, std) => (
            DistributionKind::Normal,
            BTreeMap::from([("mean".to_string(), *mean), ("std_dev".to_string(), *std)]),
        ),
        Distribution::LogNormal(mu, sigma) => (
            DistributionKind::LogNormal,
            BTreeMap::from([("mu".to_string(), *mu), ("sigma".to_string(), *sigma)]),
        ),
        Distribution::Exponential(lambda) => (
            DistributionKind::Exponential,
            BTreeMap::from([("lambda".to_string(), *lambda)]),
        ),
        Distribution::Uniform(min, max) => (
            DistributionKind::Uniform,
            BTreeMap::from([("min".to_string(), *min), ("max".to_string(), *max)]),
        ),
        Distribution::Gamma(shape, rate) => (
            DistributionKind::Gamma,
            BTreeMap::from([("shape".to_string(), *shape), ("scale".to_string(), 1.0 / rate)]),
        ),
        Distribution::Beta(alpha, beta) => (
            DistributionKind::Beta,
            BTreeMap::from([("alpha".to_string(), *alpha), ("beta".to_string(), *beta)]),
        ),
        Distribution::Pareto(xm, alpha) => (
            DistributionKind::Pareto,
            BTreeMap::from([("scale".to_string(), *xm), ("shape".to_string(), *alpha)]),
        ),
        Distribution::Poisson(lambda) => (
            DistributionKind::Poisson,
            BTreeMap::from([("lambda".to_string(), *lambda)]),
        ),
        Distribution::Zipf(n, s) => (
            DistributionKind::Zipf,
            BTreeMap::from([("n".to_string(), *n as f64), ("s".to_string(), *s)]),
        ),
    };

    // Only merge same-family distributions
    if spec.kind != ref_kind {
        warn!(
            field = %field_name,
            base = ?spec.kind,
            reference = ?ref_kind,
            "distribution family mismatch — skipping (would need --replace-generator-kind)"
        );
        return false;
    }

    // Weighted merge of parameters
    // Use base weight = 1.0 (prior), reference weight proportional to sample size
    let base_weight = 1.0_f64;
    let ref_weight = (enrichment.sample_size as f64 / ref_row_count.max(1) as f64).min(2.0);
    let total = base_weight + ref_weight;

    let mut updated = false;

    // Special case for Normal: use combined variance formula that accounts for mean difference
    if spec.kind == DistributionKind::Normal {
        if let (Some(base_mean), Some(base_std)) = (spec.params.get("mean").copied(), spec.params.get("std_dev").copied()) {
            if let (Some(&ref_mean), Some(&ref_std)) = (ref_params.get("mean"), ref_params.get("std_dev")) {
                let merged_mean = (base_weight * base_mean + ref_weight * ref_mean) / total;
                // Combined variance includes between-mean variance
                let base_var = base_std * base_std;
                let ref_var = ref_std * ref_std;
                let combined_var = (base_weight * (base_var + (base_mean - merged_mean).powi(2))
                    + ref_weight * (ref_var + (ref_mean - merged_mean).powi(2))) / total;
                spec.params.insert("mean".to_string(), merged_mean);
                spec.params.insert("std_dev".to_string(), combined_var.sqrt());
                updated = true;
            }
        }
    } else {
        for (key, ref_val) in &ref_params {
            if let Some(base_val) = spec.params.get_mut(key) {
                *base_val = (base_weight * *base_val + ref_weight * ref_val) / total;
                updated = true;
            }
        }
    }

    if !updated {
        debug!(field = %field_name, "no parameters updated");
        return false;
    }

    info!(
        field = %field_name,
        kind = ?spec.kind,
        "distribution parameters enriched"
    );
    true
}

/// Merge categorical data into a OneOf generator using weighted averaging.
fn merge_oneof(
    field_name: &str,
    choices: &mut Vec<WeightedChoice>,
    enrichment: &FieldEnrichment,
    _ref_row_count: u64,
) -> bool {
    let Some(ref cat_fit) = enrichment.categorical else {
        debug!(field = %field_name, "no categorical fit from reference");
        return false;
    };

    if cat_fit.weights.is_empty() {
        return false;
    }

    // Normalize base weights to probabilities
    let base_total: f64 = choices.iter().map(|c| c.weight).sum();
    if base_total <= 0.0 {
        return false;
    }

    let ref_total: f64 = cat_fit.weights.values().sum();
    // Weight base more heavily (prior) to avoid reference overwhelming existing knowledge
    let base_w = 0.6_f64;
    let ref_w = 0.4_f64;

    // Update weights for existing values via weighted average of probabilities
    for choice in choices.iter_mut() {
        let val_str = match &choice.value {
            Value::String(s) => s.clone(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => f.to_string(),
            Value::Bool(b) => b.to_string(),
            _ => continue,
        };

        let base_frac = choice.weight / base_total;
        let ref_frac = cat_fit.weights.get(&val_str)
            .map(|&w| w / ref_total.max(1.0))
            .unwrap_or(0.0);

        choice.weight = base_w * base_frac + ref_w * ref_frac;
    }

    // Add new values from reference that aren't in base
    let existing_vals: Vec<String> = choices.iter().map(|c| match &c.value {
        Value::String(s) => s.clone(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }).collect();

    let mut added = 0;
    for (val, &weight) in &cat_fit.weights {
        if !existing_vals.contains(val) {
            let ref_frac = weight / ref_total.max(1.0);
            // Only add new values if they have meaningful frequency
            if ref_frac > 0.01 {
                choices.push(WeightedChoice {
                    value: Value::String(val.clone()),
                    weight: ref_w * ref_frac,
                });
                added += 1;
            }
        }
    }

    // Normalize weights to sum to 1.0
    let total: f64 = choices.iter().map(|c| c.weight).sum();
    if total > 0.0 {
        for choice in choices.iter_mut() {
            choice.weight /= total;
        }
    }

    info!(
        field = %field_name,
        existing = existing_vals.len(),
        added = added,
        "categorical values enriched"
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::DataType;
    use crate::learn::fitting::{CandidateFit, CategoricalFit, Distribution, FitResult};
    use std::collections::HashMap;

    fn make_field(name: &str, dt: DataType, gen: GeneratorSpec) -> Field {
        Field {
            name: name.to_string(),
            description: None,
            data_type: dt,
            generator: Some(gen),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
        }
    }

    #[test]
    fn test_merge_same_family_normal() {
        let mut field = make_field(
            "score",
            DataType::Float,
            GeneratorSpec::Distribution {
                spec: DistributionSpec {
                    kind: DistributionKind::Normal,
                    params: BTreeMap::from([
                        ("mean".to_string(), 50.0),
                        ("std_dev".to_string(), 10.0),
                    ]),
                    array_params: BTreeMap::new(),
                    round: false,
                },
            },
        );

        let enrichment = FieldEnrichment {
            distribution: Some(FitResult {
                best: CandidateFit {
                    distribution: Distribution::Normal(60.0, 15.0),
                    ks_stat: 0.05,
                    p_value: 0.8,
                    aic: 100.0,
                    bic: 110.0,
                },
                alternatives: vec![],
            }),
            categorical: None,
            null_rate: 0.0,
            sample_size: 100,
        };

        let result = merge_enrichment(&mut field, &enrichment, 100);
        assert!(result);

        // Check that mean moved toward 60
        if let Some(GeneratorSpec::Distribution { spec }) = &field.generator {
            let mean = spec.params["mean"];
            assert!(mean > 50.0 && mean < 60.0, "mean should be between 50 and 60, got {}", mean);
        } else {
            panic!("expected Distribution generator");
        }
    }

    #[test]
    fn test_merge_different_family_skips() {
        let mut field = make_field(
            "score",
            DataType::Float,
            GeneratorSpec::Distribution {
                spec: DistributionSpec {
                    kind: DistributionKind::Normal,
                    params: BTreeMap::from([
                        ("mean".to_string(), 50.0),
                        ("std_dev".to_string(), 10.0),
                    ]),
                    array_params: BTreeMap::new(),
                    round: false,
                },
            },
        );

        let enrichment = FieldEnrichment {
            distribution: Some(FitResult {
                best: CandidateFit {
                    distribution: Distribution::Exponential(0.5),
                    ks_stat: 0.1,
                    p_value: 0.6,
                    aic: 200.0,
                    bic: 210.0,
                },
                alternatives: vec![],
            }),
            categorical: None,
            null_rate: 0.0,
            sample_size: 100,
        };

        let result = merge_enrichment(&mut field, &enrichment, 100);
        assert!(!result); // Should skip due to family mismatch
    }

    #[test]
    fn test_merge_oneof_adds_values() {
        let mut field = make_field(
            "region",
            DataType::String,
            GeneratorSpec::OneOf {
                choices: vec![
                    WeightedChoice { value: Value::String("US".into()), weight: 0.6 },
                    WeightedChoice { value: Value::String("EU".into()), weight: 0.4 },
                ],
            },
        );

        let mut weights = HashMap::new();
        weights.insert("US".to_string(), 0.4);
        weights.insert("EU".to_string(), 0.3);
        weights.insert("APAC".to_string(), 0.2);
        weights.insert("LATAM".to_string(), 0.1);

        let enrichment = FieldEnrichment {
            distribution: None,
            categorical: Some(CategoricalFit { weights, cardinality: 4 }),
            null_rate: 0.0,
            sample_size: 200,
        };

        let result = merge_enrichment(&mut field, &enrichment, 200);
        assert!(result);

        if let Some(GeneratorSpec::OneOf { choices }) = &field.generator {
            // Should have added APAC and LATAM
            let vals: Vec<String> = choices.iter().map(|c| match &c.value {
                Value::String(s) => s.clone(),
                _ => String::new(),
            }).collect();
            assert!(vals.contains(&"APAC".to_string()));
            assert!(vals.contains(&"LATAM".to_string()));
            // Weights should sum to ~1.0
            let total: f64 = choices.iter().map(|c| c.weight).sum();
            assert!((total - 1.0).abs() < 0.01);
        } else {
            panic!("expected OneOf generator");
        }
    }

    #[test]
    fn test_skip_non_statistical_generator() {
        let mut field = make_field(
            "id",
            DataType::Int,
            GeneratorSpec::Sequence {
                start: IntOrString::Int(1),
                step: IntOrString::Int(1),
                prefix: None,
                values: None,
                cycle: None,
                jitter: None,
            },
        );

        let enrichment = FieldEnrichment {
            distribution: None,
            categorical: None,
            null_rate: 0.0,
            sample_size: 100,
        };

        let result = merge_enrichment(&mut field, &enrichment, 100);
        assert!(!result); // Sequence generator should be skipped
    }
}
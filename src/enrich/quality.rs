//! Enrichment quality scoring — evaluates how well reference data matches the model.
//!
//! Computes per-field and aggregate quality metrics after enrichment, helping
//! users judge whether the enrichment is trustworthy.

use std::fmt;

use colored::Colorize;

use crate::enrich::extract::FieldEnrichment;
use crate::enrich::mapper::ColumnMapping;

/// Quality report for an entire enrichment run.
#[derive(Debug, Clone)]
pub struct QualityReport {
    /// Overall quality score (0.0–1.0).
    pub overall_score: f64,
    /// Average mapping confidence across accepted mappings.
    pub avg_mapping_confidence: f64,
    /// Average fit quality (1 - ks_stat) for numeric fields.
    pub avg_fit_quality: Option<f64>,
    /// Fraction of fields with adequate sample size (≥ 30).
    pub sample_adequacy: f64,
    /// Per-field quality scores.
    pub field_scores: Vec<FieldQuality>,
    /// Actionable concerns.
    pub concerns: Vec<QualityConcern>,
}

/// Quality metrics for a single enriched field.
#[derive(Debug, Clone)]
pub struct FieldQuality {
    /// Field name in the model.
    pub field_name: String,
    /// Reference column name.
    pub ref_col_name: String,
    /// Mapping confidence (0.0–1.0).
    pub mapping_confidence: f64,
    /// Whether types are compatible.
    pub type_compatible: bool,
    /// Distribution fit quality (1 - ks_stat), if numeric.
    pub fit_quality: Option<f64>,
    /// KS p-value, if numeric.
    pub p_value: Option<f64>,
    /// Sample size used for fitting.
    pub sample_size: u64,
    /// Observed null rate in reference.
    pub null_rate: f64,
    /// Categorical cardinality, if categorical.
    pub categorical_cardinality: Option<usize>,
    /// Whether the merge succeeded.
    pub merge_succeeded: bool,
    /// Composite field score (0.0–1.0).
    pub score: f64,
}

/// A specific quality concern worth flagging.
#[derive(Debug, Clone)]
pub struct QualityConcern {
    /// Severity: "high", "medium", "low".
    pub severity: &'static str,
    /// Which field is affected.
    pub field: String,
    /// Description of the concern.
    pub message: String,
}

/// Compute quality metrics for a single field enrichment.
pub fn score_field(
    mapping: &ColumnMapping,
    enrichment: &FieldEnrichment,
    merge_succeeded: bool,
) -> FieldQuality {
    let mut concerns_weight = 0.0;

    // Distribution fit quality
    let (fit_quality, p_value) = if let Some(ref fit) = enrichment.distribution {
        let fq = 1.0 - fit.best.ks_stat;
        (Some(fq), Some(fit.best.p_value))
    } else {
        (None, None)
    };

    // Categorical cardinality
    let categorical_cardinality = enrichment.categorical.as_ref().map(|c| c.cardinality);

    // Sample adequacy penalty
    if enrichment.sample_size < 30 {
        concerns_weight += 0.2;
    }

    // Type mismatch penalty
    if !mapping.type_compatible {
        concerns_weight += 0.15;
    }

    // Merge failure penalty
    if !merge_succeeded {
        concerns_weight += 0.3;
    }

    // Compute composite score
    let mapping_component = mapping.confidence;
    let fit_component = fit_quality.unwrap_or(0.8); // default to 0.8 for non-numeric
    let sample_component = if enrichment.sample_size >= 100 {
        1.0
    } else if enrichment.sample_size >= 30 {
        0.8
    } else {
        0.5
    };

    let raw_score = 0.4 * mapping_component + 0.35 * fit_component + 0.25 * sample_component;
    let score = (raw_score - concerns_weight).clamp(0.0, 1.0);

    FieldQuality {
        field_name: mapping.target_field.clone(),
        ref_col_name: mapping.ref_col_name.clone(),
        mapping_confidence: mapping.confidence,
        type_compatible: mapping.type_compatible,
        fit_quality,
        p_value,
        sample_size: enrichment.sample_size,
        null_rate: enrichment.null_rate,
        categorical_cardinality,
        merge_succeeded,
        score,
    }
}

/// Aggregate field scores into an overall quality report.
pub fn build_report(field_scores: Vec<FieldQuality>) -> QualityReport {
    if field_scores.is_empty() {
        return QualityReport {
            overall_score: 0.0,
            avg_mapping_confidence: 0.0,
            avg_fit_quality: None,
            sample_adequacy: 0.0,
            field_scores,
            concerns: vec![],
        };
    }

    let n = field_scores.len() as f64;

    let avg_mapping_confidence =
        field_scores.iter().map(|f| f.mapping_confidence).sum::<f64>() / n;

    let fit_scores: Vec<f64> = field_scores.iter().filter_map(|f| f.fit_quality).collect();
    let avg_fit_quality = if fit_scores.is_empty() {
        None
    } else {
        Some(fit_scores.iter().sum::<f64>() / fit_scores.len() as f64)
    };

    let adequate_count = field_scores.iter().filter(|f| f.sample_size >= 30).count();
    let sample_adequacy = adequate_count as f64 / n;

    let overall_score = field_scores.iter().map(|f| f.score).sum::<f64>() / n;

    // Collect concerns
    let mut concerns = Vec::new();
    for f in &field_scores {
        if f.mapping_confidence < 0.7 {
            concerns.push(QualityConcern {
                severity: "medium",
                field: f.field_name.clone(),
                message: format!("low mapping confidence ({:.0}%)", f.mapping_confidence * 100.0),
            });
        }
        if let Some(fq) = f.fit_quality {
            if fq < 0.9 {
                concerns.push(QualityConcern {
                    severity: if fq < 0.7 { "high" } else { "medium" },
                    field: f.field_name.clone(),
                    message: format!(
                        "distribution fit quality {:.0}% (KS={:.3})",
                        fq * 100.0,
                        1.0 - fq
                    ),
                });
            }
        }
        if let Some(pv) = f.p_value {
            if pv < 0.05 {
                concerns.push(QualityConcern {
                    severity: "high",
                    field: f.field_name.clone(),
                    message: format!(
                        "KS test rejects fit (p={:.4})",
                        pv
                    ),
                });
            }
        }
        if f.sample_size < 30 {
            concerns.push(QualityConcern {
                severity: "medium",
                field: f.field_name.clone(),
                message: format!("small sample size (n={})", f.sample_size),
            });
        }
        if !f.type_compatible {
            concerns.push(QualityConcern {
                severity: "medium",
                field: f.field_name.clone(),
                message: "type mismatch between reference and model".into(),
            });
        }
        if !f.merge_succeeded {
            concerns.push(QualityConcern {
                severity: "high",
                field: f.field_name.clone(),
                message: "enrichment merge failed (incompatible generator)".into(),
            });
        }
    }

    QualityReport {
        overall_score,
        avg_mapping_confidence,
        avg_fit_quality,
        sample_adequacy,
        field_scores,
        concerns,
    }
}

/// Rating label for a score.
fn rating(score: f64) -> &'static str {
    if score >= 0.9 {
        "excellent"
    } else if score >= 0.75 {
        "good"
    } else if score >= 0.5 {
        "fair"
    } else {
        "poor"
    }
}

impl fmt::Display for QualityReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}", "== Enrichment Quality Report ==".bold())?;
        writeln!(
            f,
            "  Overall score:        {:.0}% [{}]",
            self.overall_score * 100.0,
            rating(self.overall_score).to_uppercase(),
        )?;
        writeln!(
            f,
            "  Mapping confidence:   {:.0}%",
            self.avg_mapping_confidence * 100.0,
        )?;
        if let Some(fq) = self.avg_fit_quality {
            writeln!(f, "  Distribution fit:     {:.0}%", fq * 100.0)?;
        }
        writeln!(
            f,
            "  Sample adequacy:      {:.0}% (fields with n≥30)",
            self.sample_adequacy * 100.0,
        )?;

        if !self.field_scores.is_empty() {
            writeln!(f)?;
            writeln!(
                f,
                "  {:<20} {:<20} {:>6}  {:>6}  {:>8}  {:>6}",
                "Field", "Ref Column", "Conf", "Fit", "Samples", "Score"
            )?;
            writeln!(f, "  {}", "─".repeat(72))?;
            for fs in &self.field_scores {
                let fit_str = fs
                    .fit_quality
                    .map(|q| format!("{:.0}%", q * 100.0))
                    .unwrap_or_else(|| "—".into());
                let status = if fs.merge_succeeded { " " } else { "✗" };
                writeln!(
                    f,
                    "  {:<20} {:<20} {:>5.0}%  {:>6}  {:>8}  {:>5.0}% {}",
                    fs.field_name,
                    fs.ref_col_name,
                    fs.mapping_confidence * 100.0,
                    fit_str,
                    fs.sample_size,
                    fs.score * 100.0,
                    status,
                )?;
            }
        }

        if !self.concerns.is_empty() {
            writeln!(f)?;
            writeln!(f, "  {}", "Concerns:".bold())?;
            for c in &self.concerns {
                let icon = match c.severity {
                    "high" => "⚠",
                    "medium" => "●",
                    _ => "○",
                };
                writeln!(f, "    {} [{}] {}: {}", icon, c.severity, c.field, c.message)?;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::learn::fitting::{CandidateFit, CategoricalFit, Distribution, FitResult};
    use std::collections::HashMap;

    fn make_mapping(ref_col: &str, target: &str, confidence: f64) -> ColumnMapping {
        ColumnMapping {
            ref_col_index: 0,
            ref_col_name: ref_col.to_string(),
            target_field: target.to_string(),
            confidence,
            type_compatible: true,
        }
    }

    fn make_numeric_enrichment(ks_stat: f64, p_value: f64, sample_size: u64) -> FieldEnrichment {
        FieldEnrichment {
            distribution: Some(FitResult {
                best: CandidateFit {
                    distribution: Distribution::Normal(0.0, 1.0),
                    ks_stat,
                    p_value,
                    aic: 100.0,
                    bic: 105.0,
                },
                alternatives: vec![],
            }),
            categorical: None,
            null_rate: 0.0,
            sample_size,
        }
    }

    fn make_categorical_enrichment(cardinality: usize, sample_size: u64) -> FieldEnrichment {
        let weights: HashMap<String, f64> = (0..cardinality)
            .map(|i| (format!("cat_{}", i), 1.0 / cardinality as f64))
            .collect();
        FieldEnrichment {
            distribution: None,
            categorical: Some(CategoricalFit {
                weights,
                cardinality,
            }),
            null_rate: 0.05,
            sample_size,
        }
    }

    #[test]
    fn score_high_quality_numeric_field() {
        let mapping = make_mapping("age_col", "age", 0.95);
        let enrichment = make_numeric_enrichment(0.03, 0.85, 500);
        let fq = score_field(&mapping, &enrichment, true);

        assert!(fq.score > 0.8, "high-quality field score should be >0.8, got {}", fq.score);
        assert_eq!(fq.fit_quality, Some(0.97));
        assert_eq!(fq.p_value, Some(0.85));
        assert!(fq.merge_succeeded);
    }

    #[test]
    fn score_low_quality_field() {
        let mut mapping = make_mapping("foo", "bar", 0.55);
        mapping.type_compatible = false;
        let enrichment = make_numeric_enrichment(0.4, 0.01, 15);
        let fq = score_field(&mapping, &enrichment, false);

        assert!(fq.score < 0.3, "low-quality field score should be <0.3, got {}", fq.score);
        assert!(!fq.merge_succeeded);
    }

    #[test]
    fn score_categorical_field() {
        let mapping = make_mapping("category", "category", 0.90);
        let enrichment = make_categorical_enrichment(5, 200);
        let fq = score_field(&mapping, &enrichment, true);

        assert!(fq.score > 0.7);
        assert_eq!(fq.categorical_cardinality, Some(5));
        assert_eq!(fq.fit_quality, None);
    }

    #[test]
    fn build_report_empty() {
        let report = build_report(vec![]);
        assert_eq!(report.overall_score, 0.0);
        assert!(report.concerns.is_empty());
    }

    #[test]
    fn build_report_aggregates_correctly() {
        let m1 = make_mapping("age", "age", 0.95);
        let e1 = make_numeric_enrichment(0.03, 0.85, 500);
        let f1 = score_field(&m1, &e1, true);

        let m2 = make_mapping("name", "name", 0.85);
        let e2 = make_categorical_enrichment(10, 200);
        let f2 = score_field(&m2, &e2, true);

        let report = build_report(vec![f1, f2]);

        assert_eq!(report.field_scores.len(), 2);
        assert!((report.avg_mapping_confidence - 0.9).abs() < 0.01);
        assert!(report.avg_fit_quality.is_some());
        assert_eq!(report.sample_adequacy, 1.0);
    }

    #[test]
    fn build_report_flags_concerns() {
        let mut m = make_mapping("x", "y", 0.5);
        m.type_compatible = false;
        let e = make_numeric_enrichment(0.5, 0.01, 10);
        let fq = score_field(&m, &e, false);

        let report = build_report(vec![fq]);

        assert!(report.concerns.len() >= 3, "expected multiple concerns, got {}", report.concerns.len());
        let severities: Vec<&str> = report.concerns.iter().map(|c| c.severity).collect();
        assert!(severities.contains(&"high"), "expected high severity concern");
    }

    #[test]
    fn display_report_format() {
        let m = make_mapping("age_col", "age", 0.92);
        let e = make_numeric_enrichment(0.04, 0.82, 300);
        let fq = score_field(&m, &e, true);
        let report = build_report(vec![fq]);

        let output = format!("{}", report);
        assert!(output.contains("Quality Report"));
        assert!(output.contains("age"));
        assert!(output.contains("age_col"));
    }
}

//! Decision logging and reporting infrastructure.
//!
//! Captures key decisions made during learn and generate pipelines with their
//! context, alternatives considered, confidence levels, and reasoning. The
//! collected decisions can be serialized as a JSON report for troubleshooting.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use serde::Serialize;

/// Global decision logger, set once at program start.
static GLOBAL_LOGGER: OnceLock<DecisionLogger> = OnceLock::new();

/// Set the global decision logger. Call once at program start.
pub fn set_global_logger(logger: DecisionLogger) {
    let _ = GLOBAL_LOGGER.set(logger);
}

/// Get the global decision logger, if one was set.
/// Returns None if `--decision-report` was not specified.
pub fn global_logger() -> Option<&'static DecisionLogger> {
    GLOBAL_LOGGER.get()
}

/// Kind of decision captured during pipeline execution.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    /// Distribution fitting choice for a column.
    DistributionFit,
    /// Type inference (e.g., categorical vs free-text).
    TypeInference,
    /// Generator selection for a field.
    GeneratorSelection,
    /// Foreign-key relationship detection.
    RelationshipDetection,
    /// Temporal cadence / pattern detection.
    TemporalDetection,
    /// Correlation acceptance or rejection.
    CorrelationEvaluation,
    /// Primary key type detection (string vs int).
    PrimaryKeyType,
    /// Partition row allocation strategy.
    PartitionAllocation,
    /// Count scaling application.
    CountScaling,
    /// FK generator selection.
    ForeignKeyGenerator,
    /// Noise injection decision.
    NoiseInjection,
    /// Index/key storage strategy selection.
    IndexStrategy,
    /// Companion file classification (data vs auxiliary).
    CompanionClassification,
    /// Null handling (always-null detection).
    NullHandling,
    /// Other decisions not fitting predefined categories.
    Other,
}

impl DecisionKind {
    /// Short human-readable label for display.
    pub fn label(&self) -> &'static str {
        match self {
            DecisionKind::DistributionFit => "distribution",
            DecisionKind::TypeInference => "type",
            DecisionKind::GeneratorSelection => "generator",
            DecisionKind::RelationshipDetection => "relationship",
            DecisionKind::TemporalDetection => "temporal",
            DecisionKind::CorrelationEvaluation => "correlation",
            DecisionKind::PrimaryKeyType => "primary-key",
            DecisionKind::PartitionAllocation => "partition",
            DecisionKind::CountScaling => "count",
            DecisionKind::ForeignKeyGenerator => "fk-generator",
            DecisionKind::NoiseInjection => "noise",
            DecisionKind::IndexStrategy => "index",
            DecisionKind::CompanionClassification => "companion",
            DecisionKind::NullHandling => "null",
            DecisionKind::Other => "other",
        }
    }
}

/// Confidence level for a decision.
#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Very high confidence (>0.95).
    High,
    /// Moderate confidence (0.7–0.95).
    Medium,
    /// Low confidence (<0.7) — may warrant review.
    Low,
}

impl Confidence {
    /// Create from a numeric score (0.0–1.0).
    pub fn from_score(score: f64) -> Self {
        if score >= 0.95 {
            Confidence::High
        } else if score >= 0.7 {
            Confidence::Medium
        } else {
            Confidence::Low
        }
    }

    /// Short human-readable label.
    pub fn label(&self) -> &'static str {
        match self {
            Confidence::High => "high",
            Confidence::Medium => "medium",
            Confidence::Low => "low",
        }
    }
}

/// An alternative that was considered but not chosen.
#[derive(Debug, Clone, Serialize)]
pub struct Alternative {
    /// What this alternative was.
    pub label: String,
    /// Why it was not chosen (brief reason).
    pub reason: String,
    /// Score or metric if available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
}

/// A single decision captured during pipeline execution.
#[derive(Debug, Clone, Serialize)]
pub struct Decision {
    /// Unique identifier (e.g., "d001", auto-assigned).
    pub id: String,
    /// What kind of decision this is.
    pub kind: DecisionKind,
    /// Which pipeline phase produced this decision.
    pub phase: String,
    /// Context: entity/table name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    /// Context: column/field name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<String>,
    /// What was chosen.
    pub chosen: String,
    /// Why this was chosen (brief reasoning).
    pub reason: String,
    /// Confidence level.
    pub confidence: Confidence,
    /// Numeric confidence score (0.0–1.0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence_score: Option<f64>,
    /// Alternatives that were considered.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<Alternative>,
}

/// The complete decision report written at end of run.
#[derive(Debug, Clone, Serialize)]
pub struct DecisionReport {
    /// Pipeline that produced these decisions (e.g., "learn", "generate").
    pub pipeline: String,
    /// Total elapsed time in seconds.
    pub elapsed_secs: f64,
    /// Summary statistics.
    pub summary: ReportSummary,
    /// All decisions, in order of occurrence.
    pub decisions: Vec<Decision>,
}

/// Summary statistics for the report.
#[derive(Debug, Clone, Serialize)]
pub struct ReportSummary {
    /// Total number of decisions recorded.
    pub total_decisions: usize,
    /// Number of high-confidence decisions.
    pub high_confidence: usize,
    /// Number of medium-confidence decisions.
    pub medium_confidence: usize,
    /// Number of low-confidence decisions.
    pub low_confidence: usize,
}

/// Thread-safe collector of decisions during pipeline execution.
#[derive(Clone)]
pub struct DecisionLogger {
    inner: Arc<Mutex<LoggerInner>>,
}

struct LoggerInner {
    decisions: Vec<Decision>,
    counter: usize,
    start: Instant,
}

impl Default for DecisionLogger {
    fn default() -> Self {
        Self::new()
    }
}

impl DecisionLogger {
    /// Create a new decision logger.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(LoggerInner {
                decisions: Vec::new(),
                counter: 0,
                start: Instant::now(),
            })),
        }
    }

    /// Record a decision.
    pub fn record(&self, mut decision: Decision) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.counter += 1;
        decision.id = format!("d{:03}", inner.counter);
        inner.decisions.push(decision);
    }

    /// Build a decision using the fluent builder pattern.
    pub fn builder(&self, kind: DecisionKind) -> DecisionBuilder {
        DecisionBuilder {
            logger: self.clone(),
            kind,
            phase: String::new(),
            entity: None,
            column: None,
            chosen: String::new(),
            reason: String::new(),
            confidence: Confidence::Medium,
            confidence_score: None,
            alternatives: Vec::new(),
        }
    }

    /// Get the number of recorded decisions.
    pub fn count(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .decisions
            .len()
    }

    /// Get all low-confidence decisions for summary display.
    pub fn low_confidence_decisions(&self) -> Vec<Decision> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .decisions
            .iter()
            .filter(|d| d.confidence == Confidence::Low)
            .cloned()
            .collect()
    }

    /// Get all recorded decisions.
    pub fn all_decisions(&self) -> Vec<Decision> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.decisions.clone()
    }

    /// Set entity and column on the most recently logged decision of a given kind.
    ///
    /// Used to enrich decisions logged by lower-level code that lacks context.
    pub fn set_last_context(&self, kind: DecisionKind, entity: &str, column: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(d) = inner.decisions.iter_mut().rev().find(|d| d.kind == kind) {
            d.entity = Some(entity.to_string());
            d.column = Some(column.to_string());
        }
    }

    /// Produce the final report.
    pub fn into_report(self, pipeline: &str) -> DecisionReport {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let elapsed = inner.start.elapsed().as_secs_f64();

        let mut high = 0;
        let mut medium = 0;
        let mut low = 0;
        for d in &inner.decisions {
            match d.confidence {
                Confidence::High => high += 1,
                Confidence::Medium => medium += 1,
                Confidence::Low => low += 1,
            }
        }

        DecisionReport {
            pipeline: pipeline.to_string(),
            elapsed_secs: elapsed,
            summary: ReportSummary {
                total_decisions: inner.decisions.len(),
                high_confidence: high,
                medium_confidence: medium,
                low_confidence: low,
            },
            decisions: inner.decisions.clone(),
        }
    }
}

/// Fluent builder for recording decisions.
pub struct DecisionBuilder {
    logger: DecisionLogger,
    kind: DecisionKind,
    phase: String,
    entity: Option<String>,
    column: Option<String>,
    chosen: String,
    reason: String,
    confidence: Confidence,
    confidence_score: Option<f64>,
    alternatives: Vec<Alternative>,
}

impl DecisionBuilder {
    /// Set the pipeline phase where this decision was made.
    pub fn phase(mut self, phase: impl Into<String>) -> Self {
        self.phase = phase.into();
        self
    }

    /// Set the entity context for this decision.
    pub fn entity(mut self, entity: impl Into<String>) -> Self {
        self.entity = Some(entity.into());
        self
    }

    /// Set the column context for this decision.
    pub fn column(mut self, column: impl Into<String>) -> Self {
        self.column = Some(column.into());
        self
    }

    /// Set the chosen option for this decision.
    pub fn chosen(mut self, chosen: impl Into<String>) -> Self {
        self.chosen = chosen.into();
        self
    }

    /// Set the reason for this decision.
    pub fn reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = reason.into();
        self
    }

    /// Set the confidence level for this decision.
    pub fn confidence(mut self, confidence: Confidence) -> Self {
        self.confidence = confidence;
        self
    }

    /// Set the confidence score (0.0–1.0) and derive confidence level.
    pub fn confidence_score(mut self, score: f64) -> Self {
        self.confidence_score = Some(score);
        self.confidence = Confidence::from_score(score);
        self
    }

    /// Add a considered alternative to this decision.
    pub fn alternative(
        mut self,
        label: impl Into<String>,
        reason: impl Into<String>,
        score: Option<f64>,
    ) -> Self {
        self.alternatives.push(Alternative {
            label: label.into(),
            reason: reason.into(),
            score,
        });
        self
    }

    /// Record this decision into the logger.
    pub fn record(self) {
        let decision = Decision {
            id: String::new(), // Assigned by logger
            kind: self.kind,
            phase: self.phase,
            entity: self.entity,
            column: self.column,
            chosen: self.chosen,
            reason: self.reason,
            confidence: self.confidence,
            confidence_score: self.confidence_score,
            alternatives: self.alternatives,
        };
        self.logger.record(decision);
    }
}

/// Write a decision report to a file as JSON.
pub fn write_report(report: &DecisionReport, path: &std::path::Path) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(report)?;
    std::fs::write(path, json)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decision_logger_basic() {
        let logger = DecisionLogger::new();

        logger
            .builder(DecisionKind::DistributionFit)
            .phase("learn")
            .entity("Users")
            .column("age")
            .chosen("Normal(μ=35.2, σ=12.1)")
            .reason("best KS fit score")
            .confidence_score(0.92)
            .alternative("LogNormal(μ=3.4, σ=0.4)", "higher KS statistic", Some(0.78))
            .alternative("Uniform(18, 65)", "poor fit", Some(0.31))
            .record();

        logger
            .builder(DecisionKind::TypeInference)
            .phase("learn")
            .entity("Users")
            .column("status")
            .chosen("categorical (OneOf)")
            .reason("3 distinct values out of 1000 rows (ratio=0.003)")
            .confidence_score(0.99)
            .record();

        assert_eq!(logger.count(), 2);
        assert_eq!(logger.low_confidence_decisions().len(), 0);

        let report = logger.into_report("learn");
        assert_eq!(report.decisions.len(), 2);
        assert_eq!(report.decisions[0].id, "d001");
        assert_eq!(report.decisions[1].id, "d002");
        assert_eq!(report.summary.high_confidence, 1); // 0.99
        assert_eq!(report.summary.medium_confidence, 1); // 0.92
        assert_eq!(report.summary.low_confidence, 0);
    }

    #[test]
    fn test_confidence_from_score() {
        assert_eq!(Confidence::from_score(0.99), Confidence::High);
        assert_eq!(Confidence::from_score(0.95), Confidence::High);
        assert_eq!(Confidence::from_score(0.85), Confidence::Medium);
        assert_eq!(Confidence::from_score(0.7), Confidence::Medium);
        assert_eq!(Confidence::from_score(0.5), Confidence::Low);
        assert_eq!(Confidence::from_score(0.0), Confidence::Low);
    }

    #[test]
    fn test_low_confidence_filtering() {
        let logger = DecisionLogger::new();

        logger
            .builder(DecisionKind::RelationshipDetection)
            .phase("learn")
            .entity("Orders")
            .chosen("FK to Users.id")
            .reason("name match + 95% overlap")
            .confidence_score(0.6)
            .record();

        logger
            .builder(DecisionKind::GeneratorSelection)
            .phase("learn")
            .entity("Orders")
            .column("amount")
            .chosen("Distribution(LogNormal)")
            .reason("best fit")
            .confidence_score(0.97)
            .record();

        let low = logger.low_confidence_decisions();
        assert_eq!(low.len(), 1);
        assert_eq!(low[0].kind, DecisionKind::RelationshipDetection);
    }

    #[test]
    fn test_report_serialization() {
        let logger = DecisionLogger::new();
        logger
            .builder(DecisionKind::PrimaryKeyType)
            .phase("generate")
            .entity("Users")
            .column("id")
            .chosen("integer sequence")
            .reason("int-like column name, no UUID pattern")
            .confidence(Confidence::High)
            .record();

        let report = logger.into_report("generate");
        let json = serde_json::to_string_pretty(&report).unwrap();
        assert!(json.contains("\"primary_key_type\""));
        assert!(json.contains("\"integer sequence\""));
        assert!(json.contains("\"pipeline\": \"generate\""));
    }
}

//! Interactive review of low-confidence learn decisions.
//!
//! When `knit learn --review` is specified, presents each low/medium-confidence
//! decision to the user for confirmation or override before writing the blueprint.

use std::collections::BTreeMap;
use std::io::{self, BufRead, IsTerminal, Write};

use colored::Colorize;

use crate::core::types::{DataModel, DistributionKind, DistributionSpec, GeneratorSpec};
use crate::decision::{Confidence, Decision, DecisionKind};

/// Outcome of reviewing a single decision.
#[derive(Debug, PartialEq)]
enum ReviewAction {
    /// Keep the current choice.
    Accept,
    /// Switch to an alternative (by 1-based index into `decision.alternatives`).
    ChooseAlternative(usize),
    /// Skip without deciding (keep current).
    Skip,
}

/// Run interactive review of decisions, modifying the model in place.
///
/// Returns the number of decisions that were overridden.
pub fn interactive_review(
    model: &mut DataModel,
    decisions: &[Decision],
    quiet: bool,
) -> usize {
    // Filter to reviewable decisions: low/medium confidence with alternatives
    let reviewable: Vec<&Decision> = decisions
        .iter()
        .filter(|d| {
            d.phase == "learn"
                && !matches!(d.confidence, Confidence::High)
                && !d.alternatives.is_empty()
        })
        .collect();

    if reviewable.is_empty() {
        if !quiet {
            eprintln!(
                "\n{} All decisions are high-confidence — nothing to review.",
                "✓".green().bold()
            );
        }
        return 0;
    }

    // Check if stdin is interactive (TTY)
    if !std::io::stdin().is_terminal() {
        if !quiet {
            eprintln!(
                "\n{} --review requires an interactive terminal (stdin is not a TTY), skipping review.",
                "warning:".yellow().bold()
            );
        }
        return 0;
    }

    if !quiet {
        eprintln!(
            "\n{} {} decision(s) to review (low/medium confidence with alternatives):",
            "review:".cyan().bold(),
            reviewable.len()
        );
        eprintln!("  Enter a number to choose an alternative, 'a' to accept, or 's' to skip.\n");
    }

    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let mut overrides = 0;

    for (i, decision) in reviewable.iter().enumerate() {
        let location = format_location(decision);
        eprintln!(
            "  [{}/{}] {} {} {}",
            i + 1,
            reviewable.len(),
            format!("[{}]", decision.kind.label()).dimmed(),
            location.bold(),
            format!("({})", decision.confidence.label()).yellow(),
        );
        eprintln!("    Current: {}", decision.chosen.green());
        if !decision.reason.is_empty() {
            eprintln!("    Reason:  {}", decision.reason.dimmed());
        }

        // Show alternatives
        for (j, alt) in decision.alternatives.iter().enumerate() {
            let score_str = alt
                .score
                .map(|s| format!(" (score: {:.3})", s))
                .unwrap_or_default();
            // Display label: strip encoded params for clean output
            let display_label = alt.label.split('|').next().unwrap_or(&alt.label);
            eprintln!(
                "    {}) {}{}  — {}",
                j + 1,
                display_label,
                score_str,
                alt.reason.dimmed(),
            );
        }

        let action = prompt_action(&mut reader, decision.alternatives.len());

        match action {
            ReviewAction::Accept => {
                eprintln!("    → {}", "accepted".green());
            }
            ReviewAction::Skip => {
                eprintln!("    → {}", "skipped".dimmed());
            }
            ReviewAction::ChooseAlternative(idx) => {
                let alt = &decision.alternatives[idx];
                eprintln!("    → {} {}", "switched to:".yellow(), alt.label.bold());
                if apply_override(model, decision, &alt.label) {
                    overrides += 1;
                } else {
                    eprintln!(
                        "    {} Could not apply override (field not found in model)",
                        "warning:".yellow().bold()
                    );
                }
            }
        }
        eprintln!();
    }

    if !quiet {
        if overrides > 0 {
            eprintln!(
                "{} {} decision(s) overridden in the blueprint.",
                "✓".green().bold(),
                overrides,
            );
        } else {
            eprintln!(
                "{} No changes made.",
                "info:".cyan().bold(),
            );
        }
    }

    overrides
}

fn format_location(d: &Decision) -> String {
    match (&d.entity, &d.column) {
        (Some(e), Some(c)) => format!("{e}.{c}"),
        (Some(e), None) => e.clone(),
        (None, Some(c)) => c.clone(),
        (None, None) => "—".to_string(),
    }
}

fn prompt_action(reader: &mut impl BufRead, num_alternatives: usize) -> ReviewAction {
    loop {
        eprint!("    choice [a/s/1-{}]: ", num_alternatives);
        io::stderr().flush().ok();

        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line.is_empty() {
            return ReviewAction::Skip;
        }
        let input = line.trim().to_lowercase();

        if input == "a" || input == "accept" || input.is_empty() {
            return ReviewAction::Accept;
        }
        if input == "s" || input == "skip" {
            return ReviewAction::Skip;
        }
        if let Ok(n) = input.parse::<usize>() {
            if n >= 1 && n <= num_alternatives {
                return ReviewAction::ChooseAlternative(n - 1);
            }
        }
        eprintln!(
            "    {} enter 'a' (accept), 's' (skip), or 1-{}",
            "invalid:".red(),
            num_alternatives
        );
    }
}

/// Apply a distribution override to the model.
///
/// Finds the matching entity+column and replaces its generator with the
/// alternative distribution. Returns true if the override was applied.
fn apply_override(model: &mut DataModel, decision: &Decision, alt_label: &str) -> bool {
    match decision.kind {
        DecisionKind::DistributionFit => apply_distribution_override(model, decision, alt_label),
        _ => false, // Other decision types don't support overrides yet
    }
}

/// Parse a distribution name from a decision alternative label and rebuild the
/// generator spec for the matching field.
///
/// The alternative label format is `"name|param1=val1,param2=val2"` (from
/// `Distribution::params_str()`).
fn apply_distribution_override(
    model: &mut DataModel,
    decision: &Decision,
    alt_label: &str,
) -> bool {
    let entity_name = match &decision.entity {
        Some(e) => e,
        None => return false,
    };
    let column_name = match &decision.column {
        Some(c) => c,
        None => return false,
    };

    // Parse "kind|param1=val1,param2=val2"
    let (kind, params) = match parse_alternative_label(alt_label) {
        Some(kp) => kp,
        None => return false,
    };

    // Find the field in the model and update its generator
    for entity in &mut model.entities {
        if entity.name != *entity_name {
            continue;
        }
        for field in &mut entity.fields {
            if field.name != *column_name {
                continue;
            }
            if let Some(GeneratorSpec::Distribution { ref mut spec }) = field.generator {
                let is_integer = spec.round;
                spec.kind = kind;
                spec.params = params;
                spec.round = is_integer;
                return true;
            }
        }
    }
    false
}

/// Parse an alternative label of the form `"kind|param1=val1,param2=val2"`.
fn parse_alternative_label(label: &str) -> Option<(DistributionKind, BTreeMap<String, f64>)> {
    let (name_part, params_part) = label.split_once('|')?;
    let kind = parse_distribution_kind(name_part.trim())?;
    let mut params = BTreeMap::new();
    for kv in params_part.split(',') {
        let (k, v) = kv.split_once('=')?;
        let val: f64 = v.trim().parse().ok()?;
        params.insert(k.trim().to_string(), val);
    }
    Some((kind, params))
}

/// Parse a distribution kind from a label string.
fn parse_distribution_kind(label: &str) -> Option<DistributionKind> {
    let lower = label.to_lowercase();
    // Extract just the distribution name (label may have extra info)
    let name = lower.split('(').next().unwrap_or(&lower).trim();
    match name {
        "normal" => Some(DistributionKind::Normal),
        "log_normal" | "lognormal" => Some(DistributionKind::LogNormal),
        "exponential" => Some(DistributionKind::Exponential),
        "uniform" => Some(DistributionKind::Uniform),
        "poisson" => Some(DistributionKind::Poisson),
        "beta" => Some(DistributionKind::Beta),
        "gamma" => Some(DistributionKind::Gamma),
        "pareto" => Some(DistributionKind::Pareto),
        "zipf" => Some(DistributionKind::Zipf),
        _ => None,
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::{CountSpec, DataType, Entity, Field, NullSpec};
    use crate::decision::Alternative;

    fn make_test_model() -> DataModel {
        let mut params = BTreeMap::new();
        params.insert("mean".into(), 50.0);
        params.insert("std_dev".into(), 10.0);

        let mut model = DataModel::default();
        model.entities.push(Entity {
            name: "Users".into(),
            count: CountSpec::Fixed(100),
            fields: vec![Field {
                name: "age".into(),
                data_type: DataType::Float,
                generator: Some(GeneratorSpec::Distribution {
                    spec: DistributionSpec {
                        kind: DistributionKind::Normal,
                        params,
                        array_params: BTreeMap::new(),
                        round: false,
                    },
                }),
                description: None,
                nullable: NullSpec::default(),
                primary_key: None,
                precision: None,
                actor_column: false,
                fields: vec![],
                stats: None,
                traits: None,
            }],
            description: None,
            tags: vec![],
            constraints: vec![],
            topology: None,
            actor: false,
            persona_distribution: None,
            activity_count: None,
            mixin_refs: None,
            output: None,
            stats: None,
            scaling: None,
        });
        model
    }

    fn make_distribution_decision(entity: &str, column: &str) -> Decision {
        Decision {
            id: "d001".into(),
            kind: DecisionKind::DistributionFit,
            phase: "learn".into(),
            entity: Some(entity.into()),
            column: Some(column.into()),
            chosen: "normal(ks=0.05, aic=120.0)".into(),
            reason: "lowest AIC".into(),
            confidence: Confidence::Medium,
            confidence_score: Some(0.85),
            alternatives: vec![
                Alternative {
                    label: "log_normal|mu=3.4,sigma=0.4".into(),
                    reason: "aic=122.0, ks=0.06".into(),
                    score: Some(0.82),
                },
                Alternative {
                    label: "gamma|shape=2.0,scale=0.5".into(),
                    reason: "aic=125.0, ks=0.08".into(),
                    score: Some(0.78),
                },
            ],
        }
    }

    #[test]
    fn parse_distribution_kinds() {
        assert_eq!(parse_distribution_kind("normal"), Some(DistributionKind::Normal));
        assert_eq!(parse_distribution_kind("log_normal"), Some(DistributionKind::LogNormal));
        assert_eq!(parse_distribution_kind("exponential"), Some(DistributionKind::Exponential));
        assert_eq!(parse_distribution_kind("gamma"), Some(DistributionKind::Gamma));
        assert_eq!(parse_distribution_kind("unknown"), None);
    }

    #[test]
    fn parse_alternative_label_roundtrip() {
        let (kind, params) = parse_alternative_label("log_normal|mu=3.4,sigma=0.4").unwrap();
        assert_eq!(kind, DistributionKind::LogNormal);
        assert_eq!(params.get("mu"), Some(&3.4));
        assert_eq!(params.get("sigma"), Some(&0.4));
    }

    #[test]
    fn parse_alternative_label_invalid() {
        assert!(parse_alternative_label("just_a_name").is_none());
        assert!(parse_alternative_label("normal|bad").is_none());
        assert!(parse_alternative_label("unknown|x=1").is_none());
    }

    #[test]
    fn apply_distribution_override_changes_kind() {
        let mut model = make_test_model();
        let decision = make_distribution_decision("Users", "age");

        assert!(apply_distribution_override(&mut model, &decision, "log_normal|mu=3.4,sigma=0.4"));

        // Verify the field's generator was updated with correct kind and params
        let field = &model.entities[0].fields[0];
        if let Some(GeneratorSpec::Distribution { spec }) = &field.generator {
            assert_eq!(spec.kind, DistributionKind::LogNormal);
            assert_eq!(spec.params.get("mu"), Some(&3.4));
            assert_eq!(spec.params.get("sigma"), Some(&0.4));
        } else {
            panic!("expected Distribution generator");
        }
    }

    #[test]
    fn apply_override_returns_false_for_missing_entity() {
        let mut model = make_test_model();
        let decision = make_distribution_decision("NonExistent", "age");
        assert!(!apply_distribution_override(&mut model, &decision, "gamma|shape=2.0,scale=0.5"));
    }

    #[test]
    fn apply_override_returns_false_for_missing_column() {
        let mut model = make_test_model();
        let decision = make_distribution_decision("Users", "nonexistent");
        assert!(!apply_distribution_override(&mut model, &decision, "gamma|shape=2.0,scale=0.5"));
    }

    #[test]
    fn review_action_from_input() {
        // Test accept
        let mut input = b"a\n".as_slice();
        assert_eq!(prompt_action(&mut input, 3), ReviewAction::Accept);

        // Test empty = accept
        let mut input = b"\n".as_slice();
        assert_eq!(prompt_action(&mut input, 3), ReviewAction::Accept);

        // Test skip
        let mut input = b"s\n".as_slice();
        assert_eq!(prompt_action(&mut input, 3), ReviewAction::Skip);

        // Test numeric choice
        let mut input = b"2\n".as_slice();
        assert_eq!(
            prompt_action(&mut input, 3),
            ReviewAction::ChooseAlternative(1)
        );
    }

    #[test]
    fn interactive_review_no_reviewable_decisions() {
        let mut model = make_test_model();
        let decisions = vec![Decision {
            id: "d001".into(),
            kind: DecisionKind::DistributionFit,
            phase: "learn".into(),
            entity: Some("Users".into()),
            column: Some("age".into()),
            chosen: "normal".into(),
            reason: "best fit".into(),
            confidence: Confidence::High,
            confidence_score: Some(0.98),
            alternatives: vec![],
        }];

        let count = interactive_review(&mut model, &decisions, true);
        assert_eq!(count, 0);
    }
}

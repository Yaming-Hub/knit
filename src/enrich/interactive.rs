//! Interactive confirmation of column mappings for `knit enrich --interactive`.
//!
//! Presents each proposed mapping to the user and lets them accept, reject,
//! or remap columns before enrichment proceeds.

use std::io::{self, BufRead, IsTerminal, Write};

use anyhow::Result;
use colored::Colorize;

use super::mapper::ColumnMapping;

/// Action for a single mapping.
#[derive(Debug, PartialEq)]
enum MappingAction {
    /// Accept this mapping.
    Accept,
    /// Reject this mapping (do not enrich this column).
    Reject,
}

/// Interactively confirm column mappings, returning only accepted ones.
///
/// If stdin is not a TTY, warns and returns all mappings unchanged.
pub fn confirm_mappings(mappings: Vec<ColumnMapping>) -> Result<Vec<ColumnMapping>> {
    if mappings.is_empty() {
        return Ok(mappings);
    }

    if !io::stdin().is_terminal() {
        anyhow::bail!(
            "--interactive requires an interactive terminal (stdin is not a TTY). \
             Remove --interactive to accept all mappings automatically, \
             or use --dry-run to preview mappings."
        );
    }

    let stdin = io::stdin();
    let reader = stdin.lock();
    confirm_mappings_with_reader(mappings, reader)
}

/// Core confirmation logic with injectable reader (for testing).
fn confirm_mappings_with_reader(
    mappings: Vec<ColumnMapping>,
    mut reader: impl BufRead,
) -> Result<Vec<ColumnMapping>> {
    eprintln!(
        "\n{} Review column mappings ({} proposed)\n",
        "▸".cyan().bold(),
        mappings.len()
    );
    eprintln!(
        "  For each mapping: {} accept, {} reject\n",
        "[a/Enter]".bold(),
        "[r]".bold(),
    );

    let mut accepted = Vec::new();
    let mut rejected = 0usize;
    let total = mappings.len();

    for (i, mapping) in mappings.into_iter().enumerate() {
        let conf_pct = format!("{:.0}%", mapping.confidence * 100.0);
        let type_str = if mapping.type_compatible {
            "✓ types match".green().to_string()
        } else {
            "⚠ type mismatch".yellow().to_string()
        };

        let conf_color = if mapping.confidence >= 0.9 {
            conf_pct.green()
        } else if mapping.confidence >= 0.7 {
            conf_pct.yellow()
        } else {
            conf_pct.red()
        };

        eprintln!(
            "  [{}/{}] {} → {}  ({}, {})",
            i + 1,
            total,
            mapping.ref_col_name.cyan(),
            mapping.target_field.green(),
            conf_color,
            type_str,
        );

        let action = prompt_mapping_action(&mut reader)?;

        match action {
            MappingAction::Accept => {
                accepted.push(mapping);
            }
            MappingAction::Reject => {
                rejected += 1;
            }
        }
    }

    eprintln!(
        "\n  {} {} accepted, {} rejected\n",
        "✓".green().bold(),
        accepted.len(),
        rejected,
    );

    Ok(accepted)
}

/// Read a single mapping action from stdin.
fn prompt_mapping_action(reader: &mut impl BufRead) -> Result<MappingAction> {
    loop {
        eprint!("    > ");
        io::stderr().flush()?;

        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            // EOF — treat as reject to avoid silent acceptance
            eprintln!("    (EOF — rejecting remaining mappings)");
            return Ok(MappingAction::Reject);
        }
        let input = line.trim().to_lowercase();

        match input.as_str() {
            "a" | "accept" | "y" | "yes" => return Ok(MappingAction::Accept),
            "" => return Ok(MappingAction::Accept), // Enter = accept
            "r" | "reject" | "n" | "no" => return Ok(MappingAction::Reject),
            _ => {
                eprintln!("    {} enter [a] accept or [r] reject", "?".yellow());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn make_mapping(ref_col: &str, target: &str, confidence: f64) -> ColumnMapping {
        ColumnMapping {
            ref_col_index: 0,
            ref_col_name: ref_col.to_string(),
            target_field: target.to_string(),
            confidence,
            type_compatible: true,
        }
    }

    #[test]
    fn prompt_accept_empty_input() {
        let mut reader = Cursor::new(b"\n");
        let action = prompt_mapping_action(&mut reader).unwrap();
        assert_eq!(action, MappingAction::Accept);
    }

    #[test]
    fn prompt_accept_explicit() {
        let mut reader = Cursor::new(b"a\n");
        let action = prompt_mapping_action(&mut reader).unwrap();
        assert_eq!(action, MappingAction::Accept);
    }

    #[test]
    fn prompt_reject() {
        let mut reader = Cursor::new(b"r\n");
        let action = prompt_mapping_action(&mut reader).unwrap();
        assert_eq!(action, MappingAction::Reject);
    }

    #[test]
    fn prompt_reject_no() {
        let mut reader = Cursor::new(b"no\n");
        let action = prompt_mapping_action(&mut reader).unwrap();
        assert_eq!(action, MappingAction::Reject);
    }

    #[test]
    fn prompt_retry_then_accept() {
        let mut reader = Cursor::new(b"xyz\na\n");
        let action = prompt_mapping_action(&mut reader).unwrap();
        assert_eq!(action, MappingAction::Accept);
    }

    #[test]
    fn prompt_eof_rejects() {
        let mut reader = Cursor::new(b"");
        let action = prompt_mapping_action(&mut reader).unwrap();
        assert_eq!(action, MappingAction::Reject);
    }

    #[test]
    fn confirm_eof_rejects_remaining() {
        let mappings = vec![
            make_mapping("email", "email", 0.95),
            make_mapping("age", "age", 0.8),
            make_mapping("name", "name", 0.9),
        ];
        // Accept first, then EOF — remaining two get rejected
        let reader = Cursor::new(b"a\n");
        let result = confirm_mappings_with_reader(mappings, reader).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].ref_col_name, "email");
    }

    #[test]
    fn empty_mappings_returns_empty() {
        let result = confirm_mappings(vec![]).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn confirm_accept_all() {
        let mappings = vec![
            make_mapping("user_email", "email", 0.95),
            make_mapping("age", "age", 0.85),
        ];
        let reader = Cursor::new(b"a\na\n");
        let result = confirm_mappings_with_reader(mappings, reader).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].ref_col_name, "user_email");
        assert_eq!(result[1].ref_col_name, "age");
    }

    #[test]
    fn confirm_reject_some() {
        let mappings = vec![
            make_mapping("user_email", "email", 0.95),
            make_mapping("foo", "bar", 0.5),
            make_mapping("age", "age", 0.85),
        ];
        let reader = Cursor::new(b"a\nr\na\n");
        let result = confirm_mappings_with_reader(mappings, reader).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].ref_col_name, "user_email");
        assert_eq!(result[1].ref_col_name, "age");
    }

    #[test]
    fn confirm_reject_all() {
        let mappings = vec![
            make_mapping("x", "y", 0.6),
            make_mapping("a", "b", 0.5),
        ];
        let reader = Cursor::new(b"r\nr\n");
        let result = confirm_mappings_with_reader(mappings, reader).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn confirm_preserves_fields() {
        let mut mapping = make_mapping("user_email", "email", 0.85);
        mapping.type_compatible = false;
        mapping.ref_col_index = 3;
        let reader = Cursor::new(b"a\n");
        let result = confirm_mappings_with_reader(vec![mapping], reader).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].ref_col_name, "user_email");
        assert_eq!(result[0].target_field, "email");
        assert_eq!(result[0].confidence, 0.85);
        assert!(!result[0].type_compatible);
        assert_eq!(result[0].ref_col_index, 3);
    }
}


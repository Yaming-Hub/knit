//! `knit learn` — schema inference from existing data (placeholder).
//!
//! This command is wired into the CLI but delegates to `knit-learn`,
//! which is not yet implemented.

use anyhow::Result;
use colored::Colorize;

/// Run the learn command.
///
/// Currently prints a placeholder message. The actual implementation
/// will delegate to the `knit-learn` crate for schema inference.
pub fn run(source: &str) -> Result<()> {
    eprintln!(
        "{} `knit learn` is not yet implemented.",
        "note:".yellow().bold()
    );
    eprintln!(
        "  It will infer a schema from data source: {}",
        source.cyan()
    );
    eprintln!(
        "  See {} for progress.",
        "https://github.com/Yaming-Hub/knit".dimmed()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn learn_placeholder_does_not_error() {
        let result = run("some-file.csv");
        assert!(result.is_ok());
    }
}

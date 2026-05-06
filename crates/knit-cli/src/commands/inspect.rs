//! `knit inspect` — display summary information about a learn state file.
//!
//! This command reads a serialized [`LearnState`] file and prints a human-readable
//! summary: tables, row counts, columns, cardinality estimates, and processing
//! history. Useful for monitoring incremental learning progress without finalizing.

use std::path::Path;

use anyhow::{Context, Result};
use colored::Colorize;
use knit_learn::streaming::state::LearnState;

use crate::Cli;

/// Run the inspect command.
pub fn run(state_path: &str, show_columns: bool, cli: &Cli) -> Result<()> {
    let path = Path::new(state_path);
    let state = LearnState::load(path)
        .map_err(|e| anyhow::anyhow!("{}", e))?
        .with_context(|| format!("state file not found: {}", state_path))?;

    if cli.json {
        print_json(&state, show_columns);
    } else {
        print_human(&state, show_columns);
    }

    Ok(())
}

fn print_json(state: &LearnState, show_columns: bool) {
    let tables: Vec<serde_json::Value> = state
        .tables
        .values()
        .map(|t| {
            let mut obj = serde_json::json!({
                "name": t.name,
                "rows": t.row_count,
                "columns": t.columns.len(),
            });
            if show_columns {
                let cols: Vec<serde_json::Value> = t
                    .columns
                    .iter()
                    .map(|c| {
                        serde_json::json!({
                            "name": c.name,
                            "type": column_type_str(c.data_type),
                            "count": c.count,
                            "null_count": c.null_count,
                            "null_rate": round2(c.null_rate()),
                            "cardinality": c.estimated_cardinality().round() as u64,
                            "top_values": c.top_k.top_items().into_iter().take(5)
                                .map(|(v, n)| serde_json::json!({"value": v, "count": n}))
                                .collect::<Vec<_>>(),
                        })
                    })
                    .collect();
                obj["column_details"] = serde_json::Value::Array(cols);
            }
            obj
        })
        .collect();

    let recent_chunks: Vec<serde_json::Value> = state
        .chunks
        .iter()
        .rev()
        .take(10)
        .map(|c| {
            serde_json::json!({
                "source": c.source,
                "row_count": c.row_count,
                "processed_at": c.processed_at,
            })
        })
        .collect();

    let summary = serde_json::json!({
        "format_version": state.format_version,
        "algorithm_version": state.algorithm_version,
        "seed": state.seed,
        "total_rows": state.total_rows,
        "tables": tables,
        "chunks_processed": state.chunks.len(),
        "recent_chunks": recent_chunks,
        "relationships_detected": state.relationship_evidence.len(),
        "correlations_tracked": state.correlations.len(),
    });

    println!("{}", serde_json::to_string_pretty(&summary).unwrap());
}

fn print_human(state: &LearnState, show_columns: bool) {
    println!(
        "{} (format v{}, algorithm v{}, seed {})",
        "State Summary".bold(),
        state.format_version,
        state.algorithm_version,
        state.seed,
    );
    println!();

    // Overall stats
    println!(
        "  {} rows across {} table(s), {} chunk(s) processed",
        format_count(state.total_rows).cyan(),
        state.tables.len(),
        state.chunks.len(),
    );
    if !state.relationship_evidence.is_empty() {
        println!(
            "  {} FK candidate(s) detected",
            state.relationship_evidence.len(),
        );
    }
    if !state.correlations.is_empty() {
        println!(
            "  {} correlation pair(s) tracked",
            state.correlations.len(),
        );
    }
    println!();

    // Per-table summary
    println!("{}", "Tables:".bold());
    for table in state.tables.values() {
        println!(
            "  {} — {} rows, {} columns",
            table.name.green(),
            format_count(table.row_count),
            table.columns.len(),
        );

        if show_columns {
            for col in &table.columns {
                let cardinality = col.estimated_cardinality().round() as u64;
                let null_pct = col.null_rate() * 100.0;
                println!(
                    "    {} ({:?}) — {} values, ~{} distinct, {:.1}% null",
                    col.name.yellow(),
                    col.data_type,
                    format_count(col.count),
                    format_count(cardinality),
                    null_pct,
                );

                // Show top values
                let top: Vec<_> = col.top_k.top_items().into_iter().take(5).collect();
                if !top.is_empty() {
                    let top_str: Vec<String> = top
                        .iter()
                        .map(|(v, n)| {
                            let display = if v.chars().count() > 20 {
                                let truncated: String = v.chars().take(19).collect();
                                format!("{}…", truncated)
                            } else {
                                v.clone()
                            };
                            format!("\"{}\" ({})", display, n)
                        })
                        .collect();
                    println!("      top: {}", top_str.join(", "));
                }

                // Show numeric stats if present
                if let Some(ref num) = col.numeric {
                    let mean = num.mean();
                    let std = num.std_dev();
                    println!(
                        "      range: [{:.4}, {:.4}], mean: {:.4}, std: {:.4}",
                        num.min(),
                        num.max(),
                        mean,
                        std,
                    );
                }
            }
        }
    }

    // Chunk history (last 10)
    if !state.chunks.is_empty() {
        println!();
        println!("{}", "Recent chunks:".bold());
        let start = state.chunks.len().saturating_sub(10);
        for chunk in &state.chunks[start..] {
            let ts = format_timestamp(chunk.processed_at);
            println!(
                "  {} — {} rows ({})",
                chunk.source,
                format_count(chunk.row_count),
                ts,
            );
        }
        if start > 0 {
            println!("  ... and {} earlier chunk(s)", start);
        }
    }
}

fn format_count(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1_000_000_000.0)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 10_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn format_timestamp(unix_secs: u64) -> String {
    // Simple relative-time display
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if now == 0 || unix_secs == 0 {
        return "unknown".to_string();
    }
    let diff = now.saturating_sub(unix_secs);
    if diff < 60 {
        "just now".to_string()
    } else if diff < 3600 {
        format!("{}m ago", diff / 60)
    } else if diff < 86400 {
        format!("{}h ago", diff / 3600)
    } else {
        format!("{}d ago", diff / 86400)
    }
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn column_type_str(dt: knit_learn::streaming::state::ColumnDataType) -> &'static str {
    use knit_learn::streaming::state::ColumnDataType;
    match dt {
        ColumnDataType::Integer => "integer",
        ColumnDataType::Float => "float",
        ColumnDataType::String => "string",
        ColumnDataType::Temporal => "temporal",
        ColumnDataType::Boolean => "boolean",
        ColumnDataType::Other => "other",
    }
}

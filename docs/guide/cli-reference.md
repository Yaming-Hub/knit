# CLI Command Reference

Complete reference for every `knit` command, subcommand, and flag.

**[← Back to User Guide](index.md)**

---

## Global Options

These flags can be used with any command:

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--seed <N>` | u64 | Blueprint's seed | Override the RNG seed |
| `--format <FMT>` | enum | `parquet` | Output format: `parquet`, `csv`, `json`, `jsonl`, `arrow` |
| `--compression <ALG>` | enum | `snappy` | Compression: `none`, `snappy`, `gzip`, `lz4`, `zstd` |
| `--parallel <N>` | int | auto (CPU count) | Worker thread count (`0` = auto) |
| `--batch-size <N>` | int | `8192` | Rows per Arrow batch |
| `--param key=value` | string | — | Override blueprint parameter (repeatable) |
| `--dry-run` | bool | `false` | Validate and plan only, don't generate |
| `--no-noise` | bool | `false` | Skip noise injection even if blueprint defines profiles |
| `--json` | bool | `false` | Machine-readable JSON output |
| `-q`, `--quiet` | bool | `false` | Suppress all non-error output |
| `-v`, `--verbose` | bool | `false` | Extra diagnostic logging |
| `--count <SPEC>` | string | — | Override row count for `plan`/`generate` (e.g. `1000`, `0.1x`, `10x`) |
| `--log-format <FMT>` | enum | auto | Log format: `text` (terminals), `json` (pipes) |
| `--log-file <PATH>` | string | — | Write all log events to file (always JSON) |
| `--log-filter <DIR>` | string | — | Tracing filter (e.g. `learn=debug,gen=info`) |
| `--decision-report <PATH>` | string | — | Write JSON decision report to file |
| `--version` | — | — | Print version and exit |
| `--help` | — | — | Print help and exit |

---

## `knit validate`

Parse and validate a blueprint file, reporting any errors or warnings.

```bash
knit validate <blueprint-file>
```

### Options

| Flag | Description |
|------|-------------|
| `--json` | Output diagnostics as JSON |
| `--quiet` | Suppress non-error output |

### Examples

```bash
# Validate a blueprint
knit validate my_blueprint.knit.toml

# JSON output for CI pipelines
knit validate my_blueprint.knit.toml --json
```

### Output

**Valid blueprint:**
```
✓ Blueprint is valid (3 entities, 15 fields, 2 relationships)
```

**Invalid blueprint:**
```
error[E0301]: unknown generator type "sequnce"
  --> my_blueprint.knit.toml:12:8
   |
   = help: did you mean "sequence"?

warning[W0102]: field "email" has no uniqueness constraint
  --> my_blueprint.knit.toml:18:1
   |
   = help: consider adding a unique constraint if emails should be distinct
```

**JSON output:**
```json
{
  "valid": false,
  "diagnostics": [
    {
      "severity": "error",
      "code": "E0301",
      "message": "unknown generator type \"sequnce\"",
      "path": "entities[0].fields[2].generator.type",
      "suggestion": "did you mean \"sequence\"?"
    }
  ]
}
```

### Exit Codes

| Code | Meaning |
|------|---------|
| `0` | Blueprint is valid |
| `1` | Blueprint has errors |

---

## `knit plan`

Show the execution plan without generating data. Useful for understanding
entity ordering, parallelism, and estimated output size.

```bash
knit plan <blueprint-file>
```

### Options

| Flag | Description |
|------|-------------|
| `--json` | Output plan as JSON |
| `--parallel <N>` | Simulate with N threads |

### Examples

```bash
# View execution plan
knit plan ecommerce.knit.toml

# JSON plan for scripting
knit plan ecommerce.knit.toml --json
```

### What the Plan Shows

- **Entity ordering** — Topological sort based on FK dependencies
- **Phase breakdown** — Which entities can be generated in parallel
- **Row counts** — Rows per entity, total rows
- **Generator assignments** — Which generator handles each field
- **Estimated sizes** — Approximate output size in bytes
- **RNG tree** — Seed derivation for reproducibility

---

## `knit generate`

The main command — generate synthetic data from a blueprint.

```bash
knit generate <blueprint-file> [OPTIONS]
```

### Options

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--output <DIR>` | `-o` | `output` | Output directory |
| `--format <FMT>` | — | `parquet` | Output format |
| `--compression <ALG>` | — | `snappy` | Compression algorithm |
| `--seed <N>` | — | blueprint seed | Override RNG seed |
| `--parallel <N>` | — | auto | Worker threads |
| `--batch-size <N>` | — | `8192` | Rows per batch |
| `--no-noise` | — | — | Skip noise injection |
| `--json` | — | — | JSON progress events |
| `--quiet` | `-q` | — | Suppress progress bars |

### Examples

```bash
# Basic generation (Parquet output)
knit generate blueprint.knit.toml -o ./data

# CSV with no compression
knit generate blueprint.knit.toml -o ./data --format csv --compression none

# JSON Lines format
knit generate blueprint.knit.toml -o ./data --format jsonl

# Override seed for different data
knit generate blueprint.knit.toml -o ./data --seed 999

# Machine-readable progress for CI
knit generate blueprint.knit.toml -o ./data --json --quiet

# Tune performance
knit generate blueprint.knit.toml -o ./data --parallel 8 --batch-size 16384

# Generate clean data (skip noise profiles defined in blueprint)
knit generate blueprint.knit.toml -o ./clean_data --no-noise
```

### Progress Output

**Terminal mode (default):**
```
customers  [████████████████████████████████] 10,000/10,000  done
orders     [██████████████░░░░░░░░░░░░░░░░░░] 25,430/50,000  12.3k rows/s
reviews    [waiting]
✓ Generated 3 entities (63,000 rows) in 1.4s
```

**JSON mode (`--json`):**
```json
{"event":"entity_start","entity":"customers","count":10000}
{"event":"progress","entity":"customers","rows":10000,"total":10000}
{"event":"entity_done","entity":"customers","duration_ms":42}
{"event":"complete","entities":3,"rows":63000,"duration_ms":1400}
```

### Output Formats

| Format | Extension | Notes |
|--------|-----------|-------|
| `parquet` | `.parquet` | Default. Columnar, compressed, fast reads |
| `csv` | `.csv` | Universal text format |
| `json` | `.json` | One JSON array per entity |
| `jsonl` | `.jsonl` | One JSON object per line (streaming) |
| `arrow` | `.arrow` | Arrow IPC format (zero-copy reads) |

### Noise Injection

If the blueprint defines `[[noise]]` profiles, the `generate` command automatically
applies the noise pipeline after data generation. Use `--no-noise` to produce
clean data from the same blueprint. See the [Noise Guide](noise.md) for details
on configuring noise profiles.

---

## `knit init`

Scaffold a new blueprint file with documented examples.

```bash
knit init [OPTIONS]
```

### Options

| Flag | Short | Description |
|------|-------|-------------|
| `--output <PATH>` | `-o` | Output file path (default: `blueprint.knit.toml`) |

### Example

```bash
# Create a starter blueprint
knit init
knit init -o my_project.knit.toml
```

This creates a documented `blueprint.knit.toml` with an example entity and comments
explaining each generator type.

---

## `knit learn`

Infer a knit blueprint from existing data files or a directory of files.

```bash
knit learn <PATH> [OPTIONS]
```

### Options

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--output <PATH>` | `-o` | `learned.knit.toml` | Output blueprint file or directory path |
| `--model-format <FMT>` | — | auto | Output format: `flat` (single TOML) or `structured` (directory) |
| `--sample <N>` | — | all rows | Limit rows per entity for faster profiling |
| `--state <PATH>` | — | — | State file for incremental learning |
| `--finalize` | — | — | Emit blueprint from existing state without new data |
| `--strict` | — | — | Error on duplicate source paths |
| `--entity <NAME>` | — | all | Learn specific tables only (repeatable) |
| `--actors` | — | — | Enable behavioral analysis |
| `--actor-column <COL>` | — | auto | Specify actor columns explicitly (repeatable) |
| `--personas <N>` | — | auto | Maximum personas to discover |

When `--model-format` is not specified, the format is auto-detected: if the output
path has no file extension (or is an existing directory), structured format is used;
otherwise flat TOML is written.

### Examples

```bash
# Learn from a single CSV file
knit learn data/sales.csv

# Learn from a Parquet file with custom output
knit learn data/events.parquet -o events.knit.toml

# Learn from a directory of files (each file → one entity)
knit learn data/

# Output as structured model directory
knit learn data/ -o my_model/ --model-format structured

# Verbose logging to see analysis details
knit learn data/sales.csv -v

# Incremental learning with state file
knit learn batch1/ --state model.state -o blueprint.toml
knit learn batch2/ --state model.state --finalize -o blueprint.toml
```

### Supported Input Formats

| Format | Extension |
|--------|-----------|
| CSV | `.csv` |
| TSV | `.tsv` |
| Parquet | `.parquet` |
| JSON | `.json` |
| JSON Lines | `.jsonl` |

### What It Does

1. **Ingest** — reads files into Arrow record batches
2. **Profile** — computes statistics for every column
3. **Infer types** — detects semantic types in string columns
4. **Fit distributions** — MLE fitting with KS-test scoring
5. **Detect temporal patterns** — periodicity and schedule detection
6. **Detect relationships** — FK inference via naming and value overlap
7. **Detect correlations** — Pearson, Spearman, and Cramér's V
8. **Assemble blueprint** — writes a Weave TOML with confidence annotations

---

## `knit blueprint` Subcommands

Utilities for manipulating blueprint files.

### `knit blueprint expand`

Flatten an extends chain into a standalone blueprint.

```bash
knit blueprint expand <blueprint-file>
```

Resolves any `extends` directives and prints the fully merged blueprint as TOML
(or JSON with `--json`).

### `knit blueprint normalize`

Resolve and reformat a blueprint to canonical style.

```bash
knit blueprint normalize <blueprint-file>
```

Parses the blueprint (resolving any `extends` chain), then re-serializes it as a
standalone file in a consistent format. Note that inheritance structure is
flattened in the output. Use `--json` for JSON output.

### `knit blueprint diff`

Compare two blueprint files and show differences.

```bash
knit blueprint diff <blueprint-a> <blueprint-b>
```

```bash
# Human-readable diff
knit blueprint diff v1.knit.toml v2.knit.toml

# JSON diff for scripting
knit blueprint diff v1.knit.toml v2.knit.toml --json
```

Output shows added, removed, and changed entities, fields, and relationships.

### `knit blueprint doc`

Generate markdown documentation for a blueprint.

```bash
knit blueprint doc <blueprint-file> [--output <path>]
```

Produces a Markdown document with:
- Model overview table (version, seed, locale, entity/relationship counts)
- Per-entity sections with field tables (type, nullable, generator)
- Relationship table with FK columns

```bash
# Print to stdout
knit blueprint doc ecommerce.knit.toml

# Write to file
knit blueprint doc ecommerce.knit.toml --output docs/blueprint.md
```

---

## `knit completions`

Generate shell completion scripts for tab-completion support.

```bash
knit completions <shell>
```

Supported shells: `bash`, `zsh`, `fish`, `elvish`, `powershell`.

### Examples

```bash
# Bash — add to ~/.bashrc or ~/.bash_completion
knit completions bash >> ~/.bash_completion

# Zsh — place in fpath
knit completions zsh > ~/.zfunc/_knit

# Fish
knit completions fish > ~/.config/fish/completions/knit.fish

# PowerShell — add to $PROFILE
knit completions powershell >> $PROFILE
```

---

## `knit model` Subcommands

Manage and inspect structured model directories.

### `knit model convert`

Convert between flat blueprint files and structured model directories.

```bash
knit model convert <input> <output>
```

| Argument | Description |
|----------|-------------|
| `<input>` | Path to source (flat `.toml` file or structured directory) |
| `<output>` | Path to write converted output |

The direction is auto-detected: if `<input>` is a directory containing `knit.toml`, it converts structured → flat; otherwise flat → structured.

#### Examples

```bash
# Convert flat blueprint to structured directory
knit model convert blueprint.knit.toml my_model/

# Convert structured directory back to flat file
knit model convert my_model/ blueprint_flat.toml
```

### `knit model info`

Display a summary of a model's contents regardless of format.

```bash
knit model info <input>
```

#### Examples

```bash
knit model info my_model/
knit model info blueprint.knit.toml
```

Output includes: model name, seed, locale, entity count with field/row summaries, relationship and correlation counts.

---

## `knit enrich`

Enrich an existing model with statistics and distributions extracted from reference data.

```bash
knit enrich <blueprint> --reference <data-path> [--output <path>]
```

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `<blueprint>` | path | — | Base blueprint to enrich |
| `--reference <path>` | path | — | Reference data file or directory |
| `--output <path>` | path | stdout | Where to write enriched blueprint |
| `--sample <N>` | int | all rows | Limit reference data rows for faster profiling |

The enrich command maps reference columns to blueprint fields by name similarity and type compatibility, then merges extracted distributions into the model using Bayesian update rules.

#### Examples

```bash
# Enrich blueprint with real-world sample data
knit enrich blueprint.knit.toml --reference samples/ --output enriched.toml

# Enrich with row limit for large datasets
knit enrich blueprint.knit.toml --reference big_data.parquet --sample 10000
```

---

## `knit scale`

Analyze and adjust row counts for scaling a dataset up or down.

```bash
knit scale <blueprint> <factor> [--output <path>]
```

| Argument/Option | Type | Default | Description |
|-----------------|------|---------|-------------|
| `<blueprint>` | path | — | Blueprint file or structured model directory |
| `<factor>` | string | — | Scale factor: `2x`, `0.5x`, `1000` (absolute), or `+500` (delta) |
| `--output <path>` | path | stdout | Where to write scaled blueprint |
| `--dimension <dim>` | string | — | Scale along a specific dimension (e.g., `location`, `time`) |
| `--dry-run` | bool | `false` | Show plan without writing |

#### Examples

```bash
# Double all entity counts
knit scale blueprint.knit.toml 2x --output scaled.toml

# Scale to absolute count
knit scale blueprint.knit.toml 10000 --output big.toml
```

---

## `knit tokenize`

Replace string content with tokens for privacy-safe troubleshooting.

```bash
knit tokenize <input-dir> --output <output-dir> [--dictionary <path>]
```

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `<input-dir>` | path | — | Directory containing dataset files |
| `--output <dir>` | path | — | Where to write tokenized dataset |
| `--dictionary <path>` | path | `<output>/dictionary.json` | Token dictionary output path |
| `--restore` | bool | `false` | Restore tokenized data using dictionary |

The tokenize command scans all string values, builds a reversible token mapping, and replaces content while preserving dataset structure and relationships. Blueprint files, dictionary files, and other non-content files are handled appropriately.

#### Examples

```bash
# Tokenize a dataset for sharing
knit tokenize my_dataset/ --output tokenized/

# Restore from tokenized data (requires dictionary)
knit tokenize tokenized/ --output restored/ --restore --dictionary tokenized/dictionary.json
```

---

## Common Recipes

### Generate Multiple Formats

```bash
for fmt in parquet csv json; do
  knit generate blueprint.knit.toml -o "./data/$fmt" --format $fmt
done
```

### CI/CD Validation

```bash
# In your CI pipeline
knit validate blueprint.knit.toml --json --quiet
if [ $? -ne 0 ]; then
  echo "Blueprint validation failed"
  exit 1
fi
```

### Deterministic Test Fixtures

```bash
# Always produces identical output
knit generate fixtures.knit.toml -o ./test/fixtures --seed 12345 --quiet
```

### Large-Scale Generation

```bash
# Tune for throughput on large datasets
knit generate big_blueprint.knit.toml \
  -o ./data \
  --parallel 16 \
  --batch-size 131072 \
  --compression zstd \
  --quiet
```

### Blueprint Evolution Workflow

```bash
# Check what changed between versions
knit blueprint diff v1.knit.toml v2.knit.toml
```

---

## What's Next?

- **[Getting Started](getting-started.md)** — First-time setup tutorial
- **[Blueprint Language Tutorial](blueprint-language.md)** — Write blueprints from scratch
- **[Examples Walkthrough](examples.md)** — Real-world blueprint examples

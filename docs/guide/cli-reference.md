# CLI Command Reference

Complete reference for every `knit` command, subcommand, and flag.

**[← Back to User Guide](index.md)**

---

## Global Options

These flags can be used with any command:

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--seed <N>` | u64 | Schema's seed | Override the RNG seed |
| `--format <FMT>` | enum | `parquet` | Output format: `parquet`, `csv`, `json`, `jsonl`, `arrow` |
| `--compression <ALG>` | enum | `snappy` | Compression: `none`, `snappy`, `gzip`, `lz4`, `zstd` |
| `--parallel <N>` | int | auto (CPU count) | Worker thread count (`0` = auto) |
| `--batch-size <N>` | int | `8192` | Rows per Arrow batch |
| `--param key=value` | string | — | Override schema parameter (repeatable) |
| `--dry-run` | bool | `false` | Validate and plan only, don't generate |
| `--json` | bool | `false` | Machine-readable JSON output |
| `-q`, `--quiet` | bool | `false` | Suppress all non-error output |
| `-v`, `--verbose` | bool | `false` | Extra diagnostic logging |
| `--version` | — | — | Print version and exit |
| `--help` | — | — | Print help and exit |

---

## `knit validate`

Parse and validate a schema file, reporting any errors or warnings.

```bash
knit validate <schema-file>
```

### Options

| Flag | Description |
|------|-------------|
| `--json` | Output diagnostics as JSON |
| `--quiet` | Suppress non-error output |

### Examples

```bash
# Validate a schema
knit validate my_schema.weave.toml

# JSON output for CI pipelines
knit validate my_schema.weave.toml --json
```

### Output

**Valid schema:**
```
✓ Schema is valid (3 entities, 15 fields, 2 relationships)
```

**Invalid schema:**
```
error[E0301]: unknown generator type "sequnce"
  --> my_schema.weave.toml:12:8
   |
   = help: did you mean "sequence"?

warning[W0102]: field "email" has no uniqueness constraint
  --> my_schema.weave.toml:18:1
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
| `0` | Schema is valid |
| `1` | Schema has errors |

---

## `knit plan`

Show the execution plan without generating data. Useful for understanding
entity ordering, parallelism, and estimated output size.

```bash
knit plan <schema-file>
```

### Options

| Flag | Description |
|------|-------------|
| `--json` | Output plan as JSON |
| `--parallel <N>` | Simulate with N threads |

### Examples

```bash
# View execution plan
knit plan ecommerce.weave.toml

# JSON plan for scripting
knit plan ecommerce.weave.toml --json
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

The main command — generate synthetic data from a schema.

```bash
knit generate <schema-file> [OPTIONS]
```

### Options

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--output <DIR>` | `-o` | `output` | Output directory |
| `--format <FMT>` | — | `parquet` | Output format |
| `--compression <ALG>` | — | `snappy` | Compression algorithm |
| `--seed <N>` | — | schema seed | Override RNG seed |
| `--parallel <N>` | — | auto | Worker threads |
| `--batch-size <N>` | — | `8192` | Rows per batch |
| `--json` | — | — | JSON progress events |
| `--quiet` | `-q` | — | Suppress progress bars |

### Examples

```bash
# Basic generation (Parquet output)
knit generate schema.weave.toml -o ./data

# CSV with no compression
knit generate schema.weave.toml -o ./data --format csv --compression none

# JSON Lines format
knit generate schema.weave.toml -o ./data --format jsonl

# Override seed for different data
knit generate schema.weave.toml -o ./data --seed 999

# Machine-readable progress for CI
knit generate schema.weave.toml -o ./data --json --quiet

# Tune performance
knit generate schema.weave.toml -o ./data --parallel 8 --batch-size 16384
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

---

## `knit init`

Scaffold a new schema file with documented examples.

```bash
knit init [OPTIONS]
```

### Options

| Flag | Short | Description |
|------|-------|-------------|
| `--output <PATH>` | `-o` | Output file path (default: `.weave.toml`) |

### Example

```bash
# Create a starter schema
knit init -o my_project.weave.toml
```

This creates a documented `.weave.toml` with an example entity and comments
explaining each generator type.

---

## `knit learn`

Infer a Weave schema from existing data files or a directory of files.

```bash
knit learn <PATH> [OPTIONS]
```

### Options

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--output <PATH>` | `-o` | `learned.weave.toml` | Output schema file path |

### Examples

```bash
# Learn from a single CSV file
knit learn data/sales.csv

# Learn from a Parquet file with custom output
knit learn data/events.parquet -o events.weave.toml

# Learn from a directory of files (each file → one entity)
knit learn data/

# Verbose logging to see analysis details
knit learn data/sales.csv -v
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
8. **Assemble schema** — writes a Weave TOML with confidence annotations

---

## `knit schema` Subcommands

Utilities for manipulating schema files.

### `knit schema diff`

Compare two schema files and show differences.

```bash
knit schema diff <schema-a> <schema-b>
```

```bash
# Human-readable diff
knit schema diff v1.weave.toml v2.weave.toml

# JSON diff for scripting
knit schema diff v1.weave.toml v2.weave.toml --json
```

Output shows added, removed, and changed entities, fields, and relationships.

---

## Common Recipes

### Generate Multiple Formats

```bash
for fmt in parquet csv json; do
  knit generate schema.weave.toml -o "./data/$fmt" --format $fmt
done
```

### CI/CD Validation

```bash
# In your CI pipeline
knit validate schema.weave.toml --json --quiet
if [ $? -ne 0 ]; then
  echo "Schema validation failed"
  exit 1
fi
```

### Deterministic Test Fixtures

```bash
# Always produces identical output
knit generate fixtures.weave.toml -o ./test/fixtures --seed 12345 --quiet
```

### Large-Scale Generation

```bash
# Tune for throughput on large datasets
knit generate big_schema.weave.toml \
  -o ./data \
  --parallel 16 \
  --batch-size 131072 \
  --compression zstd \
  --quiet
```

### Schema Evolution Workflow

```bash
# Check what changed between versions
knit schema diff v1.weave.toml v2.weave.toml
```

---

## What's Next?

- **[Getting Started](getting-started.md)** — First-time setup tutorial
- **[Schema Language Tutorial](schema-language.md)** — Write schemas from scratch
- **[Examples Walkthrough](examples.md)** — Real-world schema examples

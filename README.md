# Knit

**High-performance synthetic data generation toolkit.**

Knit generates realistic, schema-driven synthetic datasets at scale. Define your
data model in a declarative TOML or JSON schema, and Knit handles execution
planning, deterministic generation, output formatting, and optional noise
injection — all from a single CLI command.

[![Rust](https://img.shields.io/badge/Rust-1.87%2B-orange)](https://www.rust-lang.org)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![crates.io](https://img.shields.io/crates/v/knit.svg)](https://crates.io/crates/knit)

---

## Installation

```bash
cargo install knit
```

Or build from source:

```bash
git clone https://github.com/Yaming-Hub/knit.git
cd knit
cargo build --release
```

## Features

- **Declarative schema language** — Define entities, fields, generators, and
  relationships in `.weave.toml` files with inheritance via `extends` and
  modular composition via `include`.
- **Rich generator library** — Sequences, distributions (normal, uniform,
  Pareto, Zipf, …), patterns, UUIDs, one-of, derived expressions, temporal
  generators, correlated fields, and graph topologies.
- **Deterministic output** — Seeded RNG tree ensures identical datasets across
  runs for any given seed.
- **Multiple output formats** — Parquet, CSV, JSON, JSONL, and Arrow IPC with
  configurable compression (Snappy, LZ4, Zstd).
- **Noise injection** — Post-generation perturbation pipeline with 7 built-in
  perturbators (typos, null injection, outliers, drift, swap, truncation, format
  variation).
- **Reverse engineering** — Ingest existing data, profile distributions, and fit
  schemas automatically (`knit learn`).
- **Behavioral modeling** — Define actor personas with trait distributions,
  activity-driven row counts, temporal biases, social graphs, and conversation
  threading. Learn behavioral patterns from existing data with `knit learn --actors`.
- **Incremental learning** — Process datasets larger than memory in bounded
  chunks with streaming statistics and persistent state files.
- **Dictionary extraction** — Automatically extracts domain-specific vocabularies
  from eligible high-cardinality string columns for realistic text generation.
- **Foreign-key integrity** — Automatic topological ordering and key stores
  ensure referential integrity across entities.
- **Plugin architecture** — Register custom generators at runtime via the
  `GeneratorPlugin` trait.
- **Scalable** — Batch-oriented Arrow columnar engine with Rayon parallelism;
  sampled key stores for 100M+ row entities.

## Architecture

```mermaid
graph LR
    A[Schema TOML/JSON] -->|schema| B[DataModel]
    B -->|plan| C[ExecutionPlan]
    C -->|gen| D[RecordBatches]
    D -->|noise| E[Perturbed Batches]
    E -->|bind| F[Parquet / CSV / JSON / Arrow]
    G[learn] -->|ingest + profile| B
    H[cli] --> A
```

## Quick Start

### Install

```bash
# Clone and build
git clone https://github.com/Yaming-Hub/knit.git
cd knit
cargo build --release

# The binary is at target/release/knit
```

### Create a Schema

Create a file called `demo.weave.toml`:

```toml
schema_version = "1.0"

[model]
name = "demo"
seed = 42

[[entities]]
name = "users"
count = 10000

[[entities.fields]]
name = "id"
data_type = "int"
primary_key = true
[entities.fields.generator]
type = "sequence"
start = 1
step = 1

[[entities.fields]]
name = "email"
data_type = "string"
[entities.fields.generator]
type = "pattern"
pattern = "user####@example.com"

[[entities.fields]]
name = "age"
data_type = "int"
[entities.fields.generator]
type = "distribution"
kind = "normal"
[entities.fields.generator.params]
mean = 35.0
std_dev = 12.0
```

### Generate

```bash
# Validate schema
knit validate demo.weave.toml

# Preview execution plan
knit plan demo.weave.toml

# Generate data (default: Parquet)
knit generate demo.weave.toml -o ./data

# Generate as CSV with a specific seed
knit generate demo.weave.toml --format csv --seed 123 -o ./data

# Dry run — validate and plan without generating
knit generate demo.weave.toml --dry-run
```

### Schema Composition

Build large schemas from reusable fragments using `include`:

```toml
# main.weave.toml
include = ["users.weave.toml", "products.weave.toml"]

[model]
name = "my_project"
seed = 42

# Add entities specific to this schema
[[entities]]
name = "orders"
count = 5000
# ...

[[relationships]]
name = "orders_to_users"
from = "orders"
to = "users"
kind = "many_to_one"
```

**Rules:**
- Fragments define entities, relationships, personas, etc. — but no `[model]` section
- Name conflicts between included fragments are errors; the main schema silently overrides
- Includes are recursive and diamond-safe (each file loaded at most once)
- Security: absolute paths and `..` traversal are rejected

See `examples/modular/` for a working example.

## Module Structure

Knit is published as a single crate. Internally it is organized into modules:

| Module | Description |
|---|---|
| `knit::core` | Shared types: `DataModel`, `Entity`, `Field`, `Value`, `GeneratorSpec` |
| `knit::schema` | TOML/JSON parsing, validation, schema inheritance (`extends`) |
| `knit::plan` | Compiles a `DataModel` into an `ExecutionPlan` with RNG tree |
| `knit::gen` | Generation engine: executes plans → Arrow `RecordBatch`es |
| `knit::noise` | Post-generation perturbation pipeline (7 perturbators) |
| `knit::bind` | Output sinks: Parquet, CSV, JSON, JSONL, Arrow IPC |
| `knit::learn` | Data ingestion, profiling, distribution fitting, schema inference, behavioral persona discovery |
| `knit::cli` | Binary commands: `validate`, `plan`, `generate`, `schema`, `init`, `learn`, `inspect`, `completions`, `generators` |

## Examples

The `examples/` directory contains sample schemas:

- `ecommerce.weave.toml` — Users, products, orders, reviews with FK relationships
- `ecommerce_behavioral.weave.toml` — Persona-driven purchasing: 4 customer
  segments, activity-driven orders, temporal shopping biases, review threading
- `email_traffic.weave.toml` — Email messaging with sender/receiver personas
- `financial.weave.toml` — Accounts and transactions with risk scoring
- `hr_org.weave.toml` — Employees with behavioral personas, activity-driven tasks,
  manager hierarchy, and work-hour temporal biases
- `iot_sensors.weave.toml` — Devices, sensor readings, and alerts with FK chains
- `server_logs.weave.toml` — Servers, HTTP requests, and error logs
- `social_platform.weave.toml` — Social network with actor graphs, persona-driven
  temporal patterns, burst sessions, and posts/comments/DMs
- `modular/` — Modular composition example: `users.weave.toml` and
  `products.weave.toml` fragments composed via `include` in `ecommerce.weave.toml`
- `cli_test.weave.toml` — Minimal schema for integration testing

Generate all examples:

```bash
for schema in examples/*.weave.toml; do
  knit generate "$schema" -o data/$(basename "$schema" .weave.toml) --format csv
done
```

### Reverse Engineering

Infer a schema from existing data and re-generate:

```bash
# Learn a schema from CSV files
knit learn ./my-data/ -o inferred.weave.toml

# Learn from a large dataset (sample first 10k rows per table)
knit learn ./big-data/ -o inferred.weave.toml --sample 10000

# Review and customize the inferred schema, then generate
knit generate inferred.weave.toml -o ./synthetic-data --format parquet
```

### Incremental Learning

For datasets too large to fit in memory, use incremental mode to process data
in chunks. Each invocation updates a persistent state file:

```bash
# Process data in batches — each call updates the state file
knit learn ./chunk1/ --state learn.state
knit learn ./chunk2/ --state learn.state
knit learn ./chunk3/ --state learn.state

# Finalize: emit schema from accumulated statistics
knit learn --state learn.state --finalize -o schema.weave.toml
```

Incremental mode uses streaming algorithms (Welford for mean/variance,
HyperLogLog for cardinality, reservoir sampling for distribution fitting) so
memory usage stays bounded regardless of dataset size.

### Dictionary Extraction

When learning from data, Knit automatically extracts domain-specific
dictionaries for eligible high-cardinality string columns (e.g., product names,
person names) that don't match a standard faker pattern:

```bash
knit learn ./products/ -o schema.weave.toml
# Creates: schema.weave.toml + products_name.dict.txt (alongside the schema)
```

The learned schema references the dictionary file, and generation draws values
from it — producing output that matches the domain vocabulary of the original
data. Dictionary extraction works in both batch and incremental modes.
Extracted dictionaries are capped at ~10,000 entries for large vocabularies.

### Behavioral Modeling

Define actor personas with distinct behavioral traits to generate data with
realistic human-like patterns:

```toml
# Define behavioral segments
[[personas]]
name = "power_user"
weight = 0.15
[personas.traits]
activity_rate = 20.0    # events/month
peak_hours = 9.0        # preferred hour of day

[[personas]]
name = "casual_user"
weight = 0.85
[personas.traits]
activity_rate = 3.0
peak_hours = 20.0

# Mark an entity as an actor with persona assignment
[[entities]]
name = "users"
count = 1000
actor = true
persona_distribution = "personas"

# Activity-driven row counts (total rows = sum of per-actor trait values)
[[entities]]
name = "events"
count = 5000  # fallback estimate
[entities.activity_count]
actor_field = "user_id"
trait = "activity_rate"

# FK field linking events to their actor
[[entities.fields]]
name = "user_id"
data_type = "int"
actor_column = true

# Temporal bias — timestamps cluster around each actor's peak_hours
[[entities.fields]]
name = "created_at"
data_type = "datetime"
[entities.fields.generator]
type = "actor_temporal"
trait = "peak_hours"

# Relationship required for activity_count resolution
[[relationships]]
name = "event_user"
from = "events"
to = "users"
kind = "many_to_one"
foreign_key = "user_id"
```

Learn behavioral patterns from existing data:

```bash
# Infer personas and actor relationships from data
knit learn ./my-data/ --actors -o behavioral.weave.toml

# Inspect discovered behavioral structure
knit inspect behavioral.weave.toml --actors

# Generate with persona-driven realism
knit generate behavioral.weave.toml -o ./synthetic
```

See `examples/social_platform.weave.toml` and `examples/ecommerce_behavioral.weave.toml`
for complete behavioral schemas.

### Parameterized Schemas

Derived expressions can reference `--param` values using `${param.key}` syntax:

```toml
[[entities.fields]]
name = "email"
data_type = "string"
[entities.fields.generator]
type = "derived"
expr = "${name}@${param.domain}"
depends_on = ["name"]
```

```bash
knit generate schema.weave.toml -o out/ --param domain=example.com
```

Unresolved params stay as literal `${param.key}` in the output.

## CLI Reference

```
knit [OPTIONS] <COMMAND>

Commands:
  validate     Parse and validate a schema file
  plan         Show execution plan (dry run)
  generate     Generate synthetic data
  schema       Schema manipulation (expand, normalize, diff)
  init         Create a starter schema
  learn        Infer schema from data
  inspect      Inspect state files or schema summaries
  generators   List available generator types
  completions  Generate shell completions

Global options:
  --seed <N>            Override schema seed
  --format <FMT>        Output format (parquet|csv|json|jsonl|arrow)
  --compression <ALG>   Compression (none|snappy|gzip|lz4|zstd)
  --parallel <N>        Worker threads (0 = auto)
  --batch-size <N>      Rows per batch (default: 8192)
  --count <N|Nx>        Override row count (absolute or multiplier, e.g. 100, 0.1x, 10x)
  --param key=value     Override schema parameter (repeatable)
  --json                Machine-readable JSON output
  --dry-run             Validate and plan only
  --no-noise            Skip noise injection
  -q, --quiet           Suppress non-error output
  -v, --verbose         Debug logging
  --version             Show version

Learn-specific options:
  --sample <N>          Limit rows per table (faster profiling on large data)
  --state <PATH>        Incremental mode: persist statistics to a state file
  --finalize            Emit schema from state without processing new data
  --strict              Error on reprocessing same source into same state (default: warn)
  --actors              Enable behavioral modeling (persona discovery, actor graphs)

Inspect options:
  --actors              Show behavioral summary (personas, relationships, generators)
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for build instructions, coding
conventions, and PR guidelines.

## License

This project is licensed under the MIT License — see [LICENSE](LICENSE) for
details.

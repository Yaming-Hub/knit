# Changelog

All notable changes to Knit are documented in this file.

## [Unreleased]

## [0.2.0] — 2026-05-07

### Added

- **Behavioral modeling** — Full human behavior simulation pipeline:
  - **Personas** — Define actor segments with weighted trait distributions
    (activity rates, temporal preferences, spending patterns)
  - **Activity-driven rows** — Entity row counts determined by per-actor trait
    sums instead of fixed counts (`activity_count`)
  - **Actor temporal** — Timestamp generation biased toward persona peak hours
    with burst/session clustering and inter-event gap enforcement
  - **Actor relationships** — Graph-based relationships between actors
    (scale-free, small-world, hierarchical, Erdős–Rényi topologies)
  - **Relationship ref** — FK generation following graph edges instead of
    uniform sampling
  - **Persona field** — Field values drawn from persona trait distributions
  - **Thread ref** — Self-referential conversation threading with configurable
    reply probability, depth limits, and recency-weighted reply targets
  - **Cross-entity causal ordering** — `temporal_after` ensures child entity
    timestamps follow parent entity events
  - **Actor identity resolution** — Cross-entity actor unification via FK-based
    and name-based namespace inference (`knit learn --actors`)
- **Learn behavioral patterns** — `knit learn --actors` detects actor columns,
  profiles per-actor features, clusters into personas, and discovers actor
  relationship graphs
- **Inspect behavioral summary** — `knit inspect <schema> --actors` shows
  persona groups, actor relationships, and behavioral generators
- **Behavioral CLI flags** — `--actor-column` for explicit actor column
  specification, `--personas` for persona count control in `knit learn`
- **Behavioral output in validate/plan** — `knit validate` and `knit plan`
  now display persona counts, actor relationships, and behavioral generator
  summaries
- **Behavioral examples** — Four example schemas exercising behavioral modeling:
  `social_platform`, `ecommerce_behavioral`, `email_traffic`, `hr_org`
- **GitHub Actions CI** — Automated check, test, clippy, fmt, and doc jobs

### Changed

- README updated with behavioral modeling documentation, examples, and CLI
  reference for `--actors` flag
- Design doc §12.3 example filenames corrected to match actual files
- Applied `cargo fmt` across entire workspace

### Fixed

- All clippy warnings resolved across workspace
- Ignored doc tests converted to compilable `no_run` examples (graph, plugin,
  pipeline)
- Fixed 6 broken rustdoc links (types.rs, actor_pool.rs, string_fk.rs,
  temporal_store.rs, clustering.rs, learn.rs)

## [0.1.0] — 2026-05-02

Initial release with core synthetic data generation capabilities.

### Core

- **Declarative schema language** — `.weave.toml` and `.weave.json` formats
  with `extends` inheritance
- **Schema validation** — Type compatibility checks, FK/PK constraint
  verification, generator parameter validation
- **Execution planner** — Topological ordering, RNG tree seeding, batch
  partitioning with parallelism
- **Deterministic generation** — Seeded RNG tree ensures reproducible output
  across runs

### Generators

- Sequence, UUID, pattern, one-of (weighted), distribution (normal, uniform,
  log-normal, Pareto, Zipf, beta, exponential, Poisson, Bernoulli, binomial),
  faker (20+ methods), dictionary (sample/combinatorial/suffix expansion),
  derived expressions, temporal (business hours, relative), correlation,
  conditional, graph topology (scale-free, small-world, Erdős–Rényi), unique
  wrapper, constant

### Output

- **Formats** — Parquet, CSV, JSON, JSONL, Arrow IPC
- **Compression** — Snappy, LZ4, Zstd
- **Template engine** — MiniJinja for custom output formats

### Noise Pipeline

- 7 built-in perturbators: Gaussian noise, null injection, typo injection,
  outlier injection, value drift, field swap/truncation, format corruption
- Three-stage pipeline (clean → constrained → breaking)
- Per-perturbator rate configuration

### Reverse Engineering

- `knit learn` — Ingest CSV/JSON/Parquet, profile distributions, fit schemas
- Incremental learning with streaming statistics and persistent state files
- Dictionary extraction from high-cardinality string columns
- Progress feedback during ingestion

### CLI

- Commands: `validate`, `plan`, `generate`, `schema` (expand/normalize/diff/doc),
  `init`, `learn`, `inspect`, `generators`, `completions`
- Global flags: `--seed`, `--format`, `--compression`, `--parallel`,
  `--batch-size`, `--count`, `--param`, `--json`, `--dry-run`, `--no-noise`,
  `--quiet`, `--verbose`
- Shell completion generation (bash, zsh, fish, PowerShell)

### Architecture

- 8-crate workspace: knit-core, knit-schema, knit-plan, knit-gen, knit-noise,
  knit-bind, knit-learn, knit-cli
- Batch-oriented Arrow columnar engine with Rayon parallelism
- Sampled key stores for 100M+ row entities
- Plugin architecture for custom generators

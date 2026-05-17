# Knit — Architecture Document

**Version:** 0.4.0
**Status:** Implemented

---

## 1. Overview

Knit is a high-performance Rust toolset for generating large synthetic datasets
(100GB+ in hours). It combines a declarative blueprint language (**Knit**) with a
multi-stage pipeline that compiles, generates, perturbs, and serializes data.

```mermaid
flowchart TB
    user([User / AI Agent]) --> blueprint([knit blueprint\n.knit.toml / .knit.json])
    blueprint --> learn[knit learn\nReverse engineer]
    blueprint --> generate[knit generate\nForward pipeline]
    blueprint --> validate[knit validate\nBlueprint checking]
    learn --> inferred([knit blueprint\ninferred])
    generate --> output([Output Dataset\nParquet / JSON / CSV / custom])
```

---

## 2. The Knit Language

Knit is the declarative blueprint language at the center of the project. A Knit document
describes **what** data looks like — its structure, statistical properties,
relationships, and quality characteristics — without specifying **how** to generate it.

**Specification:** [`docs/knit-spec.md`](knit-spec.md)

### Role in the Architecture

```mermaid
flowchart LR
    doc([Knit Document]) --> model[DataModel\nin-memory AST] --> stages[Pipeline stages]
```

Knit serves as:
- **Input** to the forward pipeline (blueprint → data)
- **Output** of the reverse pipeline (data → blueprint)
- **Contract** between the user/AI and the engine
- **Portable artifact** that can be versioned, shared, and composed

### Key Language Capabilities

| Capability | Purpose |
|-----------|---------|
| Entities & fields | Define tables and columns with types |
| Generators | 15 generator types for value production |
| Distributions | 17+ statistical distributions with clamping |
| Temporal types | `date`, `time`, `datetime`, `datetimetz`, `duration` with timezone support |
| Relationships | Foreign keys with cardinality distributions and graph topologies |
| Correlations | Cross-field statistical dependencies (copula, conditional) |
| Time series | Trend, seasonality, AR, spikes for temporal data |
| Noise profiles | Invariant-aware perturbation specifications |
| Composition | `extends`, `includes`, `mixins`, `params` for reuse and AI-driven modification |
| Custom types | Reusable domain type bundles |
| **Personas** | Actor behavioral profiles for human-like data generation |
| **Actor relationships** | Graph-based modeling of inter-person connections |

---

## 3. Pipeline Architecture

### 3.1 Forward Pipeline (Blueprint → Data)

The forward pipeline transforms a Knit document into output files through five stages:

```mermaid
flowchart LR
    blueprint([.knit.toml]) --> parse[Parse]
    parse --> validate[Validate]
    validate --> plan[Plan]
    plan --> gen[Generate]
    gen --> perturb[Perturb]
    perturb --> bind[Bind]
    bind --> output([Output Files])
```

| Stage | Input | Output | Module |
|-------|-------|--------|--------|
| **Parse** | `.knit.toml` / `.knit.json` | `DataModel` (AST) | `blueprint` |
| **Validate** | `DataModel` | Validated `DataModel` + diagnostics | `blueprint` |
| **Plan** | Validated `DataModel` | `ExecutionPlan` | `plan` |
| **Generate** | `ExecutionPlan` | `RecordBatch` stream | `gen` |
| **Perturb** | `RecordBatch` stream | Perturbed `RecordBatch` stream | `noise` |
| **Bind** | `RecordBatch` stream | Output files (Parquet, JSON, CSV, etc.) | `bind` |

### 3.2 Reverse Pipeline (Data → Blueprint)

The reverse pipeline infers a Knit document from an existing dataset:

```mermaid
flowchart LR
    data([Existing Dataset]) --> ingest[Ingest]
    ingest --> infer[Infer]
    infer --> score[Score]
    score --> emit[Emit]
    emit --> blueprint([Candidate Blueprint])
```

| Stage | Description | Module |
|-------|-------------|--------|
| **Ingest** | Read CSV / Parquet / JSON via Arrow readers | `learn` |
| **Infer** | Type detection, distribution fitting, FK discovery | `learn` |
| **Score** | Confidence scoring for every inferred element | `learn` |
| **Emit** | Output a candidate `DataModel` for human/AI review | `learn` |

---

## 4. Module Map

Knit is a single Cargo crate with a library (`src/lib.rs`) and a binary
(`src/main.rs`). Internally it is organized into focused modules:

```
knit/
├── src/
│   ├── lib.rs           Crate root (re-exports all modules)
│   ├── main.rs          CLI binary entry point
│   ├── core/            Semantic data model types (Value, DataModel, Entity, Field, …)
│   ├── blueprint/       Knit blueprint parser (TOML + JSON) and validator
│   ├── plan/            Execution planner / compiler
│   ├── gen/             Data generation engine
│   ├── noise/           Perturbation pipeline
│   ├── bind/            Output serialization (sinks + templates)
│   ├── learn/           Blueprint extraction from existing data
│   ├── scale/           Multi-dimensional scaling
│   ├── tokenize/        Dataset tokenization
│   ├── enrich/          Model enrichment from reference data
│   ├── model/           Model serialization and conversion
│   ├── decision.rs      Decision logging and reporting
│   └── cli/             CLI commands and configuration
├── docs/                Design documents and language spec
├── examples/            Example knit blueprints
└── tests/               Integration tests
```

### Core Pipeline Dependencies

The following graph shows the primary pipeline dependencies between modules.
Auxiliary modules (`scale`, `tokenize`, `enrich`, `model`, `decision`) are used
by various pipeline stages and by `cli` but are omitted for clarity.

```mermaid
flowchart BT
    core[core]
    blueprint[blueprint] --> core
    plan[plan] --> core & blueprint
    gen[gen] --> plan
    noise[noise] --> gen
    bind[bind] --> noise
    learn[learn] --> blueprint
    cli[cli] --> gen & learn & bind
```

`core` is the foundational module that every other module depends on. It contains no
engine logic — only the shared type definitions that flow between stages.

---

## 5. Modules

Each module has a specific role in the architecture. Dedicated design documents
cover each module in detail; this section provides the high-level purpose,
responsibilities, and key design points.

### 5.1 core — Semantic Model

**Role:** Define the shared vocabulary for the entire toolset.

**Responsibilities:**
- `DataModel` — the in-memory representation of a parsed Knit document
- `Entity`, `Field`, `Relationship`, `Constraint` — structural types
- `GeneratorSpec`, `DistributionSpec` — generation specifications
- `Value` enum — typed value representation (used at API boundaries)
- `NullSpec`, `CountSpec` — behavioral specifications
- Temporal types: `date`, `time`, `datetime`, `datetimetz`, `duration`

**Design principle:** Narrow and stable. No engine traits, no I/O, no external
dependencies beyond `serde` and `chrono`. Changes to `core` ripple across all
modules, so the bar for additions is high.

---

### 5.2 blueprint — Parser & Validator

**Role:** Transform Knit text into a validated `DataModel`.

**Responsibilities:**
- Parse TOML and JSON into `DataModel`
- Resolve `extends` chains (single inheritance, keyed merge, flattening)
- Resolve `includes` (type/mixin library imports)
- Resolve `params` (compile-time substitution)
- Validate structural, type, referential, and semantic rules
- Report machine-readable errors with element paths and line numbers

**Key design points:**
- Blueprint normalization (`knit blueprint normalize`) — rewrite to canonical form
- Blueprint expansion (`knit blueprint expand`) — flatten inheritance
- Separate parse → resolve → validate phases for clear error reporting
- JSON Blueprint generation for external validation (IDE, AI pipelines)

---

### 5.3 plan — Execution Planner

**Role:** Compile a validated `DataModel` into an `ExecutionPlan` that the generation
engine can execute efficiently.

**Responsibilities:**
- Build entity dependency graph from relationships (via `petgraph`)
- Topological sort for acyclic subgraph
- Identify cyclic/deferred relationships → assign two-phase generation
- Partition entities into parallel work units
- Build hierarchical deterministic RNG tree:
  `hash(global_seed, entity, field, partition)` → per-stream seed
- Determine index strategy (in-memory vs spill-to-disk) based on entity sizes
- Compile field-level generator specs into `FieldPlan` execution nodes
- Resolve derived field DAG ordering within each entity

**Key design points:**
- The plan is a **pure data structure** — no I/O, no randomness, fully deterministic
- Same `DataModel` always produces the same `ExecutionPlan`
- Plan can be inspected (`knit plan <blueprint>`) for debugging before generation

**Key types:**
```
ExecutionPlan
├── phases: [Phase]              # ordered generation phases
│   ├── entity_plans: [EntityPlan]
│   │   ├── partitions: [PartitionRange]
│   │   └── field_plans: [FieldPlan]
│   └── deferred_refs: [DeferredRef]   # backpatch after phase
├── rng_tree: RngTree            # deterministic seed hierarchy
└── index_strategy: IndexStrategy
```

---

### 5.4 gen — Generation Engine

**Role:** Execute the plan to produce Arrow `RecordBatch` streams.

**Responsibilities:**
- Columnar generation using Arrow array builders (not row-by-row `Value`)
- Two-phase generation for cyclic relationships
- Key stores for FK resolution (in-memory + spill-to-disk)
- Statistical distribution sampling (17+ distributions)
- Faker-style structured data generation
- Derived field evaluation (expression interpreter)
- Conditional and composite generators
- Time series generation (trend, seasonality, AR, event streams)
- Graph topology generation (Barabási–Albert, Watts–Strogatz, etc.)
- Correlation enforcement (copula-based joint distributions)
- Parallel partition execution via `rayon`

**Key design points:**
- `FieldGenerator` trait — each generator type implements this to produce `ArrayRef`
- `KeyStore` trait — abstracts PK index for FK resolution (memory or disk-backed)
- 64K rows per batch (tuned to stay in CPU cache for numeric columns)
- Each partition gets its own deterministic RNG stream → reproducible in any thread order
- Streaming: batches are handed to noise/bind immediately, never buffered in full

---

### 5.5 noise — Perturbation Pipeline

**Role:** Inject controlled imperfections into generated data.

**Responsibilities:**
- Three-stage invariant-aware pipeline: **clean → constrained → breaking**
- `Perturbator` trait — each noise type implements this
- Per-field or scoped (conditional) noise application
- Built-in perturbators: null injection, Gaussian noise, typos, outliers,
  duplicates, FK violations, temporal spikes
- Noise operates on `RecordBatch` in-place for efficiency

**Key design points:**
- Perturbators declare what invariants they break (`InvariantSet`)
- Clean stage preserves all constraints; breaking stage intentionally violates them
- Users control which stages run — clean-only for test data, full pipeline for
  robustness testing
- Scoped noise: apply only to records matching a predicate

---

### 5.6 bind — Output Serialization

**Role:** Write `RecordBatch` streams to output files.

**Responsibilities:**
- **Sinks** (columnar, dataset-oriented):
  - Parquet — zero-copy from `RecordBatch`, configurable compression (zstd, lz4, snappy)
  - JSON / JSONL — streaming, one object per line or array-of-objects
  - CSV — streaming rows
  - Arrow IPC — for inter-process analytics pipelines
- **Templates** (row-oriented, custom formats):
  - MiniJinja integration for user-defined templates
  - SQL INSERT statements, XML, log lines, etc.

**Key design points:**
- `Sink` trait — unified interface for all output formats
- Each partition writes to its own file (no write contention)
- Parquet: row groups align with generation batches for streaming writes
- Template rendering converts `RecordBatch` → row iterator → template context

---

### 5.7 learn — Blueprint Extraction

**Role:** Reverse-engineer a knit blueprint from an existing dataset.

**Responsibilities:**
- Ingest existing data via Arrow readers (CSV, Parquet, JSON)
- Infer column types from samples
- Fit candidate statistical distributions per column (KS-test / AIC scoring)
- Detect FK relationships via value overlap + cardinality analysis
- Detect self-referential patterns
- Score every inferred element with a confidence value
- Emit a candidate `DataModel` with review annotations

**Key design points:**
- Statistical approach only (no heavy ML dependencies in v1)
- Output is a **candidate** — confidence scores indicate certainty
- Candidate relationships are marked for human/AI review
- Reproducible: same input data → same inferred blueprint

---

### 5.8 cli — Command-Line Interface

**Role:** User-facing binary that orchestrates all modules.

**Commands:**

| Command | Modules Used | Description |
|---------|-------------|-------------|
| `knit init` | blueprint | Create a starter knit blueprint (interactive) |
| `knit validate <blueprint>` | blueprint | Parse and validate, report errors |
| `knit plan <blueprint>` | blueprint, plan | Show execution plan (dry run) |
| `knit generate <blueprint> -o <dir>` | blueprint, plan, gen, noise, bind | Full forward pipeline |
| `knit learn <data> -o <blueprint>` | learn | Reverse pipeline |
| `knit blueprint expand <file>` | blueprint | Flatten `extends` chain |
| `knit blueprint normalize <file>` | blueprint | Reformat to canonical style |

**Key flags:**

| Flag | Description |
|------|-------------|
| `--seed <N>` | Override global RNG seed |
| `--format parquet\|json\|csv` | Output format (default: parquet) |
| `--compression zstd\|lz4\|snappy\|none` | Parquet compression |
| `--parallel <N>` | Thread count (default: num_cpus) |
| `--batch-size <N>` | Rows per batch (default: 65536) |
| `--param key=value` | Override blueprint parameters |
| `--dry-run` | Show plan without generating |

**Key design points:**
- Built with `clap` for argument parsing
- Progress bars via `indicatif`
- Structured error output (human-readable + JSON for CI)
- Exit codes: 0 = success, 1 = validation error, 2 = generation error

---

## 6. Data Flow

### 6.1 Batch Lifecycle

A single batch flows through the pipeline without full-dataset buffering:

```mermaid
flowchart LR
    subgraph partition[Per Partition - repeat until count reached]
        ep([ExecutionPlan\npartition N, seed N]) --> gen[Generate\nbatch]
        gen --> perturb[Perturb\nbatch]
        perturb --> sink[Write to Sink\nParquet/…]
    end
```

### 6.2 Parallel Execution

Partitions run concurrently across a `rayon` thread pool. Each partition is independent:

```mermaid
flowchart TB
    subgraph phase1[Phase 1: Independent entities — no deps]
        p0_1[Partition 0] --> f0_1([file_0])
        p1_1[Partition 1] --> f1_1([file_1])
        p2_1[Partition 2] --> f2_1([file_2])
    end

    phase1 -- key stores populated --> phase2

    subgraph phase2[Phase 2: Dependent entities — FK resolution]
        p0_2[Partition 0] --> f0_2([file_0])
        p1_2[Partition 1] --> f1_2([file_1])
    end

    phase2 -- if cycles --> phase3

    subgraph phase3[Phase N: Backpatch deferred FKs]
        bp[Backpatch cyclic relationships]
    end
```

### 6.3 Two-Phase Generation (Cyclic Relationships)

When the dependency graph contains cycles (e.g., A references B, B references A):

```mermaid
sequenceDiagram
    participant P as Planner
    participant G as Generator
    participant KA as A KeyStore
    participant KB as B KeyStore

    rect rgb(230, 240, 255)
        Note over P,KB: Phase 1 — Create records (cyclic FKs are NULL)
        P->>G: Generate A records (PKs only)
        G->>KA: Store A PKs
        P->>G: Generate B records (PKs only)
        G->>KB: Store B PKs
    end

    rect rgb(255, 240, 230)
        Note over P,KB: Phase 2 — Backpatch deferred FKs
        P->>G: Backpatch A.fk_to_b
        G->>KB: Sample from B key store
        KB-->>G: Return keys
        P->>G: Backpatch B.fk_to_a
        G->>KA: Sample from A key store
        KA-->>G: Return keys
    end
```

The planner detects cycles automatically. Cyclic FK fields must be nullable.

---

## 7. Performance Architecture

**Target:** 100GB+ Parquet output in 1–3 hours on 8+ core, 32GB RAM hardware.

### Design Decisions for Performance

| Decision | Rationale |
|----------|-----------|
| Columnar generation (Arrow) | Avoid per-cell `Value` boxing; vectorized operations |
| 64K-row batches | Fit in L2 cache for numeric columns |
| Rayon parallelism | Partition-level concurrency with work stealing |
| Per-partition output files | No write contention between threads |
| Streaming pipeline | Generate → perturb → write; one batch in flight per partition |
| Deterministic hierarchical RNG | Reproducible regardless of thread scheduling |
| Tiered key stores | In-memory for < 10M keys; memory-mapped for larger |
| Zero-copy Parquet writes | RecordBatch → ParquetWriter without serialization |

### Bottleneck Mitigation

| Bottleneck | Mitigation |
|-----------|------------|
| Parquet compression CPU | `zstd` level 1 or `lz4` for throughput |
| String allocation | Arrow `StringBuilder` with pre-allocated capacity |
| FK lookup contention | Read-only key store views per partition |
| Memory pressure | Streaming; only 1 batch in flight per partition |
| Disk I/O | Parallel writes to separate files on NVMe |

---

## 8. Extension Architecture

### 8.1 v1: Compile-Time Registry

Custom generators, distributions, and perturbators are registered via the `inventory`
crate at compile time:

```mermaid
flowchart LR
    user[User code] --> impl[Implements FieldGenerator trait]
    impl --> reg["inventory::submit! { GeneratorPlugin }"]
    reg --> bin[Links into knit binary]
```

### 8.2 Future: WASM Plugins

Dynamic extension without recompilation. WASM modules implement a stable ABI and are
loaded at runtime via `--plugin` / `--plugin-dir` flags. Plugins currently run as
trusted local code without sandboxing.

---

## 9. Design Documents Index

| Document | Path | Status |
|----------|------|--------|
| Knit Language Specification | [`docs/knit-spec.md`](knit-spec.md) | Draft |
| Architecture (this document) | [`docs/architecture.md`](architecture.md) | Draft |
| core Design | [`docs/design-core.md`](design-core.md) | Draft |
| blueprint Design | [`docs/design-blueprint.md`](design-blueprint.md) | Draft |
| plan Design | [`docs/design-plan.md`](design-plan.md) | Draft |
| gen Design | [`docs/design-gen.md`](design-gen.md) | Draft |
| noise Design | [`docs/design-noise.md`](design-noise.md) | Draft |
| bind Design | [`docs/design-bind.md`](design-bind.md) | Draft |
| learn Design | [`docs/design-learn.md`](design-learn.md) | Draft |
| cli Design | [`docs/design-cli.md`](design-cli.md) | Draft |
| scale Design | [`docs/design-scale.md`](design-scale.md) | Draft |
| tokenize Design | [`docs/design-tokenize.md`](design-tokenize.md) | Draft |
| enrich Design | [`docs/design-enrich.md`](design-enrich.md) | Draft |
| model Design | [`docs/design-model.md`](design-model.md) | Draft |
| Human Behavior Design | [`docs/design-human-behavior.md`](design-human-behavior.md) | Draft |
| Logging Design | [`docs/design-logging.md`](design-logging.md) | Draft |
| Incremental Learn Design | [`docs/design-incremental-learn.md`](design-incremental-learn.md) | Draft |
| Development Plan | [`docs/dev-plan.md`](dev-plan.md) | Draft |

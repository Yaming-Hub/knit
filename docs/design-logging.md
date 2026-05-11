# Knit Logging — Design Document

**Version:** 0.1.0
**Status:** Draft

---

## Table of Contents

- [1. Logging Guidance](#1-logging-guidance)
  - [1.1 What to Log](#11-what-to-log)
  - [1.2 What NOT to Log](#12-what-not-to-log)
  - [1.3 How to Log](#13-how-to-log)
- [2. Current State Audit](#2-current-state-audit)
  - [2.1 Inventory Summary](#21-inventory-summary)
  - [2.2 Silent Decision Points (Gaps)](#22-silent-decision-points-gaps)
  - [2.3 Anti-Patterns Found](#23-anti-patterns-found)
- [3. Log Levels & Audiences](#3-log-levels--audiences)
- [4. Decision Logging](#4-decision-logging)
- [5. Structured Log Format](#5-structured-log-format)
- [6. Pipeline Phase Logging](#6-pipeline-phase-logging)
- [7. Log Sinks & Output](#7-log-sinks--output)
- [8. AI-Friendly Log Conventions](#8-ai-friendly-log-conventions)
- [9. CLI Integration](#9-cli-integration)
- [10. Implementation Plan](#10-implementation-plan)

---

## 1. Logging Guidance

This section defines the logging policy for knit. Every contributor should read
this before adding or modifying log statements.

### 1.1 What to Log

#### ✅ ALWAYS log (at the indicated level):

| What | Level | Why | Example |
|------|-------|-----|---------|
| **Decision reasoning** | `debug` | Users and AI need to understand *why* knit chose a distribution, generator, or relationship — not just *what* it chose | "chose normal(μ=45,σ=12): KS-test=0.87, beat lognormal(0.72) and uniform(0.31)" |
| **Rejected alternatives** | `debug` | Knowing what was *not* chosen is as important as knowing what was — it answers "did knit even consider X?" | "rejected lognormal: right-skew not observed in data" |
| **Confidence scores** | `debug` | Low-confidence decisions need human review; AI tools filter on confidence | "confidence=0.52 (below threshold 0.6)" |
| **Pipeline milestones** | `info` | Users need progress feedback during long runs | "ingesting table Collab: 60 rows, 166 columns" |
| **Phase boundaries** | `info` | Delineate pipeline stages for readability and timing | "learn phase complete: 3 tables, 185 columns, 2.3s" |
| **Summaries** | `info` | End-of-run overview of everything that happened | "model written: 3 tables, 2 FKs, 12 correlations" |
| **Fallbacks and degradation** | `warn` | Something went wrong but knit recovered — user should know | "Uniform min≥max, falling back to (0,1)" |
| **Threshold-based filtering** | `debug` | When knit applies a threshold (correlation ≥0.3, p<0.05), log what passed AND what was filtered | "correlation Duration↔Subject: r=0.03, below threshold 0.3, skipped" |
| **Configuration resolution** | `debug` | Which config files were found/loaded/merged | "loaded local config from .knit.toml" |
| **Errors with context** | `error` | Enough context to locate and fix the problem | "parse error in Collab.toml line 42: unknown field 'typ'" |
| **Data shape metrics** | `debug` | Row counts, distinct counts, null rates, value ranges — the facts that drive decisions | "Duration: n=60, nulls=2, min=5.0, max=120.0, mean=45.2" |
| **File I/O** | `info` | What files were read/written, sizes, paths | "wrote output/Collab/PartitionDate=2024-10-13/data.parquet (130 rows)" |

#### ✅ Log at `trace` level only:

| What | Why | Example |
|------|-----|---------|
| **Sample data values** | Debugging requires seeing actual values, but they may contain PII | "sample: [45.0, 38.2, 51.7, ...]" |
| **Correlation matrices** | Full matrices are verbose but needed for correlation debugging | "Pearson [[1.0, 0.42], [0.42, 1.0]]" |
| **Per-row/per-batch iteration** | Hot-loop diagnostics — only enable when actively debugging | "inserted PK 42 into key store" |
| **Intermediate computation state** | Algorithm internals that only developers need | "FFT magnitudes: [0.8, 0.2, 0.05, ...]" |

### 1.2 What NOT to Log

#### ❌ NEVER log:

| What | Why |
|------|-----|
| **Secrets, credentials, tokens** | Security — logs may be shared, stored, or sent to AI tools |
| **Full record/row contents at info or debug** | Privacy — data may contain PII; use column names and aggregate stats instead |
| **Raw user data values at info level** | Privacy — even column values like names, emails should only appear at `trace` |
| **Redundant information** | Noise — don't log the same fact at multiple levels (log once at the right level) |
| **Success of routine operations** | Noise — "successfully opened file" adds no value; only log opens that are notable (first file, fallback path) |
| **Implementation details** | Confusion — internal variable names, memory addresses, struct debug dumps don't help users |
| **Speculative warnings** | Alarm fatigue — don't warn about things that *might* be wrong; only warn when knit actually degraded behavior |

#### ⚠️ Be careful with:

| What | Rule |
|------|------|
| **File paths** | OK at info for I/O operations, but use relative paths (not absolute) to avoid leaking system layout |
| **Column names** | OK at all levels — these are schema-level, not data-level |
| **Entity/table names** | OK at all levels — structural metadata |
| **Distribution parameters** | OK at debug — these are model parameters, not raw data |
| **Counts and rates** | OK at all levels — aggregate statistics don't reveal individual records |

### 1.3 How to Log

#### Rule 1: Use structured fields, not format strings

```rust
// ❌ Bad: information buried in format string, not filterable
debug!("chose normal distribution for Duration (score=0.87)");
warn!("found local config: {}", path.display());

// ✅ Good: structured fields that tools can filter and aggregate
debug!(column = "Duration", generator = "normal", confidence = 0.87, "distribution selected");
debug!(path = %path.display(), "local config found");
```

**Why:** Structured fields enable AI tools to filter (`confidence < 0.6`),
aggregate (`count decisions by kind`), and correlate (`all decisions for table X`).
Format strings require regex parsing.

#### Rule 2: Use tracing spans for hierarchical context

```rust
// ❌ Bad: context manually repeated in every log line
debug!(phase = "learn", table = "Collab", column = "Duration", "fitting");
debug!(phase = "learn", table = "Collab", column = "Duration", "selected");

// ✅ Good: span carries context automatically
let _span = info_span!("learn").entered();
let _table = info_span!("table", name = "Collab").entered();
let _col = info_span!("column", name = "Duration").entered();
debug!("fitting");   // Automatically includes learn > table{Collab} > column{Duration}
debug!("selected");
```

**Why:** Spans provide automatic hierarchical context without repetition.
They also enable timing (span enter/exit) and distributed tracing.

#### Rule 3: One decision, one log event

Each non-trivial decision should produce exactly one `debug!` event that includes:
- **What** was decided (the choice)
- **Why** (the reasoning)
- **What else** was considered (alternatives, if applicable)
- **How confident** (0.0–1.0 score)

```rust
debug!(
    chosen = "normal",
    params = "μ=45.2, σ=12.1",
    confidence = 0.87,
    reason = "best KS-test score, symmetric histogram",
    alternatives = "lognormal(0.72), uniform(0.31)",
    "distribution selected"
);
```

#### Rule 4: Consistent message verbs

Use **present tense, verb-first** messages. Standardize on these patterns:

| Pattern | Use for | Example |
|---------|---------|---------|
| `"<noun> discovered"` | Finding something | `"table discovered"` |
| `"<noun> selected"` | Making a choice | `"generator selected"` |
| `"<noun> detected"` | Inferring from data | `"relationship detected"` |
| `"<noun> fitted"` | Statistical fitting | `"distribution fitted"` |
| `"<noun> complete"` | Phase/step finished | `"ingestion complete"` |
| `"<noun> skipped"` | Filtered/excluded | `"correlation skipped"` |
| `"<noun> written"` | File output | `"output written"` |
| `"<noun> fallback"` | Degraded path taken | `"parameter fallback"` |

#### Rule 5: Rate-limit hot-loop logging

```rust
// ❌ Bad: O(n) log lines for n rows
for (i, pk) in primary_keys.iter().enumerate() {
    trace!(pk = %pk, "inserted PK");
}

// ✅ Good: one summary after the loop
let count = primary_keys.len();
// ... insert loop ...
trace!(count, "PKs inserted into key store");
```

**Why:** A 1M-row table would produce 1M trace lines. Log once per batch or
per entity instead.

#### Rule 6: Promote low-confidence decisions to `warn`

When a decision's confidence is below 0.6, promote it from `debug` to `warn`
and include an actionable suggestion:

```rust
if confidence < 0.6 {
    warn!(
        column = col_name,
        chosen = best.name,
        confidence,
        runner_up = second.name,
        runner_up_score = second.score,
        "low-confidence distribution fit — consider manual review"
    );
} else {
    debug!(column = col_name, chosen = best.name, confidence, "distribution fitted");
}
```

#### Rule 7: Always include enough context to locate

Every log line must be self-locating. A reader seeing a single line in a log
file should know *which table*, *which column*, and *which phase* produced it —
either from explicit fields or from the enclosing span.

---

## 2. Current State Audit

Audit of all logging in the knit codebase as of v0.4.0 (317 log statements
across 50+ source files).

### 2.1 Inventory Summary

| Module | Total | trace | debug | info | warn | Spans |
|--------|-------|-------|-------|------|------|-------|
| **learn** | 62 | 0 | 38 | 18 | 6 | 0 |
| **gen** | 151 | 1 | 12 | 8 | 130 | 0 |
| **cli** | 62 | 0 | 27 | 9 | 26 | 0 |
| **noise** | 18 | 15 | 1 | 2 | 0 | 0 |
| **bind** | 14 | 0 | 14 | 0 | 0 | 0 |
| **plan** | 2 | 0 | 0 | 0 | 2 | 0 |
| **Total** | **317** | **16** | **92** | **37** | **164** | **0** |

**Key observations:**
- **Zero tracing spans** — no hierarchical context anywhere
- **Gen module is 86% warnings** — mostly FK fallback warnings that repeat per entity
- **Bind module is 100% format strings** — no structured fields
- **Learn module has the most critical gaps** — decision points are silent

### 2.2 Silent Decision Points (Gaps)

These are places where knit makes important decisions but logs nothing (or
insufficient detail). Ordered by impact.

#### Learn Phase (Critical)

| File | Decision | Gap |
|------|----------|-----|
| `fitting.rs:98–345` | **Distribution selection** — which distribution, why, alternatives | Only logs final "best fit" summary; no rejected candidates, no selection criteria |
| `relationships.rs:77–155` | **FK candidate evaluation** — name matching, overlap, confidence | Only logs final count; no per-candidate scores or rejection reasons |
| `temporal.rs:88–300` | **Temporal pattern classification** — cadence, periodicity, FFT | Only logs when <3 timestamps; pattern detection is completely silent |
| `correlation.rs:51–130` | **Correlation filtering** — which pairs kept, which dropped | Logs accepted correlations but not rejected ones or threshold rationale |
| `type_inference.rs:200–250` | **Categorical vs Text boundary** — distinct/total ratio | No logging of the ratio evaluation or threshold |
| `schema_assembly.rs` | **Generator selection per column** — why sequence vs uuid vs one_of | Partial; missing selection reasoning |

#### Generate Phase (High)

| File | Decision | Gap |
|------|----------|-----|
| `engine.rs:527–541` | **String vs Int PK detection** — key store type | Completely silent |
| `engine.rs:570–630` | **Sequential vs parallel partitioning** — why sequential forced | Logs the boolean but not the reason (stateful TS, unique constraint) |
| `engine.rs:745–860` | **FK generator variant selection** — actor-aware, weighted, etc. | Logs non-default selections but default (uniform FK) is silent |
| `commands/generate.rs:32–88` | **Partition row allocation** — how rows distributed across partitions | Silent |
| `commands/generate.rs` | **Count scaling** — how `--count 10x` is applied | Silent |

#### Other Phases (Medium)

| File | Decision | Gap |
|------|----------|-----|
| `compiler.rs:67–100` | **Index and partition strategy** | Silent |
| `bind/*.rs` | **Format auto-detection** | Silent |
| `commands/learn.rs` | **Companion file classification** | Silent |
| `commands/learn.rs` | **Null handling strategy** | Silent |

### 2.3 Anti-Patterns Found

#### A. Format strings instead of structured fields (14 instances)

```rust
// Found in config.rs, bind/csv.rs, bind/json.rs, bind/avro.rs, etc.
debug!("found local config: {}", local.display());          // ❌
debug!("CSV batch size: {}", batch.num_rows());              // ❌
```

**Fix:** Convert to `debug!(path = %local.display(), "local config found")`.

#### B. Raw record content logged at debug level (6 instances)

```rust
// Found in bind/json.rs, bind/csv.rs
debug!("JSON record: {}", record);    // ❌ May contain PII
```

**Fix:** Remove or downgrade to `trace!` with column names only.

#### C. Hot-loop logging without rate limiting (3 locations)

| File | Location | Issue |
|------|----------|-------|
| `engine.rs:686–690` | PK insertion loop | trace! per batch — can emit thousands of lines |
| `correlation.rs:68–110` | Pairwise comparison | debug! per pair — O(n²) for n columns |
| `relationships.rs:89–155` | FK matching | debug! per candidate — O(n²) |

**Fix:** Log one summary per loop/phase; use per-batch or per-entity granularity.

#### D. Inconsistent naming (50+ instances)

| Inconsistency | Examples |
|---------------|----------|
| Entity reference | `entity`, `target`, `target_entity` (3 names for same concept) |
| Column reference | `fields`, `columns`, `cols` |
| Correlation fields | `a`, `b`, `column_a`, `column_b` |
| Message tense | Past ("completed"), present ("reading"), noun ("ingestion") |

**Fix:** Standardize per Rule 4 in §1.3.

---

## 3. Log Levels & Audiences

| Level | Audience | Content | When to use |
|-------|----------|---------|-------------|
| `error` | Everyone | Fatal failures, unrecoverable states | Parse errors, I/O failures, constraint violations |
| `warn` | Users | Degraded behavior, fallbacks, data quality concerns | Type coercion, missing values, ambiguous inferences, low-confidence decisions |
| `info` | Users | Pipeline progress, key milestones, summaries | Phase start/end, entity counts, output paths |
| `debug` | Power users / AI | Decision reasoning, alternatives considered, confidence scores | Distribution fitting, relationship detection, generator selection |
| `trace` | Developers | Data samples, internal state, iteration details | Sample values, correlation matrices, intermediate computations |

### 3.1 Level Guidelines

**`info`** — A user running `knit learn` or `knit generate` without `-v` should
see a clean, readable narrative:

```
INFO  learn: ingesting directory "tc_multiple_weeks/Encoded"
INFO  learn: discovered 3 tables (AnalyzedUser, Collab, PeopleHistorical)
INFO  learn: AnalyzedUser — 8 rows, 19 columns
INFO  learn: Collab — 60 rows, 166 columns, partitioned by PartitionDate (13 partitions)
INFO  learn: detected 2 relationships (Collab→AnalyzedUser, PeopleHistorical→AnalyzedUser)
INFO  learn: found 30 companion files (3 schemas, 27 dictionaries)
INFO  learn: model written to schema.weave.toml (11,905 lines)
```

**`debug`** — A user running with `-v` or an AI diagnosing issues sees
decision-level detail:

```
DEBUG learn.table.Collab.column.Duration: fitting distribution
      candidates: [normal(μ=45.2,σ=12.1) score=0.87, lognormal(μ=3.7,σ=0.28) score=0.72, uniform(5..120) score=0.31]
      selected: normal — best KS-test score (0.87), shape matches symmetric histogram
DEBUG learn.table.Collab.column.SignalType: choosing generator
      distinct=4, null_rate=0.0, top_value_coverage=1.0
      decision: one_of — small cardinality (4), all values observed, categorical distribution
DEBUG learn.relationship: evaluating FK candidate Collab.ActorId → AnalyzedUser.ObjectId
      containment=1.0, cardinality_ratio=7.5, type_match=true
      decision: confirmed as many_to_one FK (containment=100%, types match)
```

**`trace`** — Full data samples and internal state:

```
TRACE learn.table.Collab.column.Duration: sample values [45.0, 38.2, 51.7, 12.0, 67.3, ...]
TRACE learn.correlation: Pearson matrix for [ParticipantCount, Duration]:
      [[1.0, 0.42], [0.42, 1.0]]
TRACE gen.table.Collab: generated batch 1/3, rows=20, seed=42
```

---

## 4. Decision Logging

The core innovation: a `Decision` log event type that captures structured
reasoning for every non-trivial choice.

### 4.1 Decision Event Structure

```rust
/// A structured decision record emitted during learn/generate pipelines.
struct Decision {
    /// What component made this decision
    phase: Phase,          // Learn, Generate, Plan, Validate, Scale, etc.
    /// What entity/table this relates to (if applicable)
    table: Option<String>,
    /// What column this relates to (if applicable)
    column: Option<String>,
    /// What kind of decision was made
    kind: DecisionKind,
    /// What was chosen
    chosen: String,
    /// Why it was chosen (human-readable reasoning)
    reason: String,
    /// What alternatives were considered
    alternatives: Vec<Alternative>,
    /// Confidence in the decision (0.0 - 1.0)
    confidence: f64,
}

struct Alternative {
    name: String,
    score: f64,
    rejected_because: String,
}

enum DecisionKind {
    GeneratorSelection,     // Which generator type for a column
    DistributionFitting,    // Which distribution best fits observed data
    TypeInference,          // What data type to assign a column
    RelationshipDetection,  // Whether two columns form a FK relationship
    PartitionDetection,     // Whether a column is a partition key
    CadenceDetection,       // What temporal cadence was detected
    CorrelationInclusion,   // Whether a correlation is significant enough to model
    NullHandling,           // How to handle null/missing values
    ScalingStrategy,        // How to scale a dimension
    TokenMapping,           // How to tokenize a value
    CompanionClassification,// Whether a file is data or companion
    OutputFormat,           // Which output format to use
    Custom(String),         // Extension point
}
```

### 4.2 Decision Categories by Pipeline Phase

#### Learn Phase Decisions

| Decision | Example reasoning |
|----------|------------------|
| **Type inference** | "Inferred `int64` for ObjectId: all 60 values parse as integer, no decimals, range 1-60" |
| **Generator selection** | "Chose `one_of` for SignalType: 4 distinct values, full coverage, categorical" |
| **Distribution fitting** | "Chose `normal(μ=45,σ=12)` for Duration: KS-test p=0.87, symmetric histogram, no heavy tails" |
| **Primary key detection** | "Marked ObjectId as PK: unique count equals row count, sequential pattern, no nulls" |
| **FK detection** | "Confirmed Collab.ActorId→AnalyzedUser.ObjectId: containment=100%, type match, cardinality ratio=7.5" |
| **Partition detection** | "Detected PartitionDate as partition key: column name matches folder variable, 13 distinct values match 13 folders" |
| **Cadence detection** | "Detected weekly cadence: median gap=7d, std=1.2d, 10/12 gaps within ±2d of median" |
| **Correlation significance** | "Including ParticipantCount↔Duration correlation (r=0.42, p<0.01); excluding Duration↔Subject (r=0.03, not significant)" |
| **Companion classification** | "Classified schema.json as companion: not tabular data, JSON schema format" |
| **Null handling** | "Column Region: null_rate=0.15, chose nullable=true with null_rate=0.15 in generator" |
| **Actor detection** | "AnalyzedUser identified as actor entity: has PK, referenced by 2 FKs, lowest cardinality root" |

#### Generate Phase Decisions

| Decision | Example reasoning |
|----------|------------------|
| **Generation order** | "Generating AnalyzedUser first: root of FK dependency graph, no inbound FKs" |
| **Count scaling** | "Scaling Collab from 60→600: --count 10x applied uniformly" |
| **Partition allocation** | "Partition 2024-10-13: 130 rows (source: 13, scale: 10x)" |
| **FK sampling** | "Sampling ActorId from AnalyzedUser pool: 80 values available, weighted random" |
| **Type coercion** | "Coercing NullArray→Int64 in conditional branch: default branch is null, Meeting branch produces Int64" |
| **Fallback** | "Falling back to string for Duration: interleave failed with mixed types (Int64, Float64)" |

#### Validate Phase Decisions

| Decision | Example reasoning |
|----------|------------------|
| **Constraint check** | "Unique constraint on ObjectId: passed (60 values, 60 distinct)" |
| **FK integrity** | "FK Collab.ActorId→AnalyzedUser.ObjectId: passed (all 60 values found in parent)" |
| **Range check** | "Duration range [5.0, 120.0]: 2 values outside range (outlier_rate=0.03)" |

---

## 5. Structured Log Format

### 5.1 Default (Human-Readable)

The default stderr output uses `tracing-subscriber`'s compact format with
hierarchical span context:

```
2025-05-10T12:00:00Z INFO  learn > ingesting "tc_multiple_weeks/Encoded"
2025-05-10T12:00:01Z INFO  learn.table{name=Collab} > 60 rows, 166 columns
2025-05-10T12:00:01Z DEBUG learn.table{name=Collab}.column{name=Duration} > fitting distribution
    chosen=normal(μ=45.2,σ=12.1) confidence=0.87
    reason="best KS-test score, symmetric histogram"
    alternatives=[lognormal(0.72), uniform(0.31)]
```

### 5.2 JSON (Machine-Parseable)

With `--log-format json`, each log line is a self-contained JSON object:

```json
{
  "timestamp": "2025-05-10T12:00:01.234Z",
  "level": "DEBUG",
  "phase": "learn",
  "table": "Collab",
  "column": "Duration",
  "event": "decision",
  "decision": {
    "kind": "distribution_fitting",
    "chosen": "normal",
    "params": { "mean": 45.2, "std": 12.1 },
    "confidence": 0.87,
    "reason": "best KS-test score, symmetric histogram",
    "alternatives": [
      { "name": "lognormal", "score": 0.72, "rejected": "right-skew not observed" },
      { "name": "uniform", "score": 0.31, "rejected": "poor fit for peaked distribution" }
    ]
  }
}
```

### 5.3 Log Line Categories

Every log line belongs to one of these categories (indicated by `event` field in
JSON mode):

| Category | Description | Example |
|----------|-------------|---------|
| `progress` | Pipeline milestone | "ingesting table Collab" |
| `decision` | Non-trivial choice with reasoning | "chose normal distribution" |
| `metric` | Quantitative measurement | "correlation r=0.42, p<0.01" |
| `warning` | Degraded behavior or concern | "high null rate (45%) in Region" |
| `summary` | End-of-phase or end-of-run summary | "learn complete: 3 tables, 185 columns" |
| `diagnostic` | Internal state for debugging | "FK pool: 80 values, min=1, max=80" |

---

## 6. Pipeline Phase Logging

### 6.1 Learn Pipeline

```
┌─ learn
│  ├─ ingest           "discovered 3 tables"
│  │  ├─ table         "AnalyzedUser: 8 rows, 19 columns"
│  │  │  ├─ column     "ObjectId: int64, unique, sequential → sequence generator"
│  │  │  ├─ column     "Region: string, 5 distinct → one_of generator"
│  │  │  └─ ...
│  │  ├─ table         "Collab: 60 rows, 166 columns"
│  │  └─ table         "PeopleHistorical: 7 rows, 15 columns"
│  ├─ relationships    "detected 2 FKs"
│  │  ├─ candidate     "Collab.ActorId → AnalyzedUser.ObjectId: confirmed (containment=100%)"
│  │  └─ candidate     "PeopleHistorical.AnalyzedUserId → AnalyzedUser.ObjectId: confirmed"
│  ├─ correlations     "found 12 significant correlations in Collab"
│  ├─ companions       "classified 30 non-data files"
│  └─ summary          "model complete: 3 tables, 185 columns, 2 relationships, 12 correlations"
```

### 6.2 Generate Pipeline

```
┌─ generate
│  ├─ load             "loaded model: 3 tables, 185 columns"
│  ├─ plan             "generation order: AnalyzedUser → PeopleHistorical → Collab"
│  ├─ phase[0]
│  │  └─ entity        "AnalyzedUser: generating 80 rows (source: 8, scale: 10x)"
│  ├─ phase[1]
│  │  └─ entity        "PeopleHistorical: generating 70 rows across 7 partitions"
│  │     ├─ partition   "2024-10-13: 10 rows"
│  │     └─ ...
│  ├─ phase[2]
│  │  └─ entity        "Collab: generating 600 rows across 13 partitions"
│  ├─ companions       "copied 30 companion files"
│  └─ summary          "generation complete: 750 rows, 20 files, 30 companions"
```

### 6.3 Tracing Spans

Use `tracing` spans to carry hierarchical context automatically:

```rust
use tracing::{info, debug, info_span};

let _span = info_span!("learn").entered();

for table in &tables {
    let _table_span = info_span!("table", name = %table.name).entered();
    info!(rows = table.row_count, columns = table.column_count, "ingested");

    for column in &table.columns {
        let _col_span = info_span!("column", name = %column.name).entered();
        debug!(
            chosen = %chosen_generator,
            confidence = score,
            reason = %reason,
            alternatives = ?alternatives,
            "generator selected"
        );
    }
}
```

---

## 7. Log Sinks & Output

### 7.1 Stderr (Default)

All log output goes to stderr (already implemented). stdout is reserved for
data output and structured command results.

### 7.2 Log File

```bash
knit learn data/ -o model/ --log-file knit.log
```

Writes all log events (up to the configured level) to a file. Useful for
post-hoc analysis. Default format is JSON when writing to file (even if
stderr shows human-readable format).

### 7.3 Decision Report

```bash
knit learn data/ -o model/ --decision-report decisions.json
```

Writes **only** decision events to a structured JSON file — a focused record of
every choice knit made. This file is designed to be:

- Fed to an AI for diagnosis ("why does my output look wrong?")
- Diffed between runs ("what changed when I re-learned with new data?")
- Reviewed by users ("do I agree with these inferences?")

**Decision report format:**

```json
{
  "knit_version": "0.4.0",
  "command": "learn",
  "timestamp": "2025-05-10T12:00:00Z",
  "source": "tc_multiple_weeks/Encoded",
  "decisions": [
    {
      "id": "d001",
      "phase": "learn",
      "table": "Collab",
      "column": "Duration",
      "kind": "distribution_fitting",
      "chosen": "normal",
      "params": { "mean": 45.2, "std": 12.1 },
      "confidence": 0.87,
      "reason": "best KS-test score (0.87), symmetric histogram, no heavy tails",
      "alternatives": [
        { "name": "lognormal", "score": 0.72, "rejected": "right-skew not observed in data" },
        { "name": "uniform", "score": 0.31, "rejected": "poor fit for peaked distribution" }
      ]
    },
    {
      "id": "d002",
      "phase": "learn",
      "table": "Collab",
      "column": "ActorId",
      "kind": "relationship_detection",
      "chosen": "foreign_key",
      "params": { "references": "AnalyzedUser.ObjectId", "kind": "many_to_one" },
      "confidence": 1.0,
      "reason": "containment=100%, type match (int64→int64), cardinality ratio=7.5",
      "alternatives": []
    }
  ],
  "summary": {
    "total_decisions": 215,
    "by_kind": {
      "generator_selection": 185,
      "distribution_fitting": 12,
      "relationship_detection": 6,
      "partition_detection": 3,
      "correlation_inclusion": 9
    },
    "low_confidence": [
      { "id": "d047", "kind": "distribution_fitting", "confidence": 0.45,
        "note": "Collab.ResponseTime: lognormal and exponential nearly tied" }
    ]
  }
}
```

### 7.4 Summary Report

At the end of each pipeline run, knit prints a structured summary to stderr:

```
══════════════════════════════════════════════
  knit learn — Summary
══════════════════════════════════════════════
  Source:          tc_multiple_weeks/Encoded
  Tables:          3
  Total columns:   185
  Total rows:      77
  Relationships:   2 foreign keys
  Correlations:    12 significant pairs
  Companions:      30 files (3 schemas, 27 dictionaries)
  Decisions:       215 total (4 low-confidence)
  Output:          schema.weave.toml (11,905 lines)
  Duration:        2.3s
══════════════════════════════════════════════
```

---

## 8. AI-Friendly Log Conventions

These conventions complement the general guidance in §1.3 with AI-specific
considerations. When an AI tool processes knit logs, it benefits from:

### 8.1 Stable Event Names

Use consistent, stable event names (the message string in `tracing` macros) so
AI tools can match on them:

| Event name | Meaning |
|-----------|---------|
| `"table discovered"` | New table found during ingestion |
| `"column profiled"` | Column statistics computed |
| `"generator selected"` | Generator type chosen for a column |
| `"distribution fitted"` | Statistical distribution fitted to data |
| `"relationship detected"` | FK or association found between tables |
| `"correlation measured"` | Correlation coefficient computed |
| `"companion classified"` | File classified as data or companion |
| `"partition detected"` | Hive partition structure found |
| `"generation complete"` | Entity/table generation finished |
| `"output written"` | File written to disk |

### 8.2 Low-Confidence Alerts

Decisions with confidence below a threshold (default 0.6) should be promoted to
`warn` level with actionable guidance:

```
WARN  learn.table.Collab.column.ResponseTime: low-confidence distribution fit
      chosen=lognormal(0.52) — alternatives nearly tied: exponential(0.48)
      suggestion: inspect column histogram, consider specifying distribution manually
```

---

## 9. CLI Integration

### 9.1 Verbosity Flags

Already implemented (keep unchanged):

```bash
knit learn data/          # info level (progress + summaries)
knit learn data/ -v       # debug level (+ decisions)
knit learn data/ -vv      # trace level (+ data samples)  [NEW]
knit learn data/ -q       # error level only
```

### 9.2 New Flags

```bash
# Output format
--log-format <text|json>      # Default: text for tty, json for piped output

# Log file
--log-file <PATH>             # Write all log events to file (always JSON)

# Decision report
--decision-report <PATH>      # Write decision-only report (JSON)

# Filter by component
--log-filter <FILTER>         # tracing EnvFilter syntax (e.g., "learn=debug,gen=info")
```

### 9.3 Environment Variables

```bash
KNIT_LOG=debug                       # Same as -v
KNIT_LOG=learn::correlation=trace    # Fine-grained module filtering
KNIT_LOG_FORMAT=json                 # Force JSON output
```

These are already partially supported via `RUST_LOG` (tracing-subscriber
`EnvFilter`). The `KNIT_LOG` variants provide a knit-specific namespace.

---

## 10. Implementation Plan

### Phase 1: Foundation — Spans & Convention Cleanup

Fix anti-patterns identified in the §2 audit before adding new logging.

- Add `info_span!` wrappers around all major pipeline phases:
  - `learn` → `table{name}` → `column{name}`
  - `generate` → `phase{idx}` → `entity{name}` → `partition{value}`
  - `noise` → `injector{name}`
- Convert 14 format-string log calls to structured fields (bind module, config.rs)
- Remove 6 raw-record-content debug calls (bind/json.rs, bind/csv.rs) — replace
  with column-name-only trace
- Rate-limit 3 hot-loop trace sites (engine.rs PK loop, correlation pairwise,
  relationship candidate loop) — switch to per-entity/per-batch summaries
- Standardize message verb conventions per §1.3 Rule 4 across all 317 log sites

**Files to touch (from §2.3):**
- `src/bind/csv.rs`, `src/bind/json.rs`, `src/bind/avro.rs`, `src/bind/ipc.rs`,
  `src/bind/parquet.rs`, `src/bind/template.rs`, `src/bind/sql.rs`
- `src/cli/config.rs`
- `src/gen/engine.rs` (spans + rate limiting)
- `src/learn/correlation.rs` (rate limiting)
- `src/learn/relationships.rs` (rate limiting)

### Phase 2: Learn Pipeline — Decision Logging

Instrument the 6 critical silent decision points from §2.2.

- `src/learn/fitting.rs:98–345` — log distribution candidates, scores,
  selection reasoning, and rejected alternatives
- `src/learn/relationships.rs:77–155` — log per-candidate FK evaluation
  (name match score, overlap, type match, confidence, rejection reason)
- `src/learn/temporal.rs:88–300` — log cadence detection (gap analysis,
  periodicity check, pattern classification)
- `src/learn/correlation.rs:51–130` — log accepted AND rejected correlations
  with threshold rationale
- `src/learn/type_inference.rs:200–250` — log categorical vs text boundary
  decision (distinct/total ratio, threshold)
- `src/learn/schema_assembly.rs` — log generator selection reasoning
  (why sequence vs uuid vs one_of vs distribution)
- Add low-confidence promotion (§1.3 Rule 6) for all learn decisions
- Add end-of-learn summary report

### Phase 3: Generate Pipeline — Decision Logging

Instrument the 5 high-priority silent decision points from §2.2.

- `src/gen/engine.rs:527–541` — log PK type detection (string vs int)
- `src/gen/engine.rs:570–630` — log sequential-partition reasoning
  (stateful TS, unique constraint, etc.)
- `src/gen/engine.rs:745–860` — log default FK generator selection
  (currently only non-default is logged)
- `src/cli/commands/generate.rs:32–88` — log partition row allocation
- `src/cli/commands/generate.rs` — log count scaling application
- Add end-of-generate summary report

### Phase 4: Decision Report & JSON Output

- Define `Decision` struct and `DecisionKind` enum in `src/core/`
- Implement `DecisionLogger` that collects decisions during pipeline execution
- Add `--decision-report <PATH>` flag — writes JSON decision report
- Add `--log-format json` with structured JSON formatter
- Add `--log-file <PATH>` for file output
- Add `KNIT_LOG` environment variable support
- Auto-detect tty vs pipe for default format selection

### Phase 5: Remaining Gaps

- `src/plan/compiler.rs` — index and partition strategy logging ✅
- `src/bind/*.rs` — format auto-detection logging (N/A: format is user-specified, not auto-detected)
- `src/cli/commands/learn.rs` — companion classification, null handling ✅
- Low-confidence summary in end-of-run report ✅
- Confidence thresholds per `DecisionKind` (deferred: current global thresholds are sufficient)

---

## 11. Implementation Status

| Phase | Status | PR |
|-------|--------|-----|
| Phase 1: Foundation (spans, structured fields) | ✅ Done | #223 |
| Phase 2: Learn decision logging | ✅ Done | #224 |
| Phase 3: Generate pipeline decision logging | ✅ Done | #225 |
| Phase 4: Decision Report & JSON Output | ✅ Done | #231 |
| Phase 5: Remaining gaps | ✅ Done | #233 |

### Phase 4 Implementation Details

| Component | Location | Description |
|-----------|----------|-------------|
| `Decision` struct | `src/decision.rs` | Kind, phase, entity, column, chosen, reason, confidence, alternatives |
| `DecisionKind` enum | `src/decision.rs` | 12 variants covering all pipeline decision types |
| `DecisionLogger` | `src/decision.rs` | Thread-safe collector with fluent builder API |
| `DecisionReport` | `src/decision.rs` | Final JSON report with summary statistics |
| `--decision-report` flag | `src/cli/mod.rs` | Global CLI flag to enable report output |
| Global logger | `src/decision.rs` | `OnceLock`-based global accessor for zero-cost when disabled |
| Learn integration | `fitting.rs`, `relationships.rs`, `type_inference.rs`, `schema_assembly.rs` | Records distribution fits, FK detection, type inference, generator selection |

---

## Appendix A: Example AI Troubleshooting Session

A user reports that generated Duration values don't match expected distribution.
They share the decision report with an AI assistant:

```
User: My generated Duration column looks wrong. Here's the decision report.

AI: Looking at decision d001, knit chose normal(μ=45.2, σ=12.1) with
    confidence 0.87 for Collab.Duration. The alternatives were lognormal
    (score 0.72) and uniform (score 0.31).

    The confidence is high (0.87), suggesting the normal fit was reasonable.
    However, I notice the source data had min=5.0, max=120.0, which suggests
    the distribution may be bounded. A truncated normal or beta distribution
    might be more appropriate.

    Also, decision d089 shows a conditional dependency: Duration depends on
    SignalType (meetings have μ=45, emails have value=0). If your generated
    data has different SignalType proportions than the source, the overall
    Duration distribution will shift.

    Suggestion: Check if --count scaling changed the SignalType mix, and
    consider adding explicit bounds to the normal generator.
```

## Appendix B: Interaction with Structured Model

When using the [structured model format](design-model.md), decision reports
can reference specific model files:

```json
{
  "id": "d001",
  "model_file": "tables/Collab.toml",
  "line": 42,
  "column": "Duration",
  "kind": "distribution_fitting",
  "chosen": "normal",
  ...
}
```

This enables AI tools to directly navigate to the relevant model section when
diagnosing issues.

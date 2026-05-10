# Design: Model Enrichment from Reference Samples (`knit enrich`)

## 1. Motivation

A common workflow with knit is:

1. **Start with a base dataset** — small, structurally correct, but with limited
   data variety (few rows, narrow value distributions, placeholder-like content)
2. **Acquire real-world samples over time** — individual contributors provide
   personal or domain-specific data that demonstrates realistic patterns
3. **Improve the model** — extract statistical knowledge from samples and merge
   it into the base model, making generated data progressively more realistic

The challenge: **reference samples may not match the base dataset's schema**.
They might have different column names, different entity structures, additional
or missing fields, or different data formats. `knit enrich` bridges this gap
by performing intelligent cross-schema knowledge transfer.

**Key principle:** Only structural information and statistical traits are
extracted from reference samples — the actual sample data is never copied into
the model or output. This enables privacy-preserving collaborative enrichment
where individuals contribute their data patterns without exposing their data.

---

## 2. User Experience

### 2.1 Enrich from a Single Reference Sample

```bash
# Start with a base model learned from a small dataset
knit learn base_dataset/ -o model.weave.toml

# Enrich the model with a reference sample (possibly different schema)
knit enrich model.weave.toml --ref sample_data.csv -o enriched.weave.toml

# Output:
# ═══ Enrichment Report ═══
#
#   Reference:  sample_data.csv (1,200 rows, 45 columns)
#   Base model: model.weave.toml (3 entities, 185 columns)
#
#   Mappings found:
#     sample.UserName     → AnalyzedUser.PersonId    (string, 92% confidence)
#     sample.EmailCount   → Collab.MessageSent-Count (int, 87% confidence)
#     sample.MeetingHours → Collab.Duration          (float, 79% confidence)
#     ... 28 more mappings
#
#   Enrichments applied:
#     AnalyzedUser.PersonId  — value distribution updated (8 → 45 unique patterns)
#     Collab.Duration        — distribution refined (uniform → lognormal μ=2.3 σ=0.8)
#     Collab.MessageSent-*   — correlation structure added (3 new correlations)
#     ... 15 more enrichments
#
#   Skipped (low confidence):
#     sample.CustomField1    — no match found (threshold: 70%)
#     sample.InternalCode    — ambiguous match (2 candidates, both <60%)
#
#   Written: enriched.weave.toml
```

### 2.2 Incremental Enrichment (Multiple Samples Over Time)

```bash
# Person A enriches with their data
knit enrich model.weave.toml --ref alice_data/ -o model.weave.toml

# Person B enriches with their data (different format, different columns)
knit enrich model.weave.toml --ref bob_export.parquet -o model.weave.toml

# Person C enriches with their data
knit enrich model.weave.toml --ref carol_data.json -o model.weave.toml

# Each enrichment refines distributions, adds correlation evidence, and
# improves value variety — without storing any individual's data
```

### 2.3 Review and Control

```bash
# Preview what would be enriched without modifying the model
knit enrich model.weave.toml --ref sample.csv --dry-run

# Only enrich specific entities
knit enrich model.weave.toml --ref sample.csv --entity Collab -o enriched.weave.toml

# Set confidence threshold for automatic mapping
knit enrich model.weave.toml --ref sample.csv --min-confidence 0.8 -o enriched.weave.toml

# Interactive mode: confirm each mapping
knit enrich model.weave.toml --ref sample.csv --interactive -o enriched.weave.toml
```

### 2.4 Publish Base + Collect Enrichments

```bash
# Publisher: create and distribute base model
knit learn base_dataset/ -o shared_model.weave.toml

# Contributors: each person enriches with private data
# (could be automated via CI, script, or shared tool)
knit enrich shared_model.weave.toml --ref my_private_data/ -o shared_model.weave.toml

# The model accumulates knowledge from all contributors.
# Generate realistic data from the community-enriched model:
knit generate shared_model.weave.toml -o synthetic/ --count 10000
```

---

## 3. Cross-Schema Mapping

The core challenge: how to map columns from a reference sample (unknown schema)
to fields in the base model (known schema).

### 3.1 Mapping Signals

Multiple signals are combined to score candidate mappings:

| Signal | Weight | Description |
|--------|--------|-------------|
| **Name similarity** | 0.3 | Fuzzy string matching on column names (Levenshtein, Jaccard on tokens, abbreviation expansion) |
| **Type compatibility** | 0.2 | Data type match or safe coercion (int→float OK, string→int unlikely) |
| **Distribution shape** | 0.25 | Statistical similarity (KS test for numeric, entropy/cardinality for categorical) |
| **Value overlap** | 0.15 | Percentage of reference values found in base model's learned value set |
| **Structural position** | 0.1 | Similar ordinal position, co-occurrence with already-mapped columns |

### 3.2 Mapping Algorithm

```
1. For each reference column R:
   a. Compute similarity score against every base model field B
   b. Filter candidates below min_confidence threshold
   c. If exactly one candidate above threshold → auto-map
   d. If multiple candidates → pick highest; flag for review if margin < 10%
   e. If no candidates → skip with warning

2. Resolve conflicts (two reference columns map to same base field):
   a. Keep the higher-confidence mapping
   b. Flag the conflict for user review

3. Output the mapping table for user confirmation (or auto-apply)
```

### 3.3 Mapping Persistence

Confirmed mappings are stored in the enrichment state so that subsequent
enrichments from similar sources can reuse them:

```toml
# In the model's enrichment metadata
[enrichment]
sample_count = 3
last_enriched = "2026-05-10T12:00:00Z"

[[enrichment.mappings]]
source_pattern = "UserName"          # regex or exact match
target = "AnalyzedUser.PersonId"
confidence = 0.92

[[enrichment.mappings]]
source_pattern = "Email.*Count"
target = "Collab.MessageSent-Count"
confidence = 0.87
```

When a new reference sample has a column matching a stored pattern, the mapping
is applied automatically without re-scoring.

---

## 4. Knowledge Extraction

Once columns are mapped, knit extracts statistical knowledge from the reference
sample and merges it into the base model.

### 4.1 What Is Extracted

| Knowledge Type | Extraction | Merge Strategy |
|---------------|------------|----------------|
| **Value distribution** | Fit distribution (normal, lognormal, uniform, etc.) | Weighted average of parameters with existing distribution |
| **Categorical frequencies** | Count distinct values and their frequencies | Merge frequency tables with Bayesian smoothing |
| **Null rate** | Proportion of null/missing values | Running average across samples |
| **Correlation structure** | Pairwise Pearson/Spearman for mapped numeric pairs | Update correlation matrix with new evidence |
| **Conditional patterns** | When discriminator field maps, extract per-value null patterns | Merge conditional null rates |
| **Value range** | Min/max for numeric, date range for temporal | Expand range to union of base + sample |
| **String patterns** | Regex patterns, length distribution, character classes | Merge pattern sets |
| **Cardinality** | Distinct value count relative to row count | Running estimate via HyperLogLog |

### 4.2 What Is NOT Extracted

- **Actual data values** — Individual row data is never stored in the model
- **Identifying patterns** — Unique identifiers, personal names, etc. are not
  transferred (only distribution shape and cardinality)
- **Schema structure** — The base model's entity/field structure is not changed;
  reference data is mapped into the existing structure

### 4.3 Merge Strategies

**Numeric distributions:**
```
new_params = α × base_params + (1 - α) × sample_params
where α = base_weight / (base_weight + sample_weight)
      base_weight = base_sample_count × base_row_count
      sample_weight = sample_row_count
```

**Categorical frequencies (Bayesian merge):**
```
For each value v:
  merged_freq(v) = (base_count(v) + sample_count(v) + prior)
                 / (base_total + sample_total + prior × num_categories)
```

**Correlations (Fisher z-transform merge):**
```
z_base = arctanh(r_base),  z_sample = arctanh(r_sample)
z_merged = (n_base × z_base + n_sample × z_sample) / (n_base + n_sample)
r_merged = tanh(z_merged)
```

---

## 5. Enrichment State Tracking

The model tracks enrichment history to support incremental refinement:

```toml
[enrichment]
sample_count = 5                    # number of reference samples applied
total_reference_rows = 12450        # total rows across all samples
last_enriched = "2026-05-10T12:00:00Z"

# Per-field enrichment evidence
[[enrichment.evidence]]
field = "Collab.Duration"
base_rows = 60                     # rows from original base dataset
reference_rows = 3200              # total rows from all reference samples
distribution = "lognormal"         # best-fit distribution after enrichment
params = { mu = 2.31, sigma = 0.82 }
confidence = 0.94                  # goodness-of-fit

[[enrichment.evidence]]
field = "AnalyzedUser.PersonId"
base_cardinality = 8
reference_cardinality = 187
merged_cardinality_estimate = 195
```

This state allows:
- **Weighted merging**: New samples are weighted proportionally to their row count
- **Convergence tracking**: As more samples arrive, distribution parameters stabilize
- **Audit trail**: Users can see how many samples contributed to the model

---

## 6. Implementation Architecture

### 6.1 Pipeline

```
load_model → load_reference → map_columns → extract_knowledge → merge → write_model
```

**Phase 1: Load** — Parse the base model (`.weave.toml`) and ingest the
reference sample (CSV/Parquet/JSON, using existing ingestion pipeline).

**Phase 2: Map** — Score all reference columns against base model fields
using the multi-signal scoring algorithm. Apply stored mappings for known
patterns. Present mapping table to user.

**Phase 3: Extract** — For each mapped column pair, compute distribution
parameters, correlations, null rates, and cardinality from the reference data.

**Phase 4: Merge** — Combine extracted knowledge with existing model parameters
using weighted merge strategies. Update enrichment state counters.

**Phase 5: Write** — Serialize the enriched model back to `.weave.toml`.

### 6.2 File Organization

```
src/cli/commands/enrich.rs       — CLI command, mapping display, progress
src/enrich/mod.rs                — Orchestration, pipeline phases
src/enrich/mapper.rs             — Cross-schema column mapping (scoring, conflict resolution)
src/enrich/extract.rs            — Statistical knowledge extraction from reference data
src/enrich/merge.rs              — Distribution merging, correlation matrix update
src/enrich/state.rs              — Enrichment state tracking, serialization
```

---

## 7. CLI Specification

```
knit enrich <MODEL> [OPTIONS]

Arguments:
    <MODEL>                   Path to the base model (.weave.toml)

Options:
    --ref <PATH>              Reference sample path (file or directory, repeatable)
    -o, --output <PATH>       Output model path (default: overwrite input)
    --entity <NAME>           Only enrich specific entity (repeatable)
    --min-confidence <F>      Minimum mapping confidence 0.0-1.0 (default: 0.7)
    --interactive             Confirm each mapping interactively
    --dry-run                 Show mappings and enrichments without modifying model
    --show-mappings           Display the full mapping table and exit
    --mapping-file <PATH>     Load/save explicit column mappings (JSON)
    --quiet                   Suppress progress output
    --json                    Machine-readable JSON output
```

---

## 8. Edge Cases and Error Handling

| Case | Behavior |
|------|----------|
| Reference has no mappable columns | Warning: "No columns matched with confidence ≥ threshold" |
| Reference has different row granularity | Map at column level; row count difference is expected |
| Reference is much larger than base | Weight proportionally; cap sample influence with `--max-weight` |
| Reference has conflicting distribution | Both distributions kept; enrichment metadata records conflict |
| Same reference applied twice | Deduplicate via content hash; warn and skip |
| Base model has no enrichment state | Initialize fresh enrichment state |
| Column name collision in mapping | Highest confidence wins; lower-confidence flagged |
| Type mismatch (reference int, base string) | Skip with warning unless coercible |
| Reference in different format than base | Format-agnostic: ingestion handles CSV/Parquet/JSON uniformly |
| `--interactive` with piped input | Fall back to auto-apply with warnings |

---

## 9. Privacy Considerations

`knit enrich` is designed for privacy-preserving collaborative model building:

| Property | Guarantee |
|----------|-----------|
| **No data retention** | Reference data is read, statistics extracted, data discarded |
| **No value storage** | Individual values are not stored in the model (only distributions, counts, ranges) |
| **No reverse engineering** | Distribution parameters cannot reconstruct individual records |
| **Contributor isolation** | Multiple contributors' statistics are merged; individual contributions cannot be separated |
| **Audit trail** | Sample count and total rows tracked, but not source identity |

**What IS stored in the model:** Distribution parameters (μ, σ, weights),
correlation coefficients, null rates, cardinality estimates, value ranges,
category frequency tables (aggregated, not individual).

**What is NOT stored:** Row-level data, unique identifiers, file paths,
contributor identity, raw sample content.

For stronger privacy guarantees, consider combining with `knit tokenize`
to pre-process reference samples before enrichment.

---

## 10. Relationship to Other Features

| Feature | Relationship |
|---------|-------------|
| **`knit learn`** | Enrich builds on learn's ingestion and statistical profiling |
| **`knit learn --incremental`** | Incremental learn handles same-schema data streams; enrich handles cross-schema knowledge transfer |
| **`knit scale`** | Enriched models produce more realistic data when scaled up |
| **`knit tokenize`** | Contributors can tokenize their data before enrichment for extra privacy |
| **Correlations** | Enrich can discover and strengthen correlation specifications |
| **Conditional generators** | Enrich can refine conditional null patterns from reference data |

---

## 11. Future Work

### 11.1 Federated Enrichment
Distributed enrichment where contributors run extraction locally and submit
only the statistical summaries (not data) to a central model. Requires a
defined summary exchange format.

### 11.2 Schema Suggestion
When reference samples have unmapped columns that appear valuable, suggest
adding new fields to the base model (opt-in schema evolution).

### 11.3 Enrichment Quality Scoring
Score the enriched model's fidelity by generating synthetic data and comparing
its statistical properties against the reference samples.

### 11.4 Active Learning
Identify which types of reference data would most improve the model and
guide contributors on what data to provide.

### 11.5 Conflict Resolution Strategies
When two reference samples provide contradictory evidence (e.g., different
distribution families for the same field), offer merge policies: newest-wins,
largest-sample-wins, ensemble (mixture distribution).

---

## 12. Implementation Status (v1)

### 12.1 Implemented Features

| Feature | Status | Notes |
|---------|--------|-------|
| Column mapping (name similarity) | ✅ | Levenshtein + abbreviation expansion |
| Column mapping (type compatibility) | ✅ | Core types covered |
| Distribution merge (same-family) | ✅ | Combined variance for Normal |
| OneOf merge (weighted average) | ✅ | Normalized probabilities, new value discovery |
| Entity auto-detection | ✅ | By filename stem or field count fallback |
| Dry-run mode | ✅ | `--dry-run` shows mappings without changes |
| CLI integration | ✅ | `--ref`, `--entity`, `--min-confidence`, `--max-rows`, `-o` |

### 12.2 v1 Limitations

- **Single reference file per invocation** — no batch/directory mode yet
- **No incremental state tracking** — repeated enrichment re-merges from scratch
- **Cross-family distributions skip** — Normal+Exponential won't merge (by design)
- **No temporal or complex type enrichment** — only numeric distributions and string categoricals
- **Greedy mapping only** — no interactive confirmation of column assignments

### 12.3 Deferred Work (v2+)

- Incremental enrichment state file (§10.1)
- Federated/distributed enrichment (§11.1)
- Schema suggestion for unmapped columns (§11.2)
- Quality scoring (§11.3)
- Active learning guidance (§11.4)
- Conflict resolution strategies (§11.5)

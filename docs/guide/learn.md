# Reverse Engineering Guide

Knit can analyze existing datasets and automatically infer a
knit blueprint — the reverse of data generation. This is useful when you
have production data and want to generate realistic synthetic equivalents.

**[← Back to User Guide](index.md)**

---

## What `knit learn` Does

The `knit learn` command reads your data, profiles every column, fits
statistical distributions, detects relationships, and outputs a
`.knit.toml` blueprint that can reproduce data with similar characteristics.

```bash
knit learn data/users.csv
knit learn data/users.csv -o my_blueprint.knit.toml
```

The output blueprint includes **confidence annotations** on every inferred
element, so you know which decisions are solid and which need manual review.

---

## Supported Input Formats

| Format | Extension | Notes |
|--------|-----------|-------|
| CSV | `.csv` | Auto-detects delimiter, headers, encoding |
| Parquet | `.parquet` | Preserves types from the Parquet schema |
| JSON | `.json` | Expects an array of objects |
| JSON Lines | `.jsonl` | One JSON object per line |

You can also point `knit learn` at a **directory** of files:

```bash
knit learn data/users/
knit learn data/
```

---

## The Learn Pipeline

The learning process runs through 9 phases:

```mermaid
flowchart TD
    ingest[1. Ingest] --> profile[2. Profile]
    profile --> infer[3. Infer Types]
    infer --> fit[4. Fit Distributions]
    fit --> temporal[5. Detect Temporal Patterns]
    temporal --> rels[6. Detect Relationships]
    rels --> analyze[7. Analyze Relationships]
    analyze --> corr[8. Detect Correlations]
    corr --> assemble[9. Assemble Blueprint]
```

### Phase 1: Ingestion

Reads data files into Arrow record batches. For large datasets, Knit samples
to keep profiling fast:

- **Default sample:** 100,000 rows
- **Sampling methods:** full scan, head (first N), reservoir sampling,
  stratified sampling

### Phase 2: Column Profiling

Computes comprehensive statistics for every column:

- **All types:** count, null count, null rate, distinct count, cardinality ratio
- **Numeric:** min, max, mean, median, std_dev, skewness, kurtosis,
  percentiles (p1, p5, p25, p50, p75, p95, p99)
- **String:** min/max/avg length, pattern detection (email, phone, UUID,
  date, URL, IP address)
- **Temporal:** date range, granularity (second/minute/hour/day),
  business hours percentage, timezone detection

### Phase 3: Type Inference

Determines the semantic type of each column:

| Detected Type | How It's Identified |
|---------------|---------------------|
| Integer | Numeric with no decimal digits |
| Float | Numeric with decimals |
| Boolean | Only true/false, 0/1, yes/no values |
| Date/Datetime | Matches ISO 8601, US, or EU date formats |
| UUID | Matches UUID v4/v7 pattern |
| Categorical | Low cardinality relative to row count |
| Continuous | High cardinality numeric |
| Free text | High cardinality, long strings |

Each inference includes a **confidence score** based on parse success rate
and format consistency.

### Phase 4: Distribution Fitting

For numeric and temporal columns, Knit fits multiple candidate distributions:

**Candidates tested:** uniform, normal, log_normal, exponential, poisson,
zipf, beta, gamma, pareto

**Method:**
1. Maximum Likelihood Estimation (MLE) for each candidate
2. Kolmogorov–Smirnov test for goodness of fit
3. AIC (Akaike Information Criterion) and BIC (Bayesian Information Criterion)
4. Best fit selected by AIC; alternatives within ΔAIC < 2 are reported

For categorical columns, value frequencies are captured as a `one_of`
generator with weights.

### Phase 5: Temporal Pattern Recognition

For date/time columns, Knit detects:

- **Periodicity:** Regular intervals between records
- **Frequency:** Events per time unit
- **Seasonality:** Daily, weekly, monthly, yearly cycles
- **Business-time patterns:** Concentration during work hours
- **Event cadence:** Burst patterns and quiet periods

This information maps to `time_series` generator components in the output
blueprint.

### Phase 6: Relationship Detection

Knit infers foreign key relationships using:

- **Naming conventions:** Columns like `user_id`, `customer_id` suggest FK
  relationships to `users`, `customers`
- **Value overlap:** High overlap between columns across tables indicates
  a referential relationship
- **Cardinality analysis:** One-to-many vs. many-to-many detection

### Phase 7: Relationship Analysis

For detected relationships, Knit profiles:

- **Degree distribution:** How many children per parent (fitted to a
  distribution)
- **Temporal ordering:** Whether child records always follow parent records
- **Graph topology:** For many-to-many relationships

### Phase 8: Correlation Detection

Knit identifies statistical correlations between fields:

| Method | What It Detects | Use Case |
|--------|----------------|----------|
| Pearson | Linear correlations | Numeric ↔ numeric |
| Spearman | Monotonic correlations | Ranked/ordinal data |
| Cramér's V | Categorical associations | Categorical ↔ categorical |

Strong correlations (|r| > 0.3) are included in the output blueprint as
`[[correlations]]` entries.

### Phase 9: Blueprint Assembly

All inferred information is assembled into a complete knit blueprint with:

- Entity definitions with row counts
- Field types and generators
- Relationships with cardinality distributions
- Correlations
- **Confidence annotations** on every element

---

## Planned Features

### Interactive Review Mode (Planned)

An interactive review mode is planned for low-confidence decisions:

```
? Column "status" detected as categorical (confidence: 0.72)
  Inferred generator: one_of ["active", "inactive", "pending"]
  Accept? [Y/n/edit]
```

### Roundtrip Workflow

A common workflow is to learn a blueprint, tune it, and generate:

```bash
# Step 1: Infer blueprint from production data
knit learn prod_export.csv -o blueprint.knit.toml

# Step 2: Review and tune the blueprint (edit in your editor)
# - Adjust distributions
# - Fix low-confidence inferences
# - Add noise profiles for testing

# Step 3: Validate your tuned blueprint
knit validate blueprint.knit.toml

# Step 4: Generate synthetic data
knit generate blueprint.knit.toml -o ./synthetic_data
```

---

## Output: Confidence Annotations

The inferred blueprint includes confidence scores as comments:

```toml
[[entities.fields]]
name = "amount"
data_type = "float"
# confidence: 0.94 — log_normal (AIC: 1234.5)
# alternatives: normal (ΔAIC: 3.2), gamma (ΔAIC: 5.1)
[entities.fields.generator]
type = "distribution"
kind = "log_normal"
[entities.fields.generator.params]
mu = 3.8
sigma = 1.1
```

**Confidence levels:**

| Score | Meaning | Action |
|-------|---------|--------|
| 0.9+ | High confidence | Usually correct, no review needed |
| 0.7–0.9 | Medium confidence | Quick review recommended |
| < 0.7 | Low confidence | Manual review and tuning advised |

---

## Limitations

- **Sample-based:** Large datasets are sampled (default 100K rows), so rare
  patterns may be missed
- **Single-table focus:** Multi-file relationship detection works best when
  column names follow conventions (`user_id` → `users.id`)
- **No semantic understanding:** Knit detects statistical patterns, not
  business logic. A column of employee IDs and a column of department IDs
  may look similar statistically
- **Distribution approximation:** Real data may not perfectly match any
  standard distribution. Complex multi-modal data may need manual tuning
- **Temporal patterns:** Complex seasonal patterns (holidays, business
  calendars) may not be fully captured automatically

### When Manual Tuning Is Needed

- Distribution fits have low confidence (< 0.7)
- Business rules that can't be inferred statistically (e.g., "orders can
  only be placed during business hours")
- Complex conditional logic between fields
- Specific noise profiles for testing scenarios
- Custom generator parameters (patterns, faker categories)

---

## Incremental Learning (Large Datasets)

For datasets too large to fit in memory, `knit learn` supports **incremental
mode**. Instead of loading all data at once, you process data in chunks and
accumulate statistical state across multiple invocations.

### Basic Usage

```bash
# Process first chunk — creates state file
knit learn data/chunk1.csv --state learned.state

# Process additional chunks — updates state
knit learn data/chunk2.csv --state learned.state
knit learn data/chunk3.csv --state learned.state

# Generate blueprint from accumulated state
knit learn --finalize --state learned.state -o blueprint.knit.toml
```

### How It Works

The state file stores **sufficient statistics** (not raw data): streaming
means/variances, percentile sketches, cardinality estimates, and value
samples. Each chunk is processed with bounded memory and merged into the
state.

| Mode | When to Use |
|------|-------------|
| `--state` without `-o` | Update state only (accumulate evidence) |
| `--state` with `-o` | Update state AND emit blueprint |
| `--finalize --state` | Emit blueprint from existing state (no new data) |

### Limitations vs Batch Mode

- Relationship detection uses approximate overlap estimation (HLL sketches)
- Distribution fitting uses a reservoir sample (10K values), not full data
- Percentiles have ~1% relative error (t-digest approximation)
- Type inference and null rates remain exact

For datasets that fit in memory, batch mode (no `--state`) remains the
best choice for maximum accuracy.

See the [Incremental Learning Design](../design-incremental-learn.md) for
full technical details.

---

## What's Next?

- **[Blueprint Language Tutorial](blueprint-language.md)** — Understand and tune
  inferred blueprints
- **[Noise Injection Guide](noise.md)** — Add noise to your learned blueprints
- **[CLI Reference](cli-reference.md)** — All `knit learn` options

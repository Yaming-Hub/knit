# Scoring Skill: Synthetic Data Quality Assessment

This document defines the scoring methodology for evaluating Knit-generated
synthetic data against original datasets.

## Overview

Scoring uses a **hybrid approach**:
- **70% LLM score**: Structural/semantic evaluation by a language model
- **30% Statistical score**: Deterministic metrics from Python

This combination provides both stability (statistical baseline) and nuance
(LLM can catch semantic issues statistics miss).

## Scoring Rubric (0-100)

### Band Definitions

| Band | Score | Criteria |
|------|-------|----------|
| Excellent | 90-100 | Statistically indistinguishable from original. All distributions, correlations, and constraints preserved. |
| Good | 70-89 | Minor distributional differences but overall patterns preserved. Downstream analysis produces similar conclusions. |
| Fair | 40-69 | Some patterns preserved but notable issues (wrong ranges, missing correlations, implausible values). |
| Poor | 0-39 | Fundamental structural or distributional failures. Data would mislead any analysis. |

### Scoring Criteria (Weighted)

1. **Value ranges and distributions** (30%)
   - Numeric columns: values within plausible range of original
   - Generated min/max close to original min/max
   - Distribution shape preserved (uniform stays uniform, skewed stays skewed)

2. **Categorical fidelity** (20%)
   - Same categories appear with similar frequencies
   - No invented categories absent from original
   - Relative proportions roughly match

3. **Structural constraints** (20%)
   - Column relationships preserved (if A > B in original, holds in generated)
   - Foreign key validity (referenced IDs exist)
   - Row count matches original

4. **Correlation preservation** (20%)
   - Correlated columns stay correlated (e.g., height/weight)
   - Independent columns stay independent
   - Sign and rough magnitude preserved

5. **Temporal/sequential patterns** (10%)
   - Time-series trends preserved (increasing stays increasing)
   - Seasonality patterns maintained
   - Sort order correct for ordered data

### What Does NOT Affect Score

- **Exact row matches**: Synthetic data is not expected to replicate rows
- **String formatting**: Quotes, whitespace, number format differences
- **Row order**: Unless data is explicitly sorted (time-series)
- **Minor range extensions**: Original max=100, generated max=105 is fine
- **Seed variance**: Different seeds produce different outputs; score each independently

### Calibration Anchors

Use these as reference points for consistent scoring:

**~95**: iris with correct species 33/33/33 split, sepal/petal measurements in
correct ranges, petal_length/petal_width correlation ~0.96 preserved.

**~75**: Flight data with correct airports and routes, delay distributions roughly
right, but weaker distance/air_time correlation. Some non-existent airport pairs.

**~45**: Stock data with correct columns and general trend direction, but volatility
10x too high, negative prices appear, dates not sequential.

**~15**: Correct column names but random values—no tip/total_bill relationship,
country names in numeric columns, complete structural breakdown.

## LLM Scoring Protocol

### Input Format

For each dataset, the LLM receives:
- Original data (first 30 rows)
- Generated data from 3 seeds (first 30 rows each)
- The scoring rubric above

### Stability Rules

These rules ensure consistent scoring across runs:

1. **Score criteria independently first, then combine** — don't let one bad
   aspect dominate your overall impression
2. **Ignore presentation** — CSV vs JSON, quoting, CRLF vs LF don't matter
3. **Focus on statistics not semantics** — don't penalize "Springfield" paired
   with wrong state; check if city-name distribution looks reasonable
4. **Row count is binary** — correct count = full marks, wrong count = penalize
   proportionally to the error
5. **Anchor to calibration examples** — before scoring, recall the anchor scores
   above and ensure your scores are calibrated relative to them
6. **Score each dataset independently** — don't anchor on previous scores in batch
7. **Categorical: check both coverage and frequency** — having all categories but
   wrong frequencies is ~70; missing major categories is <40
8. **Numeric: focus on range + shape** — exact mean/median match not required,
   but distribution shape (uniform/normal/skewed) must match

### Response Format

```json
{"dataset-name": score, "other-dataset": score}
```

No explanations unless explicitly requested.

## Statistical Score Components

The Python scorer (`scripts/score.py`) computes:

| Component | Weight | Method |
|-----------|--------|--------|
| Schema match | 20% | Column name overlap |
| Row count | 10% | Ratio closeness to 1.0 |
| Distribution | 40% | KS test (numeric) / TV distance (categorical) |
| Correlation | 20% | Pearson correlation preservation |
| Null rate | 10% | Null frequency match |

## Running Scores

```bash
# Score all datasets (statistical only)
python scripts/score.py datasets/

# Score single dataset
python scripts/score.py datasets/iris

# Score with specific seeds
python scripts/score.py datasets/ --seeds 1 2 3

# Full hybrid score (statistical + LLM via task agents)
python scripts/score.py datasets/ --hybrid
```

## Methodology Notes

- **10 fixed seeds** (1-10) used for all scoring rounds
- Same seeds every round enables apple-to-apple comparison
- High seed variance (std_dev > 15) indicates a blueprint problem
- Generated outputs must be cleaned before re-learning to avoid contamination

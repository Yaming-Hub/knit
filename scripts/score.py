"""
Knit Quality Scorer - Deterministic statistical scoring for generated data.

Computes quality metrics comparing generated data against original:
- Column/row count match
- Value range overlap (numeric columns)
- Distribution similarity (KS test for numeric, frequency match for categorical)
- Correlation preservation
- Uniqueness ratio preservation

Usage:
    python scripts/score.py datasets/         # Score all datasets
    python scripts/score.py datasets/iris     # Score one dataset
    python scripts/score.py datasets/ --seeds 1 2 3  # Specific seeds
"""

import os
import sys
import json
import csv
import math
import argparse
from pathlib import Path
from typing import Dict, List, Optional, Tuple

import numpy as np


def load_csv(path: str) -> Tuple[List[str], List[List[str]]]:
    """Load CSV file, return (headers, rows)."""
    with open(path, 'r', encoding='utf-8', errors='replace') as f:
        reader = csv.reader(f)
        headers = next(reader)
        rows = [row for row in reader if row]
    return headers, rows


def load_json_as_table(path: str) -> Tuple[List[str], List[List[str]]]:
    """Load JSON array-of-objects, return (headers, rows) as strings."""
    with open(path, 'r', encoding='utf-8', errors='replace') as f:
        data = json.load(f)
    if not isinstance(data, list) or len(data) == 0:
        return [], []
    # Collect all keys
    all_keys = []
    seen = set()
    for obj in data:
        if isinstance(obj, dict):
            for k in obj.keys():
                if k not in seen:
                    all_keys.append(k)
                    seen.add(k)
    headers = all_keys
    rows = []
    for obj in data:
        if isinstance(obj, dict):
            rows.append([str(obj.get(k, '')) for k in headers])
    return headers, rows


def load_parquet_as_table(path: str) -> Tuple[List[str], List[List[str]]]:
    """Load parquet file, return (headers, rows) as strings."""
    import pyarrow.parquet as pq
    table = pq.read_table(path)
    df = table.to_pandas()
    headers = list(df.columns)
    rows = [[str(v) for v in row] for row in df.values.tolist()]
    return headers, rows


def load_tsv(path: str) -> Tuple[List[str], List[List[str]]]:
    """Load TSV file, return (headers, rows)."""
    with open(path, 'r', encoding='utf-8', errors='replace') as f:
        reader = csv.reader(f, delimiter='\t')
        headers = next(reader, [])
        rows = [row for row in reader]
    return headers, rows


def load_data(path: str) -> Tuple[List[str], List[List[str]]]:
    """Load data from any supported format."""
    if path.endswith('.csv'):
        return load_csv(path)
    elif path.endswith('.tsv'):
        return load_tsv(path)
    elif path.endswith('.json'):
        return load_json_as_table(path)
    elif path.endswith('.parquet'):
        return load_parquet_as_table(path)
    return [], []


def find_original(ds_path: str) -> Optional[str]:
    """Find the original data file in a dataset directory."""
    for ext in ['.csv', '.tsv', '.json', '.parquet']:
        p = os.path.join(ds_path, f'original{ext}')
        if os.path.exists(p):
            return p
    return None


def find_generated(ds_path: str, seed: int) -> Optional[str]:
    """Find generated output for a given seed.

    Prefers files named 'original.*' to match the source entity,
    falling back to the first data file found.
    """
    seed_dir = os.path.join(ds_path, f'out_seed_{seed}')
    if not os.path.isdir(seed_dir):
        return None
    # First pass: look for 'original.*' (the main entity matching source)
    for ext in ['.csv', '.tsv', '.json', '.parquet']:
        p = os.path.join(seed_dir, f'original{ext}')
        if os.path.isfile(p):
            return p
    # Second pass: any data file
    fallback = None
    for f in sorted(os.listdir(seed_dir)):
        full = os.path.join(seed_dir, f)
        if os.path.isfile(full) and f.endswith(('.csv', '.tsv', '.json', '.parquet')):
            if fallback is None:
                fallback = full
        # Handle multi-entity output directory
        if os.path.isdir(full):
            try:
                for inner in sorted(os.listdir(full)):
                    inner_full = os.path.join(full, inner)
                    if os.path.isfile(inner_full) and inner.endswith(('.csv', '.tsv', '.json', '.parquet')):
                        # Prefer 'original.*' inside subdirs too
                        if inner.startswith('original.'):
                            return inner_full
                        if fallback is None:
                            fallback = inner_full
            except OSError:
                continue
    return fallback


def is_numeric(values: List[str]) -> bool:
    """Check if a column's values are mostly numeric."""
    numeric_count = 0
    for v in values[:100]:  # Sample first 100
        v = v.strip()
        if v == '' or v.lower() in ('null', 'none', 'na', 'nan'):
            continue
        try:
            float(v)
            numeric_count += 1
        except ValueError:
            pass
    non_empty = sum(1 for v in values[:100] if v.strip() and v.lower() not in ('null', 'none', 'na', 'nan'))
    return non_empty > 0 and numeric_count / max(non_empty, 1) > 0.8


def parse_numeric(values: List[str]) -> List[float]:
    """Parse numeric values, skipping nulls."""
    result = []
    for v in values:
        v = v.strip()
        if v == '' or v.lower() in ('null', 'none', 'na', 'nan'):
            continue
        try:
            result.append(float(v))
        except ValueError:
            pass
    return result


def ks_statistic(a: List[float], b: List[float]) -> float:
    """Compute two-sample Kolmogorov-Smirnov statistic."""
    if not a or not b:
        return 1.0
    a_sorted = sorted(a)
    b_sorted = sorted(b)
    n_a = len(a_sorted)
    n_b = len(b_sorted)

    # Merge and compute ECDF difference
    all_vals = sorted(set(a_sorted + b_sorted))
    max_diff = 0.0

    for v in all_vals:
        # CDF of a at v
        cdf_a = sum(1 for x in a_sorted if x <= v) / n_a
        # CDF of b at v
        cdf_b = sum(1 for x in b_sorted if x <= v) / n_b
        max_diff = max(max_diff, abs(cdf_a - cdf_b))

    return max_diff


def ks_statistic_fast(a: List[float], b: List[float]) -> float:
    """Fast KS statistic using numpy-style approach."""
    if not a or not b:
        return 1.0
    # Sample if too large
    if len(a) > 1000:
        a = sorted(np.random.default_rng(42).choice(a, 1000, replace=False))
    if len(b) > 1000:
        b = sorted(np.random.default_rng(42).choice(b, 1000, replace=False))

    a_arr = np.array(sorted(a))
    b_arr = np.array(sorted(b))
    all_vals = np.concatenate([a_arr, b_arr])
    all_vals.sort()

    cdf_a = np.searchsorted(a_arr, all_vals, side='right') / len(a_arr)
    cdf_b = np.searchsorted(b_arr, all_vals, side='right') / len(b_arr)

    return float(np.max(np.abs(cdf_a - cdf_b)))


def categorical_similarity(orig_vals: List[str], gen_vals: List[str]) -> float:
    """Compare categorical distributions using frequency overlap."""
    if not orig_vals or not gen_vals:
        return 0.0

    # Compute frequency distributions
    orig_freq = {}
    for v in orig_vals:
        v = v.strip()
        orig_freq[v] = orig_freq.get(v, 0) + 1
    gen_freq = {}
    for v in gen_vals:
        v = v.strip()
        gen_freq[v] = gen_freq.get(v, 0) + 1

    # Normalize
    orig_total = sum(orig_freq.values())
    gen_total = sum(gen_freq.values())
    if orig_total == 0 or gen_total == 0:
        return 0.0

    orig_dist = {k: v / orig_total for k, v in orig_freq.items()}
    gen_dist = {k: v / gen_total for k, v in gen_freq.items()}

    # Compute overlap (1 - total variation distance)
    all_keys = set(orig_dist.keys()) | set(gen_dist.keys())
    tv_distance = 0.5 * sum(abs(orig_dist.get(k, 0) - gen_dist.get(k, 0)) for k in all_keys)

    return 1.0 - tv_distance


def range_overlap_score(orig_vals: List[float], gen_vals: List[float]) -> float:
    """Score how well generated range covers original range."""
    if not orig_vals or not gen_vals:
        return 0.0
    orig_min, orig_max = min(orig_vals), max(orig_vals)
    gen_min, gen_max = min(gen_vals), max(gen_vals)

    if orig_max == orig_min:
        # Single value - check if generated matches
        return 1.0 if abs(gen_min - orig_min) < 0.01 * (abs(orig_min) + 1) else 0.5

    # Overlap of ranges
    overlap_min = max(orig_min, gen_min)
    overlap_max = min(orig_max, gen_max)
    overlap = max(0, overlap_max - overlap_min)
    orig_range = orig_max - orig_min

    # Coverage: what fraction of original range is covered
    coverage = overlap / orig_range

    # Penalty for exceeding original range significantly
    gen_range = gen_max - gen_min
    excess_ratio = gen_range / orig_range if orig_range > 0 else 1.0
    excess_penalty = min(1.0, 1.0 / max(excess_ratio, 0.01)) if excess_ratio > 2.0 else 1.0

    return min(1.0, coverage * excess_penalty)


def uniqueness_score(orig_vals: List[str], gen_vals: List[str]) -> float:
    """Compare uniqueness ratios between original and generated."""
    if not orig_vals or not gen_vals:
        return 0.0
    orig_unique_ratio = len(set(orig_vals)) / len(orig_vals)
    gen_unique_ratio = len(set(gen_vals)) / len(gen_vals)

    # Score based on how close the ratios are
    diff = abs(orig_unique_ratio - gen_unique_ratio)
    return max(0.0, 1.0 - diff * 2)  # 0.5 difference = 0 score


def correlation_preservation(orig_headers: List[str], orig_rows: List[List[str]],
                            gen_headers: List[str], gen_rows: List[List[str]]) -> float:
    """Check if correlations between numeric columns are preserved."""
    # Find common numeric columns
    common_cols = [h for h in orig_headers if h in gen_headers]
    if len(common_cols) < 2:
        return 1.0  # Can't measure correlation with < 2 columns

    # Get numeric column indices
    numeric_cols = []
    for col in common_cols:
        orig_idx = orig_headers.index(col)
        orig_vals = [row[orig_idx] for row in orig_rows if orig_idx < len(row)]
        if is_numeric(orig_vals):
            numeric_cols.append(col)

    if len(numeric_cols) < 2:
        return 1.0

    # Limit to first 5 numeric columns for speed
    numeric_cols = numeric_cols[:5]

    def get_correlation(headers, rows, col1, col2):
        idx1 = headers.index(col1)
        idx2 = headers.index(col2)
        pairs = []
        for row in rows:
            if idx1 < len(row) and idx2 < len(row):
                try:
                    v1 = float(row[idx1])
                    v2 = float(row[idx2])
                    pairs.append((v1, v2))
                except (ValueError, IndexError):
                    pass
        if len(pairs) < 5:
            return 0.0
        x = [p[0] for p in pairs]
        y = [p[1] for p in pairs]
        mean_x = sum(x) / len(x)
        mean_y = sum(y) / len(y)
        cov = sum((xi - mean_x) * (yi - mean_y) for xi, yi in zip(x, y)) / len(x)
        std_x = (sum((xi - mean_x) ** 2 for xi in x) / len(x)) ** 0.5
        std_y = (sum((yi - mean_y) ** 2 for yi in y) / len(y)) ** 0.5
        if std_x < 1e-10 or std_y < 1e-10:
            return 0.0
        return cov / (std_x * std_y)

    # Compare correlations
    corr_diffs = []
    for i in range(len(numeric_cols)):
        for j in range(i + 1, len(numeric_cols)):
            orig_corr = get_correlation(orig_headers, orig_rows, numeric_cols[i], numeric_cols[j])
            gen_corr = get_correlation(gen_headers, gen_rows, numeric_cols[i], numeric_cols[j])
            corr_diffs.append(abs(orig_corr - gen_corr))

    if not corr_diffs:
        return 1.0

    avg_diff = sum(corr_diffs) / len(corr_diffs)
    return max(0.0, 1.0 - avg_diff)


def score_single(orig_headers: List[str], orig_rows: List[List[str]],
                 gen_headers: List[str], gen_rows: List[List[str]]) -> Dict[str, float]:
    """Score a single generated output against original. Returns component scores."""

    scores = {}

    # 1. Schema match (20% weight)
    orig_cols_set = set(orig_headers)
    gen_cols_set = set(gen_headers)
    if orig_cols_set:
        col_match = len(orig_cols_set & gen_cols_set) / len(orig_cols_set)
    else:
        col_match = 0.0
    scores['schema'] = col_match

    # 2. Row count match (10% weight)
    if len(orig_rows) > 0:
        ratio = len(gen_rows) / len(orig_rows)
        row_score = max(0.0, 1.0 - abs(1.0 - ratio))
    else:
        row_score = 1.0 if len(gen_rows) == 0 else 0.0
    scores['row_count'] = row_score

    # 3. Per-column distribution scores (40% weight)
    col_scores = []
    common_cols = [h for h in orig_headers if h in gen_headers]

    for col in common_cols:
        orig_idx = orig_headers.index(col)
        gen_idx = gen_headers.index(col)
        orig_vals = [row[orig_idx] for row in orig_rows if orig_idx < len(row)]
        gen_vals = [row[gen_idx] for row in gen_rows if gen_idx < len(row)]

        if not orig_vals or not gen_vals:
            col_scores.append(0.0)
            continue

        if is_numeric(orig_vals):
            orig_nums = parse_numeric(orig_vals)
            gen_nums = parse_numeric(gen_vals)
            if orig_nums and gen_nums:
                # KS test (distribution shape)
                ks = ks_statistic_fast(orig_nums, gen_nums)
                ks_score = max(0.0, 1.0 - ks * 2)  # KS=0.5 -> score 0

                # Range overlap
                range_score = range_overlap_score(orig_nums, gen_nums)

                # Uniqueness
                uniq_score = uniqueness_score(orig_vals, gen_vals)

                col_scores.append(0.5 * ks_score + 0.3 * range_score + 0.2 * uniq_score)
            else:
                col_scores.append(0.0)
        else:
            # Categorical
            cat_score = categorical_similarity(orig_vals, gen_vals)
            uniq_score = uniqueness_score(orig_vals, gen_vals)
            col_scores.append(0.7 * cat_score + 0.3 * uniq_score)

    scores['distribution'] = sum(col_scores) / max(len(col_scores), 1)

    # 4. Correlation preservation (20% weight)
    scores['correlation'] = correlation_preservation(orig_headers, orig_rows, gen_headers, gen_rows)

    # 5. Null rate match (10% weight)
    null_scores = []
    for col in common_cols:
        orig_idx = orig_headers.index(col)
        gen_idx = gen_headers.index(col)
        orig_vals = [row[orig_idx] for row in orig_rows if orig_idx < len(row)]
        gen_vals = [row[gen_idx] for row in gen_rows if gen_idx < len(row)]
        orig_null_rate = sum(1 for v in orig_vals if v.strip() == '' or v.lower() in ('null', 'none', 'nan')) / max(len(orig_vals), 1)
        gen_null_rate = sum(1 for v in gen_vals if v.strip() == '' or v.lower() in ('null', 'none', 'nan')) / max(len(gen_vals), 1)
        null_scores.append(max(0.0, 1.0 - abs(orig_null_rate - gen_null_rate) * 5))
    scores['null_rate'] = sum(null_scores) / max(len(null_scores), 1)

    return scores


def compute_final_score(component_scores: Dict[str, float]) -> int:
    """Compute weighted final score 0-100."""
    weights = {
        'schema': 0.20,
        'row_count': 0.10,
        'distribution': 0.40,
        'correlation': 0.20,
        'null_rate': 0.10,
    }
    total = sum(component_scores.get(k, 0.0) * w for k, w in weights.items())
    return min(100, max(0, round(total * 100)))


def score_dataset(ds_path: str, seeds: List[int] = None) -> Optional[Dict]:
    """Score a dataset across multiple seeds. Returns aggregate result."""
    if seeds is None:
        seeds = list(range(1, 11))

    orig_file = find_original(ds_path)
    if not orig_file:
        return None

    orig_headers, orig_rows = load_data(orig_file)
    if not orig_headers:
        return None

    seed_scores = []
    for seed in seeds:
        gen_file = find_generated(ds_path, seed)
        if not gen_file:
            continue
        gen_headers, gen_rows = load_data(gen_file)
        if not gen_headers:
            continue

        components = score_single(orig_headers, orig_rows, gen_headers, gen_rows)
        final = compute_final_score(components)
        seed_scores.append({'seed': seed, 'score': final, 'components': components})

    if not seed_scores:
        return None

    scores_list = [s['score'] for s in seed_scores]
    return {
        'name': os.path.basename(ds_path),
        'mean_score': round(sum(scores_list) / len(scores_list), 1),
        'min_score': min(scores_list),
        'max_score': max(scores_list),
        'std_dev': round((sum((s - sum(scores_list)/len(scores_list))**2 for s in scores_list) / len(scores_list))**0.5, 1),
        'seed_count': len(seed_scores),
        'per_seed': seed_scores,
    }


def main():
    parser = argparse.ArgumentParser(description='Score generated data quality')
    parser.add_argument('path', help='Dataset directory or parent directory')
    parser.add_argument('--seeds', nargs='+', type=int, default=list(range(1, 11)),
                       help='Seeds to score (default: 1-10)')
    parser.add_argument('--verbose', '-v', action='store_true',
                       help='Show per-component scores')
    parser.add_argument('--json', action='store_true',
                       help='Output as JSON')
    args = parser.parse_args()

    path = Path(args.path)

    # Determine if scoring one dataset or all
    if find_original(str(path)):
        # Single dataset
        datasets = [str(path)]
    else:
        # Directory of datasets
        datasets = sorted([
            str(path / d) for d in os.listdir(path)
            if os.path.isdir(path / d) and d != 'gen' and find_original(str(path / d))
        ])

    if not datasets:
        print(f"No scoreable datasets found in {path}")
        sys.exit(1)

    results = []
    for ds in datasets:
        result = score_dataset(ds, args.seeds)
        if result:
            results.append(result)

    if args.json:
        print(json.dumps(results, indent=2))
        return

    # Print table
    print(f"\n{'Dataset':<35} {'Mean':>5} {'Min':>4} {'Max':>4} {'StdDev':>6} {'Seeds':>5}")
    print("-" * 65)

    for r in sorted(results, key=lambda x: x['mean_score'], reverse=True):
        print(f"{r['name']:<35} {r['mean_score']:>5.1f} {r['min_score']:>4} {r['max_score']:>4} {r['std_dev']:>6.1f} {r['seed_count']:>5}")

    print("-" * 65)
    all_means = [r['mean_score'] for r in results]
    print(f"{'OVERALL':<35} {sum(all_means)/len(all_means):>5.1f}")
    print(f"\nDatasets: {len(results)}")
    print(f">=90: {sum(1 for m in all_means if m >= 90)}")
    print(f">=70: {sum(1 for m in all_means if m >= 70)}")
    print(f"<40:  {sum(1 for m in all_means if m < 40)}")


if __name__ == '__main__':
    main()

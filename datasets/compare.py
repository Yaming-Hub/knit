"""Compare original CSV vs knit-generated CSV and produce a report."""
import csv
import statistics
import sys
import re
from collections import Counter
from pathlib import Path


def read_csv(path):
    with open(path, "r", encoding="utf-8", errors="replace") as f:
        return list(csv.DictReader(f))


def analyze(rows, col):
    vals = [r[col] for r in rows if r.get(col)]
    if not vals:
        return {"type": "empty", "count": 0}

    # Try numeric
    nums = []
    for v in vals:
        try:
            nums.append(float(v))
        except (ValueError, TypeError):
            pass

    if len(nums) > len(vals) * 0.8:
        return {
            "type": "numeric",
            "count": len(nums),
            "min": min(nums),
            "max": max(nums),
            "mean": statistics.mean(nums),
            "std": statistics.stdev(nums) if len(nums) > 1 else 0,
            "unique": len(set(nums)),
        }

    # Categorical
    counter = Counter(vals)
    return {
        "type": "categorical",
        "count": len(vals),
        "unique": len(counter),
        "top3": counter.most_common(3),
    }


def compare(orig_path, gen_path):
    orig = read_csv(orig_path)
    gen = read_csv(gen_path)

    report = []
    report.append(f"Rows: orig={len(orig)}, gen={len(gen)}")
    
    if not orig:
        report.append("ERROR: Original dataset is empty")
        return "\n".join(report)

    cols = list(orig[0].keys())
    gen_cols = list(gen[0].keys()) if gen else []
    report.append(f"Columns: orig={len(cols)}, gen={len(gen_cols)}")

    if set(cols) != set(gen_cols):
        report.append(f"MISMATCH: orig cols={cols}, gen cols={gen_cols}")

    issues = []
    for col in cols:
        if col not in gen_cols:
            continue
        oa = analyze(orig, col)
        ga = analyze(gen, col)

        if oa["type"] == "numeric" and ga["type"] == "numeric":
            range_ok = abs(oa["mean"] - ga["mean"]) < max(abs(oa["std"]) * 0.5, 1)
            if not range_ok:
                issues.append(f"{col}: mean drift orig={oa['mean']:.2f} gen={ga['mean']:.2f}")
            report.append(
                f"  {col} (numeric): orig=[{oa['min']:.2f},{oa['max']:.2f}] mean={oa['mean']:.2f} | "
                f"gen=[{ga['min']:.2f},{ga['max']:.2f}] mean={ga['mean']:.2f}"
            )
        elif oa["type"] == "categorical" and ga["type"] == "categorical":
            ratio = ga["unique"] / max(oa["unique"], 1)
            if ratio < 0.5:
                issues.append(f"{col}: unique values dropped from {oa['unique']} to {ga['unique']}")
            report.append(
                f"  {col} (cat): orig_unique={oa['unique']} gen_unique={ga['unique']}"
            )
        else:
            report.append(f"  {col}: orig_type={oa['type']} gen_type={ga['type']}")

    if issues:
        report.append("\nISSUES:")
        for i in issues:
            report.append(f"  - {i}")
    else:
        report.append("\nNo significant issues detected.")

    return "\n".join(report)


if __name__ == "__main__":
    orig = sys.argv[1]
    gen = sys.argv[2]
    print(compare(orig, gen))

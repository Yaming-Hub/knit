#!/usr/bin/env python3
"""AI quality scoring for generated vs original datasets.

Reads pairs of (original, generated) CSV/JSON files and uses an AI model
to rate similarity on a 0-100 scale.
"""

import os
import sys
import json
import subprocess
import csv
from pathlib import Path

DATASETS_DIR = Path(r"Q:\repos\knit\datasets")
MAX_ROWS = 30  # Max rows to show AI from each file


def read_sample(filepath, max_rows=MAX_ROWS):
    """Read first N lines of a file as text."""
    try:
        with open(filepath, "r", encoding="utf-8", errors="replace") as f:
            lines = []
            for i, line in enumerate(f):
                if i >= max_rows + 1:  # +1 for header
                    break
                lines.append(line.rstrip())
            return "\n".join(lines)
    except Exception as e:
        return f"ERROR: {e}"


def find_datasets():
    """Find all datasets with both original and generated files."""
    results = []
    for d in sorted(DATASETS_DIR.iterdir()):
        if not d.is_dir():
            continue
        # Find original
        orig = None
        for ext in ["original.csv", "original.json", "original.jsonl"]:
            p = d / ext
            if p.exists():
                orig = p
                break
        # Find generated
        gen = d / "generated.csv" / "original.csv"
        if not gen.exists():
            gen = d / "generated.csv"
            if not gen.exists() or gen.is_dir():
                continue

        if orig and gen.exists():
            results.append((d.name, orig, gen))
    return results


def score_batch_with_model(batch, model="gpt-4.1"):
    """Score a batch of datasets using GitHub Copilot API via gh CLI."""
    prompt = """You are a data quality evaluator. For each dataset pair below, rate the synthetic (generated) data's similarity to the original on a 0-100 scale.

Criteria:
- Column names and types match (10 pts)
- Value ranges and distributions are realistic (25 pts)  
- Categorical values are plausible/from same domain (25 pts)
- Relationships between columns are preserved (20 pts)
- Overall structural fidelity (row count ratio, format) (20 pts)

Return ONLY a JSON array with objects: {"dataset": "<name>", "score": <0-100>, "reason": "<brief explanation>"}

"""
    for name, orig_text, gen_text in batch:
        prompt += f"\n--- Dataset: {name} ---\nORIGINAL (sample):\n{orig_text}\n\nGENERATED (sample):\n{gen_text}\n"

    prompt += "\n\nReturn ONLY the JSON array, no other text."
    
    # Write prompt to temp file to avoid shell escaping issues
    prompt_file = Path(r"Q:\repos\knit\scripts\scoring_prompt.txt")
    prompt_file.write_text(prompt, encoding="utf-8")
    
    # Call via gh copilot or direct API
    # Using a simple approach: write to file, call model
    return prompt, model


def main():
    datasets = find_datasets()
    print(f"Found {len(datasets)} dataset pairs to score")
    
    # Prepare all samples
    samples = []
    for name, orig_path, gen_path in datasets:
        orig_text = read_sample(orig_path)
        gen_text = read_sample(gen_path)
        samples.append((name, orig_text, gen_text))
    
    # Write samples to a JSON file for batch processing
    output = []
    for name, orig_text, gen_text in samples:
        output.append({
            "dataset": name,
            "original_sample": orig_text,
            "generated_sample": gen_text,
        })
    
    out_path = Path(r"Q:\repos\knit\scripts\scoring_input.json")
    with open(out_path, "w", encoding="utf-8") as f:
        json.dump(output, f, indent=2)
    
    print(f"Wrote {len(output)} samples to {out_path}")
    print("Ready for AI scoring.")


if __name__ == "__main__":
    main()

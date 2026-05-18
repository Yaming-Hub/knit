#!/usr/bin/env python3
"""Split scoring data into batch prompt files."""
import json
from pathlib import Path

data = json.load(open(r"Q:\repos\knit\scripts\scoring_input.json"))
batches = [data[i:i+10] for i in range(0, len(data), 10)]

for i, batch in enumerate(batches):
    prompt_parts = []
    for item in batch:
        orig = item["original_sample"][:800]
        gen = item["generated_sample"][:800]
        prompt_parts.append(
            f"--- Dataset: {item['dataset']} ---\n"
            f"ORIGINAL (sample):\n{orig}\n\n"
            f"GENERATED (sample):\n{gen}\n"
        )
    full_prompt = "\n".join(prompt_parts)
    Path(f"Q:/repos/knit/scripts/batch_{i}.txt").write_text(full_prompt, encoding="utf-8")

print(f"Wrote {len(batches)} batch files")

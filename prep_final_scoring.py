import os, json, random

datasets_dir = 'datasets'

def sample_csv(path, max_rows=12):
    with open(path, 'r', encoding='utf-8', errors='replace') as f:
        lines = f.readlines()
    if len(lines) <= 1:
        return ''
    header = lines[0].strip()[:300]
    data_lines = lines[1:]
    if len(data_lines) > max_rows:
        n = len(data_lines)
        safe_start = min(4, n)
        safe_end = max(safe_start, n - 4)
        mid = random.sample(range(safe_start, safe_end), min(4, safe_end - safe_start)) if safe_end > safe_start else []
        indices = list(range(safe_start)) + sorted(mid) + list(range(max(safe_end, n-4), n))
        indices = sorted(set(indices))[:max_rows]
        sampled = [data_lines[i].strip()[:300] for i in indices]
    else:
        sampled = [l.strip()[:300] for l in data_lines]
    return header + '\n' + '\n'.join(sampled)

# Find all datasets with generated output
all_datasets = sorted([d for d in os.listdir(datasets_dir)
                      if os.path.isdir(os.path.join(datasets_dir, d))
                      and d != 'gen'
                      and os.path.isfile(os.path.join(datasets_dir, d, 'gen', 'output', 'original.csv'))])

# For non-CSV datasets, convert original to CSV using knit generate from their blueprint.knit.toml
# Actually simpler: just use the gen output as both reference (since full-row dict = exact copy)
# For CSV datasets, use the actual original

prompts = []
for bi in range(0, len(all_datasets), 10):
    batch = all_datasets[bi:bi+10]
    parts = []
    for name in batch:
        gen_csv = os.path.join(datasets_dir, name, 'gen', 'output', 'original.csv')
        
        # Check for CSV original
        orig_csv = os.path.join(datasets_dir, name, 'original.csv')
        if os.path.isfile(orig_csv):
            orig_sample = sample_csv(orig_csv)
        else:
            # For JSON/parquet originals, skip comparison (we can't fairly compare formats)
            orig_sample = None
        
        gen_sample = sample_csv(gen_csv)
        
        if orig_sample:
            parts.append(f"### Dataset: {name}\nORIGINAL:\n```\n{orig_sample}\n```\nGENERATED:\n```\n{gen_sample}\n```")
        else:
            # For non-CSV, just show generated and note it's from JSON/parquet source
            parts.append(f"### Dataset: {name}\n(Source: non-CSV format, generated from learned model)\nGENERATED:\n```\n{gen_sample}\n```\nScore based on: Does this look like realistic, coherent data for this domain?")
    
    prompt = "\n\n---\n\n".join(parts)
    with open(f'score_final_{bi//10}.txt', 'w', encoding='utf-8') as f:
        f.write(prompt)
    print(f"Batch {bi//10}: {len(batch)} datasets, {len(prompt)} chars")

print(f"\nTotal: {len(all_datasets)} datasets")

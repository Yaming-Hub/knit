import os, json, random

datasets_dir = 'datasets'
# Find all datasets that have gen/output/original.csv
all_datasets = sorted([d for d in os.listdir(datasets_dir)
                      if os.path.isdir(os.path.join(datasets_dir, d))
                      and d != 'gen'
                      and os.path.isfile(os.path.join(datasets_dir, d, 'gen', 'output', 'original.csv'))])

def get_source_file(name):
    base = os.path.join(datasets_dir, name)
    for ext in ['csv', 'json', 'parquet', 'tsv']:
        path = os.path.join(base, f'original.{ext}')
        if os.path.isfile(path):
            return path
    return None

def sample_csv(path, max_rows=12):
    with open(path, 'r', encoding='utf-8', errors='replace') as f:
        lines = f.readlines()
    if len(lines) <= 1:
        return ''
    header = lines[0].strip()[:300]
    data_lines = lines[1:]
    if len(data_lines) > max_rows:
        n = len(data_lines)
        indices = list(range(min(4, n))) + random.sample(range(4, n-4), min(4, n-8)) + list(range(n-4, n))
        indices = sorted(set(indices))[:max_rows]
        sampled = [data_lines[i].strip()[:300] for i in indices]
    else:
        sampled = [l.strip()[:300] for l in data_lines]
    return header + '\n' + '\n'.join(sampled)

# For JSON/parquet originals, use the generated CSV as reference for "original"
# since we need comparable format
results = []
for name in all_datasets:
    gen_csv = os.path.join(datasets_dir, name, 'gen', 'output', 'original.csv')
    src = get_source_file(name)
    
    # For CSV sources, sample directly
    if src and src.endswith('.csv'):
        orig_sample = sample_csv(src)
    else:
        # For JSON/parquet, we can't easily sample text — use the gen output structure
        # but note this in the scoring
        orig_sample = f"[Non-CSV source: {os.path.basename(src) if src else 'unknown'}]"
    
    gen_sample = sample_csv(gen_csv)
    results.append({'name': name, 'orig': orig_sample, 'gen': gen_sample})

# Write batches of 10
batch_size = 10
for bi in range(0, len(results), batch_size):
    batch = results[bi:bi+batch_size]
    with open(f'score_full_{bi//batch_size}.json', 'w', encoding='utf-8') as f:
        json.dump(batch, f)
    print(f"Batch {bi//batch_size}: {[d['name'] for d in batch]}")

print(f"\nTotal: {len(results)} datasets in {(len(results)+batch_size-1)//batch_size} batches")

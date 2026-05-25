import os, json, random

datasets_dir = 'datasets'
datasets = sorted([d for d in os.listdir(datasets_dir)
                  if os.path.isfile(os.path.join(datasets_dir, d, 'original.csv'))
                  and os.path.isfile(os.path.join(datasets_dir, d, 'gen', 'output', 'original.csv'))])

def sample_csv(path, max_rows=15):
    with open(path, 'r', encoding='utf-8', errors='replace') as f:
        lines = f.readlines()
    if len(lines) <= 1:
        return ''
    header = lines[0].strip()
    data_lines = lines[1:]
    if len(data_lines) > max_rows:
        # First 5, 5 random middle, last 5
        mid_start = 5
        mid_end = len(data_lines) - 5
        if mid_end > mid_start:
            mid_indices = random.sample(range(mid_start, mid_end), min(5, mid_end - mid_start))
        else:
            mid_indices = []
        indices = list(range(5)) + sorted(mid_indices) + list(range(len(data_lines)-5, len(data_lines)))
        indices = sorted(set(indices))[:max_rows]
        sampled = [data_lines[i].strip() for i in indices]
    else:
        sampled = [l.strip() for l in data_lines]
    # Truncate each line to 200 chars
    sampled = [l[:200] for l in sampled]
    return header[:200] + '\n' + '\n'.join(sampled)

# Build scoring data
all_data = []
for name in datasets:
    orig = sample_csv(os.path.join(datasets_dir, name, 'original.csv'))
    gen = sample_csv(os.path.join(datasets_dir, name, 'gen', 'output', 'original.csv'))
    all_data.append({'name': name, 'orig': orig, 'gen': gen})

# Write 6 batches of 10
for bi in range(6):
    batch = all_data[bi*10:(bi+1)*10]
    with open(f'score_data_{bi}.json', 'w', encoding='utf-8') as f:
        json.dump(batch, f)
    total_chars = sum(len(d['orig']) + len(d['gen']) for d in batch)
    print(f"Batch {bi}: {len(batch)} datasets, {total_chars} chars")

import json

for i in range(6):
    with open(f'scripts/scoring_batch_{i}.json') as f:
        batch = json.load(f)
    
    prompt_parts = []
    for item in batch:
        ds = item['dataset']
        orig = item['original_sample'][:2000]
        gen = item['generated_sample'][:2000]
        orig_rows = item['original_rows']
        gen_rows = item['generated_rows']
        part = (
            f"### Dataset: {ds}\n"
            f"Original rows: {orig_rows}, Generated rows: {gen_rows}\n\n"
            f"**Original (first 30 rows):**\n```\n{orig}\n```\n\n"
            f"**Generated (first 30 rows):**\n```\n{gen}\n```\n"
        )
        prompt_parts.append(part)
    
    full_prompt = "\n---\n".join(prompt_parts)
    with open(f'scripts/batch_{i}.txt', 'w', encoding='utf-8') as f:
        f.write(full_prompt)
    print(f"Batch {i}: {len(full_prompt)} chars")

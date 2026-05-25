import json

for bi in range(6):
    data = json.load(open(f'score_batch_{bi}.json'))
    prompt_parts = []
    for d in data:
        part = f"### Dataset: {d['name']}\n\nORIGINAL:\n```\n{d['original']}\n```\n\nGENERATED:\n```\n{d['generated']}\n```\n"
        prompt_parts.append(part)
    prompt = "\n---\n".join(prompt_parts)
    with open(f'score_prompt_{bi}.txt', 'w', encoding='utf-8') as f:
        f.write(prompt)
    print(f"Batch {bi}: {len(prompt)} chars")

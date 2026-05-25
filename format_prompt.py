import json, sys

bi = int(sys.argv[1])
data = json.load(open(f'score_data_{bi}.json'))

parts = []
for d in data:
    parts.append(f"### Dataset: {d['name']}\nORIGINAL:\n```\n{d['orig']}\n```\nGENERATED:\n```\n{d['gen']}\n```")

prompt = """You are evaluating synthetic data quality. For each of the 10 datasets below, compare the GENERATED data against the ORIGINAL data and rate it 1-100.

Rate based on:
1. Column value realism (do values look like real data for this domain?)
2. Distribution fidelity (similar ranges, proportions, patterns as original?)
3. Row coherence (do column values within a row make sense together?)
4. Structural correctness (right data types, no garbled text, proper formatting?)
5. Domain authenticity (would this fool a domain expert at first glance?)

RESPOND WITH ONLY a JSON array: [{"dataset": "name", "score": N, "reason": "one sentence"}, ...]

""" + "\n\n---\n\n".join(parts)

with open(f'score_prompt_full_{bi}.txt', 'w', encoding='utf-8') as f:
    f.write(prompt)
print(f"Batch {bi}: {len(prompt)} chars, {len(data)} datasets")

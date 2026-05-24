import os, json, sys, time, random
from openai import OpenAI

client = OpenAI(api_key=os.environ.get("OPENAI_API_KEY"))

def sample_csv(path, max_rows=30):
    """Read CSV, return header + up to max_rows sample rows."""
    with open(path, 'r', encoding='utf-8', errors='replace') as f:
        lines = f.readlines()
    if len(lines) <= 1:
        return ""
    header = lines[0].strip()
    data_lines = lines[1:]
    if len(data_lines) > max_rows:
        # Take first 10, last 10, and 10 random from middle
        indices = list(range(10)) + random.sample(range(10, len(data_lines)-10), min(10, len(data_lines)-20)) + list(range(len(data_lines)-10, len(data_lines)))
        indices = sorted(set(indices))[:max_rows]
        sampled = [data_lines[i].strip() for i in indices]
    else:
        sampled = [l.strip() for l in data_lines]
    return header + "\n" + "\n".join(sampled)

def score_dataset(name, orig_path, gen_path):
    orig_sample = sample_csv(orig_path)
    gen_sample = sample_csv(gen_path)
    
    if not orig_sample or not gen_sample:
        return None, "empty"
    
    prompt = f"""You are evaluating synthetic data quality. Compare the GENERATED dataset against the ORIGINAL dataset.

ORIGINAL dataset "{name}" (sample rows):
```
{orig_sample}
```

GENERATED dataset "{name}" (sample rows):
```
{gen_sample}
```

Rate the generated data on a scale of 1-100 based on:
1. Column value realism (do values look like real data for this domain?)
2. Distribution fidelity (similar ranges, proportions, patterns as original?)
3. Row coherence (do column values within a row make sense together?)
4. Structural correctness (right data types, no garbled text, proper formatting?)
5. Domain authenticity (would this fool a domain expert at first glance?)

Respond with ONLY a JSON object: {{"score": <number>, "reason": "<one sentence>"}}"""

    try:
        resp = client.chat.completions.create(
            model="gpt-4.1",
            messages=[{"role": "user", "content": prompt}],
            temperature=0.1,
            max_tokens=150
        )
        text = resp.choices[0].message.content.strip()
        # Parse JSON from response
        if text.startswith("```"):
            text = text.split("\n", 1)[1].rsplit("```", 1)[0].strip()
        data = json.loads(text)
        return data["score"], data["reason"]
    except Exception as e:
        return None, str(e)

def main():
    datasets_dir = "datasets"
    datasets = sorted([d for d in os.listdir(datasets_dir) 
                      if os.path.isfile(os.path.join(datasets_dir, d, "original.csv"))
                      and os.path.isfile(os.path.join(datasets_dir, d, "test_output", "original.csv"))])
    
    results = []
    for i, name in enumerate(datasets):
        orig = os.path.join(datasets_dir, name, "original.csv")
        gen = os.path.join(datasets_dir, name, "test_output", "original.csv")
        score, reason = score_dataset(name, orig, gen)
        results.append({"dataset": name, "score": score, "reason": reason})
        if (i+1) % 10 == 0:
            print(f"Progress: {i+1}/{len(datasets)}", file=sys.stderr)
        time.sleep(0.3)  # rate limit
    
    # Output results
    scores = [r["score"] for r in results if r["score"] is not None]
    print(f"\n{'='*60}")
    print(f"RESULTS: {len(scores)} datasets scored")
    print(f"Mean: {sum(scores)/len(scores):.1f}")
    print(f"Median: {sorted(scores)[len(scores)//2]}")
    print(f"Min: {min(scores)} | Max: {max(scores)}")
    print(f"{'='*60}")
    
    # Show bottom 10
    scored = [r for r in results if r["score"] is not None]
    scored.sort(key=lambda x: x["score"])
    print(f"\nBOTTOM 10:")
    for r in scored[:10]:
        print(f"  {r['score']:3d} | {r['dataset']:30s} | {r['reason']}")
    
    print(f"\nTOP 10:")
    for r in scored[-10:]:
        print(f"  {r['score']:3d} | {r['dataset']:30s} | {r['reason']}")
    
    # Save full results
    with open("scoring_results.json", "w") as f:
        json.dump(results, f, indent=2)
    print(f"\nFull results saved to scoring_results.json")

if __name__ == "__main__":
    main()

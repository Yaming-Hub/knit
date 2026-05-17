# Benchmarks

Performance baseline for Knit's generation pipeline, measured with
[Criterion.rs](https://github.com/bheisler/criterion.rs).

## Running Benchmarks

```bash
cargo bench --bench generation
```

## Benchmark Descriptions

| Benchmark | What It Measures | Blueprint |
|-----------|-----------------|-----------|
| `numeric_generation_throughput` | 100K rows, single float column (normal distribution) | Measures raw numeric generation speed |
| `string_generation_throughput` | 100K rows, single string column (pattern generator) | Measures string allocation + pattern expansion |
| `fk_resolution_throughput` | 10K parent + 100K child rows with FK resolution | Measures topological ordering + key store lookups |
| `expression_evaluation_throughput` | 50K rows with a derived expression (`quantity * unit_price * (1 + tax_rate)`) | Measures expression parse + vectorized Arrow eval |
| `multi_entity_pipeline` | 3 entities (1K + 500 + 10K rows), mixed generators | Measures full pipeline with phase ordering |

## Baseline Results

> **Environment:** Windows 11, AMD Ryzen 9 7950X, 64 GB RAM, Rust 1.92 (release profile).
> Results are median values from 100 samples each.

| Benchmark | Time | Throughput |
|-----------|------|------------|
| `numeric_generation_throughput` | 1.15 ms | ~87M rows/s |
| `string_generation_throughput` | 11.3 ms | ~8.8M rows/s |
| `fk_resolution_throughput` | 8.9 ms | ~12.4M rows/s (110K total rows) |
| `expression_evaluation_throughput` | 2.4 ms | ~21M rows/s |
| `multi_entity_pipeline` | 2.1 ms | ~5.5M rows/s (11.5K total rows) |

## Notes

- These benchmarks measure the **full pipeline** (parse, validate, compile,
  and generate) but exclude serialization and IO.
- Throughput numbers are approximate and vary by machine, OS, and background load.
- Run `cargo bench` locally to establish your own baseline before optimizing.
- Criterion stores history in `target/criterion/` for regression detection.

use criterion::{Criterion, criterion_group, criterion_main};
use knit::learn::temporal::{
    autocorrelation, day_of_week_distribution, detect_temporal_pattern, hour_of_day_distribution,
};
use std::hint::black_box;

/// Generate fixed-interval timestamps with tiny jitter.
fn fixed_interval_timestamps(n: usize, interval: f64) -> Vec<f64> {
    (0..n)
        .map(|i| 1_700_000_000.0 + i as f64 * interval + (i % 7) as f64 * 0.1)
        .collect()
}

/// Generate timestamps with a sinusoidal rate modulation.
fn periodic_timestamps(n: usize, base_interval: f64, period_samples: usize) -> Vec<f64> {
    let mut ts = Vec::with_capacity(n);
    let mut t = 1_700_000_000.0;
    for i in 0..n {
        ts.push(t);
        let modulation =
            0.5 * (2.0 * std::f64::consts::PI * i as f64 / period_samples as f64).sin();
        t += base_interval * (1.0 + modulation);
    }
    ts
}

/// Generate timestamps with accelerating rate (trend).
fn trending_timestamps(n: usize) -> Vec<f64> {
    let mut ts = Vec::with_capacity(n);
    let mut t = 0.0;
    for i in 0..n {
        ts.push(t);
        t += (100.0 - 0.4 * i as f64).max(0.5);
    }
    ts
}

fn bench_detect_fixed_interval(c: &mut Criterion) {
    let ts = fixed_interval_timestamps(1000, 60.0);
    c.bench_function("temporal_detect_fixed_1k", |b| {
        b.iter(|| black_box(detect_temporal_pattern(black_box(&ts))));
    });
}

fn bench_detect_periodic(c: &mut Criterion) {
    let ts = periodic_timestamps(1000, 60.0, 24);
    c.bench_function("temporal_detect_periodic_1k", |b| {
        b.iter(|| black_box(detect_temporal_pattern(black_box(&ts))));
    });
}

fn bench_detect_trending(c: &mut Criterion) {
    let ts = trending_timestamps(1000);
    c.bench_function("temporal_detect_trending_1k", |b| {
        b.iter(|| black_box(detect_temporal_pattern(black_box(&ts))));
    });
}

fn bench_detect_large(c: &mut Criterion) {
    let ts = fixed_interval_timestamps(10_000, 60.0);
    c.bench_function("temporal_detect_fixed_10k", |b| {
        b.iter(|| black_box(detect_temporal_pattern(black_box(&ts))));
    });
}

fn bench_dow_distribution(c: &mut Criterion) {
    let ts = fixed_interval_timestamps(10_000, 3600.0);
    c.bench_function("temporal_dow_10k", |b| {
        b.iter(|| black_box(day_of_week_distribution(black_box(&ts))));
    });
}

fn bench_hod_distribution(c: &mut Criterion) {
    let ts = fixed_interval_timestamps(10_000, 3600.0);
    c.bench_function("temporal_hod_10k", |b| {
        b.iter(|| black_box(hour_of_day_distribution(black_box(&ts))));
    });
}

fn bench_autocorrelation(c: &mut Criterion) {
    let series: Vec<f64> = (0..10_000)
        .map(|i| (2.0 * std::f64::consts::PI * i as f64 / 24.0).sin())
        .collect();
    c.bench_function("temporal_autocorrelation_10k", |b| {
        b.iter(|| black_box(autocorrelation(black_box(&series), 24)));
    });
}

criterion_group!(
    benches,
    bench_detect_fixed_interval,
    bench_detect_periodic,
    bench_detect_trending,
    bench_detect_large,
    bench_dow_distribution,
    bench_hod_distribution,
    bench_autocorrelation,
);
criterion_main!(benches);

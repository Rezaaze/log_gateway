use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use log_gateway::cache::SemanticCache;
use log_gateway::redactor::Redactor;

// ── Redactor Benchmarks ───────────────────────────────────────────────────────

fn bench_redactor_no_pii(c: &mut Criterion) {
    let redactor = Redactor::new();
    let input = "The quick brown fox jumps over the lazy dog";

    c.bench_function("redactor_no_pii", |b| {
        b.iter(|| redactor.redact(criterion::black_box(input)))
    });
}

fn bench_redactor_with_email(c: &mut Criterion) {
    let redactor = Redactor::new();
    let input = "User logged in: alice@example.com from office";

    c.bench_function("redactor_email", |b| {
        b.iter(|| redactor.redact(criterion::black_box(input)))
    });
}

fn bench_redactor_multi_pii(c: &mut Criterion) {
    let redactor = Redactor::new();
    let input = "User alice@example.com (SSN 123-45-6789) connected from 192.168.1.100";

    c.bench_function("redactor_multi_pii", |b| {
        b.iter(|| redactor.redact(criterion::black_box(input)))
    });
}

fn bench_redactor_throughput(c: &mut Criterion) {
    let redactor = Redactor::new();
    let inputs: Vec<String> = vec![
        "plain log message without any sensitive data".to_string(),
        "user@example.com logged in successfully".to_string(),
        "payment 4111-1111-1111-1111 processed for 192.168.0.1".to_string(),
        "SSN 123-45-6789 verified for account DE89370400440532013000".to_string(),
    ];

    let mut group = c.benchmark_group("redactor_throughput");
    for (i, input) in inputs.iter().enumerate() {
        group.throughput(Throughput::Bytes(input.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(i), input, |b, s| {
            b.iter(|| redactor.redact(criterion::black_box(s.as_str())))
        });
    }
    group.finish();
}

// ── Cache Key Benchmarks ──────────────────────────────────────────────────────

fn bench_cache_key_short(c: &mut Criterion) {
    c.bench_function("cache_key_short", |b| {
        b.iter(|| SemanticCache::make_key(criterion::black_box("hello world")))
    });
}

fn bench_cache_key_long(c: &mut Criterion) {
    let long_msg = "a".repeat(8192);
    c.bench_function("cache_key_long_8192", |b| {
        b.iter(|| SemanticCache::make_key(criterion::black_box(&long_msg)))
    });
}

fn bench_cache_key_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("cache_key_scaling");
    for size in [64usize, 256, 1024, 4096, 8192] {
        let input = "x".repeat(size);
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &input, |b, s| {
            b.iter(|| SemanticCache::make_key(criterion::black_box(s.as_str())))
        });
    }
    group.finish();
}

// ── Criterion Groups ──────────────────────────────────────────────────────────

criterion_group!(
    redactor_benches,
    bench_redactor_no_pii,
    bench_redactor_with_email,
    bench_redactor_multi_pii,
    bench_redactor_throughput,
);

criterion_group!(
    cache_benches,
    bench_cache_key_short,
    bench_cache_key_long,
    bench_cache_key_scaling,
);

criterion_main!(redactor_benches, cache_benches);

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use log_gateway::cache::SemanticCache;
use log_gateway::redactor::Redactor;
use log_gateway::rpki_cache::RpkiCache;

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

// ── RPKI Cache Validation Benchmarks ──────────────────────────────────────────

fn bench_rpki_validate(c: &mut Criterion) {
    use std::collections::HashMap;
    use std::net::IpAddr;

    // Setup: 1000 VRPs in the index (simulates real-world density)
    // Key: (prefix_len: u8, network_addr: u128)
    // Use tokio Runtime to populate (block_on)
    let mut index = HashMap::new();
    for i in 0u32..1000 {
        let a = (i / 256) as u8;
        let b = (i % 256) as u8;
        let addr: IpAddr = format!("10.{}.{}.0", a, b).parse().unwrap();
        let network_u128 = match addr {
            IpAddr::V4(v4) => v4.to_ipv6_mapped().to_bits(),
            IpAddr::V6(v6) => v6.to_bits(),
        };
        index.insert((24u8, network_u128), vec![(24u8, 64512u32)]);
    }
    // Add hierarchical VRPs:
    // 10.0.0.0/8 → (24, AS64512)
    let supernet: IpAddr = "10.0.0.0".parse().unwrap();
    let supernet_u128 = match supernet {
        IpAddr::V4(v4) => v4.to_ipv6_mapped().to_bits(),
        IpAddr::V6(v6) => v6.to_bits(),
    };
    index.insert((8u8, supernet_u128), vec![(24u8, 64512u32)]);

    let cache = RpkiCache::new("http://dummy".to_string());
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        cache.set_test_data(index).await;
    });

    let mut group = c.benchmark_group("rpki_validate");

    // Scenario 1: Valid — exact match
    // VRP: 192.0.2.0/24, max_length=24, AS64512
    // validate("192.0.2.0/24", 64512) → Valid
    group.bench_function("valid_exact_match", |b| {
        b.iter(|| {
            cache.validate(
                criterion::black_box("192.0.2.0/24"),
                criterion::black_box(64512),
            )
        })
    });

    // Scenario 2: NotFound — no VRP present
    // validate("10.99.99.0/24", 64512) → NotFound (Prefix not in index)
    group.bench_function("not_found", |b| {
        b.iter(|| {
            cache.validate(
                criterion::black_box("10.99.99.0/24"),
                criterion::black_box(64512),
            )
        })
    });

    // Scenario 3: Hierarchical — VRP on /8, Announcement on /24
    // Loop iterates through all 24 candidate lengths until match
    // VRP: 10.0.0.0/8, max_length=24, AS64512
    // validate("10.1.2.0/24", 64512) → Valid (after 16 iterations)
    group.bench_function("valid_hierarchical_lookup", |b| {
        b.iter(|| {
            cache.validate(
                criterion::black_box("10.1.2.0/24"),
                criterion::black_box(64512),
            )
        })
    });

    // Scenario 4: Throughput measurement with 4 different inputs
    // Throughput::Elements(1) for events/sec metric
    group.throughput(Throughput::Elements(1));
    group.bench_function("throughput_mixed", |b| {
        let inputs = [
            ("192.0.2.0/24", 64512u32),
            ("10.1.2.0/24", 64512u32),
            ("203.0.113.0/24", 99999u32),
            ("198.51.100.0/22", 64512u32),
        ];
        let mut i = 0usize;
        b.iter(|| {
            let (prefix, asn) = inputs[i % inputs.len()];
            i += 1;
            cache.validate(criterion::black_box(prefix), criterion::black_box(asn))
        })
    });

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

criterion_group!(rpki_benches, bench_rpki_validate);

criterion_main!(redactor_benches, cache_benches, rpki_benches);

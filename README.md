# Log Gateway

A production-grade, high-performance Rust log processing gateway — built as a drop-in replacement for slow Python log infrastructure.

## Features

- **PII Redaction** — 7 regex patterns (email, IPv4/IPv6, SSN, phone, credit card, IBAN) with early-exit heuristics
- **Semantic Cache** — moka `SegmentedCache` (32 segments, LRU + TTL) with SHA-256 keys
- **Per-Tenant Cost Tracking** — DashMap, `X-Tenant-ID` header isolation
- **Prometheus Metrics** — 6 counters + 1 histogram, `/metrics` endpoint
- **Storage Sink** — NDJSON buffering with optional zstd compression (lock-free `ArrayQueue`)
- **S3/MinIO Export** — `aws-sdk-s3`, background export task, `force_path_style`
- **Rate Limiting** — GCRA via `governor`, per-tenant isolation
- **Auth** — API key (`X-API-Key`) + JWT HS256 (`Authorization: Bearer`), both optional
- **TLS/HTTPS** — `axum-server` + rustls, configurable via `config/default.toml`
- **Hot-Reload** — `SIGHUP` reloads config without restart (Unix)
- **OpenAPI / Swagger UI** — `utoipa` + `utoipa-swagger-ui` at `/swagger-ui/`
- **Structured Logging** — JSON or text via `LOG_FORMAT` env var
- **Alerting** — Prometheus AlertManager with 5 alert rules + Slack routing

## Performance Optimizations

| # | Optimization | Impact |
|---|---|---|
| 1 | `mimalloc` global allocator | −15–30% allocation overhead |
| 1 | `SegmentedCache::builder(32)` | reduced lock contention on cache |
| 1 | `target-cpu=native` (`.cargo/config.toml`) | AVX2/SSE4 SIMD instructions |
| 1 | Redactor early-exit heuristics + `into_owned()` | −80% no-PII latency |
| 2 | Single-parse `ingest_log` (`Bytes` + `from_slice`) | eliminates double deserialization |
| 2 | Lock-free `ArrayQueue` sink (`crossbeam`) | zero-contention writes |
| 2 | `CompressionLayer` (Gzip) | response size reduction |
| 3 | `RwLock<Registry>` in metrics | concurrent Prometheus scrapes |
| 3 | `serde_json::to_writer` → `Vec<u8>` | no intermediate String allocation |
| 3 | zstd Level 1 (was 3) | −40% compress CPU |
| 3 | Tokio worker-count startup log | production observability |

## Benchmark Results (Hetzner ARM64, release build)

| Scenario | Req/s | p50 | p95 | p99 |
|---|---|---|---|---|
| `/health` baseline | 131.866 | 0.35 ms | 0.67 ms | 0.70 ms |
| Ingest — simple payload | 83.504 | 0.56 ms | 0.99 ms | 1.33 ms |
| Ingest — 5x PII (cache miss) | 61.155 | 0.75 ms | 1.40 ms | 2.73 ms |
| Ingest — 8192 char message | 49.685 | 0.91 ms | 1.69 ms | 3.79 ms |
| Ingest — 20KB metadata | 18.287 | 1.78 ms | 3.96 ms | 31.88 ms |
| 4-instance cluster (aggregate) | ~23.000 | — | — | — |

## Stack

```
Rust 2021 · Tokio · Axum 0.7 · Prometheus · Grafana · AlertManager · MinIO/S3
```

## Quick Start

```bash
# Run locally (no auth, no TLS)
cargo run

# Run tests (105 total)
cargo test

# Lint & format
cargo clippy --all-targets -- -D warnings
cargo fmt --check

# Criterion benchmarks (redactor + cache key)
cargo bench

# HTTP benchmark (requires oha: cargo install oha)
oha -n 10000 -c 50 --no-tui -m POST \
  -H "Content-Type: application/json" \
  -d '{"source":"bench","level":"info","message":"hello"}' \
  http://localhost:8080/api/v1/logs
```

## Docker Compose

### Development (single instance)

```bash
docker compose up -d
```

Services: `gateway` (8080) · `prometheus` (9090) · `alertmanager` (9093) · `grafana` (3000) · `minio` (9000/9001)

### Production (4-instance cluster)

```bash
# Build image + deploy cluster
./deploy-cluster.sh --build

# Rollback to single instance
./deploy-cluster.sh --rollback
```

Architecture:
```
Internet :8090
  └── nginx-lb  (ip_hash load balancer)
       ├── gateway_1  CPU 0
       ├── gateway_2  CPU 1
       ├── gateway_3  CPU 2
       └── gateway_4  CPU 3
```

- `ip_hash` routes each client IP to the same instance → maximizes per-instance cache hit rate
- Each instance pinned to one vCPU via `cpuset` → no migration overhead
- Only Nginx is externally reachable; gateways are isolated in `gateway-net`
- Prometheus scrapes all 4 instances individually with `instance` labels

## Configuration

All settings live in `config/default.toml` and can be overridden via `GATEWAY__*` environment variables (separator `__`).

```toml
[server]
host = "0.0.0.0"
port = 8080

[cache]
max_capacity = 10000
ttl_seconds = 300

[sink]
enabled = true
output_dir = "data/logs"
max_buffer_size = 1000
flush_interval_secs = 30
compress = true

[rate_limit]
enabled = true
requests_per_second = 100

[s3]
enabled = false
bucket = "logs"
# ...

[tls]
enabled = false
cert_path = "certs/cert.pem"
key_path  = "certs/key.pem"
```

## Authentication

| Method | Secret | Header |
|---|---|---|
| API Key | `GATEWAY_API_KEY` or `/run/secrets/gateway_api_key` | `X-API-Key: <key>` |
| JWT HS256 | `GATEWAY_JWT_SECRET` or `/run/secrets/gateway_jwt_secret` | `Authorization: Bearer <token>` |

Both are **optional** — omit the secret to disable the check.

## API

| Method | Path | Auth | Description |
|---|---|---|---|
| `POST` | `/api/v1/logs` | ✅ | Ingest a log entry (max 64KB body) |
| `GET` | `/api/v1/cache/stats` | ✅ | Cache hit/miss statistics |
| `GET` | `/api/v1/costs` | ✅ | Cost summary (all tenants) |
| `GET` | `/api/v1/costs/:tenant_id` | ✅ | Cost for a specific tenant |
| `POST` | `/api/v1/export/s3` | ✅ | Trigger S3 export |
| `GET` | `/health` | — | Health check |
| `GET` | `/metrics` | — | Prometheus metrics |
| `GET` | `/swagger-ui/` | — | OpenAPI UI |

### Example request

```bash
curl -X POST http://localhost:8080/api/v1/logs \
  -H "Content-Type: application/json" \
  -H "X-Tenant-ID: my-service" \
  -d '{"level":"info","source":"my-service","message":"User logged in"}'
```

## Log Entry Schema

```json
{
  "level":    "info | warn | error | debug",
  "source":   "string (1–128 chars)",
  "message":  "string (1–8192 chars)",
  "metadata": { }
}
```

## CI/CD

4 GitHub Actions jobs on every push/PR:

1. **test** — `cargo test` + `cargo build --release`
2. **lint** — `cargo fmt --check` + `cargo clippy --all-targets -- -D warnings`
3. **security-audit** — `cargo-audit`
4. **docker** — multi-arch (amd64 + arm64) build & push to GHCR (main branch only)

## Environment Variables

| Variable | Description |
|---|---|
| `GATEWAY_API_KEY` | API key for authentication |
| `GATEWAY_JWT_SECRET` | JWT signing secret |
| `GATEWAY__SERVER__PORT` | Override server port |
| `LOG_FORMAT` | `json` or `text` (default: `text`) |
| `TOKIO_WORKER_THREADS` | Override Tokio worker count (default: logical CPUs) |
| `RUST_LOG` | Log level filter (e.g. `info`, `debug`) |

## License

MIT

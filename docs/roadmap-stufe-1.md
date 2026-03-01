# Roadmap — Stufe 1: Ingestion & Storage

> **Status: ✅ ABGESCHLOSSEN**
> Zeitraum: abgeschlossen

---

## Ziel

Einen produktionsreifen, hochperformanten Log-Ingestion-Layer aufbauen, der als
Fundament für alle weiteren Stufen dient.

---

## 1.1 — Core Gateway (Axum / Rust)

- [x] HTTP-Server mit Axum 0.7
- [x] API-Endpunkte:
  - `POST /api/v1/logs` — Einzel-Ingest (65 KB max)
  - `POST /api/v1/logs/batch` — Batch-Ingest (4 MB max, 1000 Einträge)
  - `GET  /health` — Healthcheck (version, uptime, cache size)
  - `GET  /metrics` — Prometheus-Metriken
  - `GET  /swagger-ui/` — OpenAPI / Swagger UI
- [x] Datenmodell: `LogEntry` (id, timestamp, level, source, message, metadata)
- [x] Schema-Validierung (jsonschema, Lazy)
- [x] Request-ID (`X-Request-ID` Header in Responses)

## 1.2 — Authentifizierung & Sicherheit

- [x] API-Key-Authentifizierung (`X-API-Key` Header)
- [x] JWT-Validierung (HS256, `X-JWT-Subject` Header)
- [x] Docker Secrets-Integration (`/run/secrets/` + ENV-Fallback)
- [x] PII-Redaktion — 7 Muster: E-Mail, IPv4, IPv6, SSN, Telefon, Kreditkarte, IBAN
- [x] TLS/HTTPS-Support (axum-server + rustls, optional)

## 1.3 — Performance

- [x] MiMalloc als globaler Allocator
- [x] SemanticCache (moka SegmentedCache, 32 Segmente, SHA-256 Key)
- [x] Lock-freie `ArrayQueue` im StorageSink (kein Mutex auf Write-Pfad)
- [x] Stream-Serialisierung via `serde_json::to_writer()` (kein intermediate String)
- [x] zstd-Kompression Level 1 (`.ndjson.zst`)
- [x] Gzip `CompressionLayer` auf allen Responses
- [x] `RwLock<Registry>` für Metriken (parallele Reads bei Scraping)
- [x] Per-Tenant GCRA Rate-Limiting (governor + DashMap)

## 1.4 — Observability

- [x] 7 Prometheus-Metriken:
  - `gateway_requests_total`
  - `gateway_cache_hits_total` / `gateway_cache_misses_total`
  - `gateway_pii_hits_total`
  - `gateway_bytes_ingested_total`
  - `gateway_rate_limit_hits_total`
  - `gateway_request_duration_ms` (Histogram, 11 Buckets)
- [x] Strukturiertes Logging (JSON / Text via `LOG_FORMAT` env)
- [x] Per-Tenant Cost-Tracking (requests, bytes, PII-Hits, Cache-Performance)
- [x] Config Hot-Reload via SIGHUP
- [x] Grafana Dashboard (11 Panels, `gateway.json`)
- [x] Alertmanager (5 Alert-Rules)

## 1.5 — Storage

- [x] StorageSink → NDJSON-Dateien lokal (`data/logs/`)
- [x] Optionaler S3/MinIO-Export (`aws-sdk-s3`)
- [x] Graceful Flush bei SIGTERM/Ctrl+C

## 1.6 — Produktions-Cluster

- [x] HAProxy Load Balancer (Port 8090, `leastconn`)
- [x] 4× Gateway-Instanzen (CPU-pinned auf vCPU 0–3)
- [x] Prometheus + Grafana + Alertmanager + MinIO in Docker Compose
- [x] Healthcheck-Gate (alle 4 Gateways müssen `healthy` sein)

## 1.7 — BGP-Stream (Datenquelle)

- [x] Rust-Rewrite des Python-Skripts
- [x] RIPE NCC RIS Live WebSocket: `wss://ris-live.ripe.net/v1/ws/`
- [x] Batched POST → Gateway (100 Events / 20ms Timeout)
- [x] 8 Worker-Goroutinen, 64k Channel-Kapazität
- [x] rustls CryptoProvider (aws-lc-rs) explizit gesetzt
- [x] Docker Secrets-Integration (API-Key + JWT-Secret)
- [x] `network_mode: host` (outbound HTTPS via Host-Stack)
- [x] ~12.500 Events/s stabil, errors=0

## 1.8 — CI/CD

- [x] GitHub Actions (4 Jobs): `test`, `lint`, `security-audit`, `docker`
- [x] Native ARM64-Build auf `ubuntu-24.04-arm` (kein QEMU)
- [x] 2 Images: `ghcr.io/rezaaze/log_gateway:latest` + `.../bgp-stream:latest`
- [x] Auto-Deploy auf Hetzner `167.235.30.106` nach Push zu `main`
- [x] SSH-basierter Deploy mit Health-Gate (60s Timeout)

## 1.9 — Tests

- [x] 105+ Tests gesamt:
  - Unit-Tests (cache, cost_tracker, models, middleware, secrets, rate_limiter, redactor, …)
  - 9 Integrationstests
  - 9 Property-based Tests (proptest)
  - 27 Chaos/Concurrency/Security/Load-Tests (hardcore_test.rs)
- [x] 7 Criterion-Benchmarks

---

## Erreichtes Performance-Niveau

| Metrik | Wert |
|--------|------|
| Durchsatz (BGP-Stream) | ~12.500 Events/s |
| p50 Latenz | ~3.7 ms |
| p95 Latenz | ~9.5 ms |
| p99 Latenz | ~10–20 ms |
| Rate-Limit pro Instanz | 20.000 req/s (× 4 = 80.000 gesamt) |
| Fehlerrate | 0 |

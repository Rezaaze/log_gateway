# Log Gateway — Claude Session Context

Dieses Dokument beschreibt das Projekt vollständig, damit Claude in einer neuen Session sofort orientiert ist.

---

## ⚠️ Stand 31.08.2026 — dieses Dokument ist ein historischer Snapshot

Alles unten beschreibt den Zustand des reinen "Log Gateway" (105/105 Tests,
Milestones M1–M16, P1–P6). **Seit 08.03.2026 ist das Projekt zu "BGP
TrustWave" gewachsen** (Wellenphysik-Triangulation, Trust-Score-Engine,
RPKI/IRR-Validierung, NATS/ClickHouse-Streaming, ~44 Module in `src/`).
Maßgeblich für den aktuellen Stand ist **`TRUSTWAVE_ROADMAP.md`**, nicht
mehr primär dieses Dokument. `DEV_ROADMAP.md` ist ein verworfener
Alternativentwurf (siehe Hinweis am Dateianfang) — nicht verwenden.

**Verifiziert am 31.08.2026 (Session-Audit), zuletzt aktualisiert 01.09.2026:**
- Build/Tests laufen sauber: 271/272 Lib-Tests grün (1 Fehlschlag ist ein
  reines Sandbox-Artefakt: Test erwartet einen Permission-Fehler beim
  Schreiben nach `/root/...`, läuft dort aber als root). `cargo check
  --workspace` (alle Workspace-Member, nicht nur die Haupt-Crate) ist
  ebenfalls sauber.
  `cargo build`/`test` scheitern in dieser Remote-Sandbox NUR am
  Swagger-UI-Download in `utoipa-swagger-ui`'s build.rs (Netzwerk-Policy
  blockiert `github.com`-Archiv-Downloads, kein Code-Fehler) — Workaround:
  `SWAGGER_UI_DOWNLOAD_URL=file:///pfad/zu/vorgebautem-swagger-ui.zip`
  (siehe `build.rs` der Dependency; nicht in Git committed, nur lokaler
  Sandbox-Workaround).
- **Kritischer Fund + Fix:** Die live NATS-Pipeline (`detector_runner.rs`,
  gespeist aus dem NATS-Subscriber) erkannte Hijacks/Flapping korrekt, hat
  sie aber nie an `EscalationRouter` weitergereicht — erkannte Anomalien
  landeten nur in Tracing-Logs + Prometheus-Zählern, nie in ClickHouse
  `alert_history` und nie per Webhook/Slack. Der parallele HTTP-Ingest-Pfad
  (`rpki_tx` → `anomaly_detector::AnomalyDetector` → `run_alert_logger`)
  hatte Escalation korrekt verdrahtet — die beiden Pfade hatten
  unterschiedlich vollständige Detector-Instanzen. Fix: `DetectorRunner`
  bekommt jetzt per `.with_escalation(router)` denselben `EscalationRouter`
  wie der HTTP-Pfad; alle drei Erkennungsstellen in `process_record()`
  rufen `router.route(&anomaly)` auf. Neuer Regressionstest:
  `test_escalation_router_invoked_on_hijack_without_panicking`.
- **Toter Code bereinigt:** `src/wave_detector.rs` (ältere, nie
  `mod`-deklarierte Wave-Score-Implementierung mit hartkodierten
  Platzhalter-Konstanten statt echter Baseline-Statistik) wurde entfernt —
  ersetzt durch die neuere `src/wave_anomaly_detector.rs` (nutzt echten
  `z_score`, `p50`/`p99`-Baseline-Werte, Kollektor-Geodistanz), die jetzt
  als `pub mod` Teil der Crate ist (kompiliert, 5 Tests laufen in CI).
- **Update 01.09.2026 — Phase 3 (Wave Anomaly Detector) an Live-Pfad
  angebunden:** `DetectorRunner` speist jetzt jeden `announce`-Record in
  einen `PropagationAggregator`; abgeschlossene `PropagationEvent`s werden
  vom `WaveAnomalyDetector` bewertet, anomale Scores laufen über denselben
  `EscalationRouter` wie Hijack/Flapping. Ein Hintergrund-Task flusht alle
  2s Gruppen, deren Fenster ohne natürlichen Abschluss abgelaufen ist. Neue
  `[wave]`-Configsektion (`enabled`, `baseline_path`), standardmäßig aktiv
  und ungefährlich ohne vorhandene Baseline-Datei (Detector degradiert dann
  zu "keine Anomalien"). 2 neue Tests.
- **Update 01.09.2026 — `tools/baseline_builder/` repariert:** Beim Verifizieren
  mit `cargo check --workspace` (nicht nur `--lib`) stellte sich heraus, dass
  `tools/baseline_builder` gar nicht kompilierte — es referenzierte
  `wave_baseline::BaselineBuilder`/`save_baseline()`, die nicht existierten.
  Fix: `WaveBaseline::save()`/`load()` nutzen jetzt bincode+zstd statt JSON
  (Format-Konsistenz mit dem Live-Ladepfad ist kritisch); neuer
  `BaselineBuilder`-Accumulator nutzt den bereits vorhandenen, aber toten
  `percentile()`-Helper für echte p50/p95/p99. 4 neue Tests. Details siehe
  `TRUSTWAVE_ROADMAP.md` Phase 2.
- **Update 01.09.2026 — `src/detector_loop.rs` entfernt:** war eine zweite,
  vollständige Pipeline-Implementierung (eigene NATS-Subscription,
  RPKI→IRR→Hijack→Flapping→Dedup→Webhook), nirgends gespawnt. Durch den
  Escalation-Fix ist `detector_runner.rs` jetzt funktional gleichwertig
  (RPKI/IRR + Wave + Dedup/Webhook über `EscalationRouter`) und damit strikt
  überlegen. Entfernt inkl. `tests/e2e_detection_test.rs` und
  `examples/detector_loop_example.rs` — die zugrundeliegende Detection-Logik
  (`HijackDetector`, `FlappingDetector`, `DedupCache`) bleibt über eigene
  Modultests und `detector_runner.rs`s Testsuite abgedeckt.
- **Update 01.09.2026 — `tools/backtest/` gebaut (Abschnitt 3.2):** neues
  Workspace-Member, liest MRT-Archivdaten für ein per `case.toml` beschriebenes
  Hijack-Fenster, baut `PropagationEvent`s, bewertet sie mit
  `WaveAnomalyDetector`, berechnet TPR/FPR/Erkennungslatenz. Bewusst **keine**
  historischen Hijack-Parameter (Präfixe/ASNs/Zeitfenster) im Code
  hartkodiert — falsch aus dem Gedächtnis rekonstruiert wären sie ein reales
  Risiko; der Bediener befüllt eine `case.toml` anhand einer Primärquelle.
  Smoke-getestet gegen echte, frisch heruntergeladene RIPE-RIS-Archivdaten
  (3 Kollektoren, 01.01.2024, ~36MB): 859k reale BGP-Records geparst, 1837
  echte PropagationEvents gebaut, Report korrekt erzeugt. Dabei einen echten
  Bug im Leakage-Check gefunden und gefixt: `WaveBaseline::created_at` ist
  die Datei-Schreibzeit (heute), nicht das Alter der Quelldaten — für
  historisches Backtesting immer falsch. Neues Feld `data_cutoff_ts`
  (spätester Sample-Zeitstempel der Quelldaten, von `BaselineBuilder`
  getrackt) ersetzt `created_at` im Leakage-Check. 2 neue Tests. **Noch
  offen:** die drei konkreten Fallstudien (MyEtherWallet 2018, Pakistan
  Telecom 2008, Rostelecom 2020) mit verifizierten echten Parametern +
  mehrwöchige Vor-Hijack-Baseline-Daten — das ist der eigentlich große
  Download, nicht die ±2h Hijack-Daten. Details siehe `TRUSTWAVE_ROADMAP.md`
  Abschnitt 3.2.
- **Update 01.09.2026 — kritische Review der Erkennungslogik + 4 Fixes:**
  Auf explizite Anfrage ("ist die Funktion überhaupt sinnig?") wurde nicht
  nur die Verdrahtung, sondern die eigentliche Detection-Logik geprüft.
  Vier reale Funde, alle behoben bis auf den letzten (bewusst dokumentiert,
  kein Bugfix):
  1. **`HijackDetector`-Cold-Start:** `DetectorRunner`s eigene, nie
     aufgewärmte `HijackDetector`-Instanz flaggte jede Präfix-Erstsichtung
     als Hijack — bei jedem Neustart praktisch die gesamte sichtbare
     globale Routing-Tabelle. Live im eigenen Funktionstest reproduziert.
     Fix: geteilte, von `AnomalyDetector` per ClickHouse-Warmup vorbefüllte
     Instanz statt zweier getrennter Kopien.
  2. **Asymmetrischer Z-Score-Clamp:** `spread_z_score` clampte auf
     `[0.0, 1.0]` und verwarf damit jeden negativen Z-Score — genau das
     "kam verdächtig gleichzeitig an"-Muster, das namensgebend für das
     ganze Projekt ist. Fix: `abs(z)/3.0`, symmetrisch.
  3. **Totes Signal:** `calculate_order_entropy` berechnete
     `unique_count/max_possible`, wobei beide Werte immer identisch waren
     (`arrival_order.len()`) — lieferte konstant 1.0, keine echte
     Reihenfolgeprüfung. Fix: `WaveBaselineEntry.expected_order` (neues
     Feld, von `BaselineBuilder` per mittlerer Ankunfts-Rangfolge
     getrackt) + echte Überlapp-Berechnung, umbenannt zu `order_deviation`.
  4. **Anycast-Lücke (dokumentiert, nicht gelöst):** `propagation_speed`
     ist absolut/physikbasiert, nicht baseline-relativ — legitimes Anycast
     (Cloudflare 1.1.1.1, Google 8.8.8.8, DNS-Root-Server) sieht für dieses
     eine Signal strukturell wie ein Hijack aus. Der jetzt symmetrische
     `spread_z_score` mildert das für Präfixe mit Baseline-Historie
     deutlich, aber `propagation_speed` selbst bleibt ein bekanntes
     Restrisiko. Echte Lösung ist ein eigenes Vorhaben (Anycast-Allowlist
     o.ä.), kein Bugfix — siehe `TRUSTWAVE_ROADMAP.md` Abschnitt 3.1.
  6 neue Tests (2× Cold-Start-Sharing, 2× symmetrischer Z-Score/Order-
  Deviation, 1× Baseline-Order-Tracking bereits mit Fix 3 mitgeliefert).
  Details siehe `TRUSTWAVE_ROADMAP.md` Phase 3.

- **Update 08.09.2026 — Projektbewertung + vier Bugfixes:** Auf die Frage, ob
  sich die Weiterarbeit lohnt, wurde die Wellenphysik-Hypothese nicht nur im
  Code, sondern an echten RIPE-Daten gemessen (2 Min Live-Stream, 1,95 Mio
  Announcements; 5 MRT-Archive, 1,5 Mio Records). Ergebnis und Empfehlung
  stehen in **`PROJEKT_BEWERTUNG.md`** — Kurzfassung: der RPKI/IRR-Teil trägt,
  die Wellenphysik in der aktuellen Form nicht (gemessener Spread p50 = 1000 ms
  statt ~100 ms, nur 3,5 % der Ankündigungen von ≥3 Kollektoren mit gleichem
  Pfad gesehen, MRT-Archive ohne Sub-Sekunden-Auflösung, zwei der fünf Signale
  praktisch konstant). Vier daraus gefundene Bugs sind behoben:
  1. **`tools/bgp_stream` las den Kollektor aus `data.id`** — das ist im
     RIS-Live-Stream eine pro Nachricht eindeutige ID, der Kollektor steht in
     `data.host`. Jedes Record bekam damit einen eigenen "Kollektor"; die
     gesamte Triangulation lief live auf Unsinn. Fix: `host` + neuer
     `collector_from_host()`.
  2. **Baseline-Schlüssel passte nicht zum Live-Schlüssel** —
     `baseline_builder` schrieb Events mit `as_path = vec![origin_as]`, der
     Live-Pfad suchte über den vollen Pfad-Hash. `find_entry()` traf nie, der
     Wave-Detektor fiel still auf `Normal` zurück. Fix: **ein** gemeinsamer
     `propagation::path_hash()` (vorher drei Kopien) + voller AS-Pfad im Event.
  3. **Batch-Tools hatten kein Zeitfenster** — Spreads bis 293 s aus
     verschmolzenen, unabhängigen Ankündigungen. Fix: gemeinsame
     `propagation::build_events_batch()` mit Sessionisierung wie im
     Live-Aggregator, `--window-secs` (Default 10 s). Verifiziert gegen echte
     Archivdaten: kein Baseline-Eintrag mehr über dem Fenster.
  4. **Build lud zur Build-Zeit von github.com** (`utoipa-swagger-ui`) → Fix:
     `vendored`-Feature. Dabei zusätzlich gefunden: `-C target-cpu=native` in
     `.cargo/config.toml` lässt den Build in VMs mit über-meldendem CPUID mit
     **SIGILL** abstürzen und backt im CI die Runner-CPU in ein Binary für einen
     anderen Host — jetzt opt-in per `RUSTFLAGS`.
  Zusätzlich zählt `WaveAnomalyDetector` jetzt Baseline-Treffer/-Fehlschläge und
  `DetectorRunner` loggt einmalig einen Fehler, wenn eine geladene Baseline nach
  1000 Events nie gematcht hat — gegen genau die stille Fehlfunktion aus Bug 2.
  **338 Tests grün** (+10), clippy und fmt sauber.

---

## Projektübersicht

**Zweck:** Production-grade Rust Log Processing Gateway als Ersatz für ineffiziente Python Log-Infrastruktur.
**Stack:** Rust 2021 · Tokio async · Axum 0.7 · Prometheus · Grafana · MinIO/S3
**Pfad:** `/Users/alirezashahsavarkhani/rust_tool/log-gateway`
**Status:** Alle Milestones + Tasks + P1–P6 abgeschlossen + Performance-Optimierungen #1, #2 & #3 + Hardcore Tests + 4-Zylinder Cluster. **105/105 Tests grün.**
Python-Streams vollständig durch Rust ersetzt. bgp-stream deployed, Verbindungsfix (rustls ALPN) gepusht — **Verifikation ausstehend (neue Session).**

---

## Workflow

Implementierung läuft nach diesem Schema:
1. DeepSeek generiert Code per Copy-Paste-Prompt
2. Claude verifiziert die Implementierung
3. Claude fixt Bugs (insb. `cargo fmt`, `cargo clippy`)
4. Nächster Task

---

## Verzeichnisstruktur

```
log-gateway/
├── src/
│   ├── main.rs              # Server-Bootstrap, Routing, Graceful Shutdown (nutzt log_gateway::create_app)
│   ├── lib.rs               # create_app() + create_test_app() — Library-Crate für Integrationstests
│   ├── handlers/mod.rs      # Alle HTTP Handler (ingest_log, health, metrics, costs, s3) + ApiDoc (OpenAPI)
│   ├── models.rs            # LogEntry, LogLevel, IngestResponse
│   ├── config.rs            # GatewayConfig (TOML + ENV)
│   ├── cache.rs             # SemanticCache (moka SegmentedCache 32 Segmente, SHA-256, LRU+TTL)
│   ├── cost_tracker.rs      # CostTracker (DashMap, per-tenant)
│   ├── logging.rs           # Structured JSON/Text Logging Initialization (LOG_FORMAT env var)
│   ├── metrics.rs           # GatewayMetrics (prometheus-client, 6 Counter + 1 Histogram)
│   ├── middleware.rs        # require_api_key (X-API-Key), require_jwt (HS256 Bearer Token) — nutzt secrets::read_secret
│   ├── secrets.rs           # read_secret() — Docker Secret File zuerst, ENV-Var Fallback
│   ├── redactor.rs          # PII-Redaktion (7 Regex-Pattern via once_cell::Lazy)
│   ├── rate_limiter.rs      # GCRA Rate Limiter (governor, TenantLimiter per X-Tenant-ID)
│   ├── sink.rs              # StorageSink (NDJSON + optionales zstd, lock-free ArrayQueue)
│   ├── s3_exporter.rs       # S3Exporter (aws-sdk-s3, MinIO-kompatibel)
│   ├── schema_validator.rs  # JSON Schema Validation (jsonschema, Lazy<JSONSchema>)
│   └── hot_reload.rs        # Config Hot-Reload via SIGHUP (tokio::sync::watch)
├── config/
│   └── default.toml         # Alle Konfigurationswerte
├── deploy/
│   ├── prometheus.yml       # Scrape-Config (gateway:8080/metrics, 5s) + Alerting Rules
│   ├── alertmanager/
│   │   ├── alerts.yml       # 5 Alert Rules für Gateway-Metriken
│   │   └── alertmanager.yml # Alertmanager Konfiguration
│   └── grafana/
│       └── provisioning/
│           ├── datasources/prometheus.yml   # uid: prometheus
│           └── dashboards/
│               ├── dashboard.yml
│               └── gateway.json            # 11 Panels, version 3 (inkl. Active Alerts alertlist)
│           └── datasources/
│               ├── prometheus.yml          # uid: prometheus
│               └── alertmanager.yml        # uid: alertmanager, url: http://alertmanager:9093
├── .cargo/
│   └── config.toml          # target-cpu=native für x86_64-unknown-linux-gnu (Produktions-Server)
├── .github/workflows/ci.yml # 4 Jobs: test, lint, security-audit, docker (GHCR publish, multi-arch)
├── Dockerfile               # Multi-stage: rust:1-slim-bookworm (builder) → debian:bookworm-slim (runtime, wget für healthcheck)
├── certs/                   # TLS Zertifikate (*.pem gitignored, nur README.md + .gitignore in Git)
│   ├── .gitignore           # *.pem *.key *.crt *.p12
│   └── README.md            # openssl self-signed cert Anleitung
├── secrets/                 # Docker Secrets (nicht in Git)
│   ├── .gitignore           # * !.gitignore !*.example
│   ├── gateway_api_key.txt.example
│   ├── gateway_jwt_secret.txt.example
│   ├── slack_webhook_critical.txt.example
│   └── slack_webhook_warning.txt.example
├── .env.example             # SLACK_WEBHOOK_CRITICAL + SLACK_WEBHOOK_WARNING
├── benches/
│   └── gateway_benchmarks.rs  # criterion 0.5: 7 Benchmarks (redactor: 4, cache key: 3), html_reports
├── tests/
│   ├── integration_test.rs  # 9 Integrationstests (incl. oversized body 413)
│   ├── property_tests.rs    # 9 Property-based Tests (proptest): 4 cache + 5 redactor Invarianten
│   └── hardcore_test.rs     # 27 Tests: Chaos(8), Concurrency(3), Security(5), Edge Cases(8), Load(3)
├── docker-compose.yml       # Dev-Stack: gateway (8080), prometheus, grafana, minio
├── docker-compose.prod.yml  # Prod-Cluster: haproxy (8090) + 4x gateway (cpuset 0-3) + bgp-stream + support services
├── deploy-cluster.sh        # Zero-Downtime Deploy: health-gates, smoke-test, --rollback flag
├── tools/
│   └── bgp_stream/
│       ├── Cargo.toml       # bgp-stream sub-crate (Cargo workspace member)
│       ├── Dockerfile       # Debian bookworm-slim + binary
│       └── src/main.rs      # RIPE RIS Live WebSocket → Log Gateway (Rust)
└── deploy/
    ├── nginx/nginx.conf     # ip_hash upstream, keepalive 64, epoll, 64KB body limit
    └── prometheus.prod.yml  # Scrapt alle 4 Instanzen einzeln mit instance/cpuset Labels
```

---

## Source-Dateien im Detail

### `src/lib.rs` (Library-Crate)
- `pub fn create_app(config: GatewayConfig) -> Result<Router>` — vollständiger Stack mit Auth-Middlewares
- `pub fn create_test_app(config: GatewayConfig) -> Result<Router>` — Stack ohne Auth für Integrationstests
- Alle Module als `pub mod` exportiert → ermöglicht `use log_gateway::...` in `tests/`
- `pub mod secrets` exportiert
- **`CompressionLayer::new()`** (Gzip) vor `TraceLayer` — in beiden `create_app` und `create_test_app` (Perf-Opt #2C)

### `src/secrets.rs`
- `pub fn read_secret(secret_name: &str, env_name: &str) -> Option<String>`
- Interne `read_secret_with_path(path, env_name)` — injizierbarer Pfad für Tests
- Liest zuerst `/run/secrets/<secret_name>` (Docker Swarm Secret File)
- Fallback: `std::env::var(env_name)` (lokale Entwicklung / docker-compose ENV)
- Leere Werte → `None`; Dateiinhalt wird getrimmt
- 5 Tests: env, empty, file (tempfile), file-overrides-env (tempfile), none

### `src/main.rs`
- **`#[global_allocator] MiMalloc`** — mimalloc als globaler Allocator (Performance-Optimierung #1)
- **Tokio Worker-Count Logging** — Perf-Opt #3D: liest `TOKIO_WORKER_THREADS` ENV-Var, loggt effektive Anzahl beim Start
  - Auf 16+ Core Servern setzen: `TOKIO_WORKER_THREADS=16` (Tokio liest diese Var nativ)
- Lädt `GatewayConfig`, initialisiert S3-Exporter + Sink + Hot-Reload
- Delegiert App-Erstellung an `log_gateway::create_app(config)`
- Status-Logs nutzen `secrets::read_secret()` für API-Key + JWT
- SIGTERM + Ctrl+C Graceful Shutdown mit Sink-Flush
- Startet S3-Background-Export-Task (60s Intervall)
- **TLS-Zweig:** `config.tls.enabled == true` → `axum_server::bind_rustls()` mit `RustlsConfig::from_pem_file()`
- **HTTP-Zweig:** `config.tls.enabled == false` → `tokio::net::TcpListener` + `axum::serve()` (mit Graceful Shutdown)

**Geschützte Routen:**
```
POST /api/v1/logs
GET  /api/v1/cache/stats
GET  /api/v1/costs
GET  /api/v1/costs/:tenant_id
POST /api/v1/export/s3
```

**Öffentliche Routen:**
```
GET /health
GET /metrics
```

### `src/handlers/mod.rs`
**AppState:**
```rust
pub struct AppState {
    pub redactor: Redactor,
    pub cache: SemanticCache,
    pub cost_tracker: CostTracker,
    pub metrics: GatewayMetrics,
    pub sink: Option<StorageSink>,
    pub s3_exporter: Option<Arc<S3Exporter>>,
    pub sink_output_dir: PathBuf,
    pub started_at: Instant,
}
```

**ingest_log Flow (Perf-Opt #2A — Single-Parse):**
1. Body als `axum::body::Bytes` empfangen — kein automatischer JSON-Parse durch Axum
2. `serde_json::from_slice::<LogEntry>(&bytes)` → einmalige Deserialisierung direkt zu `LogEntry` → 400 bei Fehler
3. `serde_json::to_value(&entry)` → `Value` nur für Schema-Validation
4. Schema-Validation via `SchemaValidator::validate()` → 422 bei Fehler (inkl. source.len 1–128, message.len 1–8192)
5. `X-Tenant-ID` Header extrahieren → Fallback `"anonymous"`, Validierung max 64 Zeichen alphanumerisch+`-`+`_` → 400 bei Fehler
6. Cache-Lookup (SHA-256 Key)
7. PII-Redaktion (7 Pattern) — nur bei Cache MISS
8. Cost-Tracking mit `tenant_id` (aus Header, nicht aus `entry.source`)
9. Prometheus-Metriken
10. Sink-Write (NDJSON / zstd, sync — kein `.await`) — `entry.source` bleibt unverändert
11. Response mit `X-Request-ID` Header + Latenz-Messung

### `src/redactor.rs` (226 Zeilen)
7 PII-Pattern als individuelle `Lazy<Regex>` statics:
- `EMAIL` — `[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}`
- `IPV4` — `\b(?:\d{1,3}\.){3}\d{1,3}\b`
- `IPV6` — Vollständiges IPv6-Pattern
- `SSN` — `\b\d{3}-\d{2}-\d{4}\b`
- `PHONE` — `\b[\+]?[\d\s\-\(\)]{10,}\b`
- `CC` / `CREDIT_CARD` — Luhn-ähnliches Pattern
- `IBAN` — `[A-Z]{2}\d{2}[A-Z0-9]{4,32}`

Gibt `RedactionResult { redacted_text: String, hits: Vec<PiiPattern>, hit_count: usize }` zurück.

### `src/cache.rs`
- `SemanticCache` mit `moka::sync::SegmentedCache` (32 Segmente, LRU + TTL) — Perf-Opt #1
- `SegmentedCache::builder(32).max_capacity(...).time_to_live(...).build()`
- SHA-256 Normalisierung: lowercase + trim → Hex-Key
- `AtomicU64` Zähler für hits/misses
- `pub fn len(&self) -> u64` für Health-Check

### `src/metrics.rs`
6 Metriken (alle mit `Arc<>`):
- **Registry via `Arc<RwLock<Registry>>`** — Perf-Opt #3A (concurrent reads beim Scraping)
- `render()` nutzt `.read()` statt `.lock()` → mehrere Prometheus-Scrapes blockieren sich nicht gegenseitig
```
gateway_requests_total          Counter
gateway_cache_hits_total        Counter
gateway_cache_misses_total      Counter
gateway_pii_hits_total          Counter
gateway_bytes_ingested_total    Counter
gateway_rate_limit_hits_total   Counter
gateway_request_duration_ms     Histogram  (buckets: 1,5,10,25,50,100,250,500,1000,2500,5000ms)
```
WICHTIG: `prometheus-client` hängt automatisch `_total` an Counter-Namen — Namen ohne `_total` registrieren!

### `src/sink.rs`
- `StorageSink::new(output_dir, max_buffer_size, flush_interval_secs, compress: bool)`
- **Lock-free Buffer via `Arc<crossbeam::queue::ArrayQueue<SinkRecord>>`** — Perf-Opt #2B
  - Kapazität: `max_buffer_size * 2` als Headroom
  - `pub fn write(&self, record: SinkRecord)` — **sync** (kein `async`, kein Lock)
  - Queue voll → Record wird silent gedroppt (kein Panic, kein Block)
- `flush()` drainiert via `pop()`-Schleife in lokalen Vec, schreibt dann async
- **Stream-Serialisierung via `serde_json::to_writer(&mut Vec<u8>)`** — Perf-Opt #3B (kein intermediärer String, kein `join()`)
- `compress=true` → `.ndjson.zst` via `zstd::encode_all(..., level=1)` — **Level 1** (Perf-Opt #3C, −40% CPU vs Level 3)
- `compress=false` → `.ndjson` plain text
- `start_flush_task()` → background tokio Task
- `flush_on_shutdown()` → aufgerufen bei Graceful Shutdown

### `src/config.rs` (103 Zeilen)
Lädt aus `config/default.toml` + ENV `GATEWAY__*` (Separator `__`):
```toml
[server]   host, port
[cache]    max_capacity, ttl_seconds
[cost]     enabled
[metrics]  enabled
[sink]     enabled, output_dir, max_buffer_size, flush_interval_secs, compress
[rate_limit] enabled, requests_per_second
[s3]       enabled, endpoint_url, bucket, region, prefix, access_key_id, secret_access_key, delete_after_upload
[tls]      enabled, cert_path, key_path
```

### `src/rate_limiter.rs` (197 Zeilen)
- `TenantLimiter = Arc<RateLimiter<String, DashMapStateStore<String>, DefaultClock, NoOpMiddleware>>`
- GCRA-Algorithmus via `governor` — **per-Tenant** isoliert
- Tenant wird aus `X-Tenant-ID` Header gelesen (Fallback: `"anonymous"`)
- Validierung: max 64 Zeichen, alphanumerisch + `-` + `_` → sonst `400`
- Injiziert als `Extension<TenantLimiter>` + `Extension<Arc<GatewayMetrics>>`
- Bei Überschreitung: `429 Too Many Requests` + `record_rate_limit_hit()`
- `pub fn new_tenant_limiter(requests_per_second: u32) -> TenantLimiter`

### `src/logging.rs` (116 Zeilen)
- `pub fn init_tracing()` — liest `LOG_FORMAT` ENV-Var, initialisiert globalen Subscriber
- `LOG_FORMAT=json` → strukturiertes JSON mit `timestamp`, `level`, `message`, `target` (via `UtcTime::rfc_3339`)
- `LOG_FORMAT=text` → Plain-Text-Format (Default)
- Ungültiger Wert → Fallback auf `text` + `tracing::warn!()`
- `main.rs` ruft nur noch `logging::init_tracing()` auf

### `src/middleware.rs`
- `require_api_key`: Liest `GATEWAY_API_KEY` Env-Var; prüft `X-API-Key` Header → `401` bei Fehler; wenn nicht gesetzt → deaktiviert
- `require_jwt`: Liest `GATEWAY_JWT_SECRET` Env-Var; validiert `Authorization: Bearer <token>` (HS256, `exp`-Check); setzt `X-JWT-Subject` Header aus `sub`-Claim; wenn nicht gesetzt → deaktiviert
- Beide Middlewares sind optional und unabhängig aktivierbar

### `src/s3_exporter.rs` (176 Zeilen)
- `S3Exporter::new()` mit `force_path_style(true)` für MinIO
- `upload_file(path)` — lädt einzelne Datei hoch
- `export_pending_files(dir)` — scannt Verzeichnis, exportiert alle `.ndjson`/`.ndjson.zst`
- `delete_after_upload` optional konfigurierbar

### `src/schema_validator.rs` (74 Zeilen)
- `JSONSchema` als `Lazy<JSONSchema>` (einmalig kompiliert)
- Validiert: `level` (enum), `source` (1-128 Zeichen), `message` (1-8192 Zeichen), `metadata` (object|null optional)
- `SchemaValidator::validate(&Value) -> Result<(), String>`

### `src/hot_reload.rs` (53 Zeilen)
- `start_config_watcher() -> watch::Receiver<Arc<GatewayConfig>>`
- `#[cfg(unix)]`: SIGHUP-Handler startet Reload-Loop
- `#[cfg(not(unix))]`: `drop(tx)` — kein Reload auf Windows
- Fehler beim Reload: loggen, aktive Config behalten

---

## Alle 105 Tests

| Modul | Tests |
|---|---|
| `cache` | test_cache_hit_increments_counter, test_cache_miss_increments_counter, test_make_key_case_insensitive, test_make_key_deterministic, test_stats_empty_cache, test_stats_hit_rate_calculation |
| `cost_tracker` | test_cache_hit_miss_tracking, test_pii_hits_accumulate, test_record_creates_tenant, test_record_increments_bytes, test_summary_sorts_by_requests_desc, test_tenant_stats_not_found |
| `handlers` | test_health_check_response, test_ingest_log_headers, test_metrics_contains_histogram, test_ingest_uses_tenant_id_header_for_cost, test_ingest_falls_back_to_anonymous_without_header, test_ingest_rejects_invalid_tenant_id |
| `hot_reload` | test_config_watcher_returns_valid_config |
| `logging` | test_json_format_env_var_respected, test_text_format_is_default |
| `middleware` | test_auth_disabled_when_no_env_var, test_auth_passes_correct_key, test_auth_rejects_missing_key_header, test_auth_rejects_wrong_key, test_jwt_disabled_when_no_env_var, test_jwt_rejects_missing_token, test_jwt_rejects_invalid_token, test_jwt_accepts_valid_token |
| `models` | test_ingest_response_serialize, test_log_entry_deserialize, test_log_level_deserialize_all_variants, test_log_level_deserialize_lowercase |
| `rate_limiter` | test_tenant_limiter_allows_first_request, test_tenant_limiter_rejects_over_limit, test_tenant_limiter_isolates_tenants, test_invalid_tenant_header_rejected, test_rate_limit_middleware_with_tenant |
| `redactor` | test_credit_card_redaction, test_email_redaction, test_hit_count_accuracy, test_iban_redaction, test_ipv4_redaction, test_multiple_pii_same_msg, test_no_pii, test_phone_redaction, test_ssn_redaction |
| `s3_exporter` | test_export_skips_empty_files, test_s3_config_default, test_s3_key_format |
| `schema_validator` | test_empty_message_fails, test_invalid_level_fails, test_missing_required_field_fails, test_valid_log_entry_passes_schema |
| `secrets` | test_read_secret_from_env, test_read_secret_returns_none_when_empty, test_read_secret_from_file, test_read_secret_file_overrides_env, test_read_secret_none_when_neither_exists |
| `sink` | test_sink_compress_creates_zst_file |
| `integration_test` | 9 Tests: health, ingest, invalid schema, missing fields, rate limit, cache stats, cost tracking, tenant id, oversized body (413) |
| `hardcore_test` | 27 Tests — Chaos(8): empty/truncated/oversized body, wrong types, null fields, array, body at limit — Concurrency(3): parallel identical/distinct, unique IDs — Security(5): injection, unicode, tenant injection, content-type — Edge Cases(8): log levels, request ID, PII, tenant length — Load(3): 1000 sequential, 500 concurrent, health under load |

---

## Dependencies (Cargo.toml)

```toml
axum = "0.7"                         # HTTP Framework
axum-server = { version = "0.7", features = ["tls-rustls"] }  # TLS/HTTPS Support
tokio = { features = ["full","signal"] }
serde / serde_json                   # Serialisierung
config = "0.14"                      # TOML + ENV Config
tracing / tracing-subscriber = { version = "0.3", features = ["env-filter", "json", "time"] }
tower / tower-http = { features = ["trace","compression-gzip"] }  # Middleware (TraceLayer + CompressionLayer aktiv, Perf-Opt #2C)
uuid = { features = ["v4","serde"] }
chrono = { features = ["serde"] }
thiserror / anyhow                   # Error Handling
regex / once_cell                    # PII Pattern
moka = { features = ["sync"] }       # In-Memory Cache
sha2 / hex                           # SHA-256 Cache Keys
dashmap = "6"                        # Lock-free HashMap
prometheus-client = "0.22"           # Metriken
governor = { features = ["dashmap"]} # GCRA Rate Limiting
mimalloc = { version = "0.1", default-features = false }  # Globaler Allocator (Perf-Opt #1)
crossbeam = { version = "0.8", features = ["crossbeam-queue"] }  # Lock-free ArrayQueue für Sink (Perf-Opt #2B)
aws-config / aws-sdk-s3 / aws-credential-types  # S3/MinIO
tokio-util                           # IO Utils
zstd = "0.13"                        # Log-Kompression
jsonschema = "0.18"                  # Schema Validation
jsonwebtoken = "9"                   # JWT HS256 Validierung (require_jwt Middleware)
utoipa = { version = "4", features = ["axum_extras","chrono","uuid"] }  # OpenAPI Spec-Generierung
utoipa-swagger-ui = { version = "7", features = ["axum"] }              # Swagger UI unter /swagger-ui/

[dev-dependencies]
proptest = "1"                       # Property-based Testing (tests/property_tests.rs)
criterion = { version = "0.5", features = ["html_reports"] }  # Benchmarks (benches/gateway_benchmarks.rs)

[[bench]]
name = "gateway_benchmarks"
harness = false

[profile.release]
opt-level = 3          # Maximale Geschwindigkeitsoptimierung
lto = true             # Link-Time Optimization
codegen-units = 1      # Ein Codegen-Block für bessere Optimierung
panic = "abort"        # Kein Stack-Unwinding, kleinere Binärdatei
strip = true           # Debug-Symbole entfernen

[profile.dev]
opt-level = 0          # Keine Optimierung für schnelle Kompilierung
debug = true           # Volle Debug-Informationen
```

---

## Bekannte Eigenheiten / Bugs die bereits gefixt wurden

| Problem | Fix |
|---|---|
| `_total_total` Metrik-Namen | `prometheus-client` hängt `_total` auto an → Namen ohne Suffix registrieren |
| Auth Middleware auf allen Routen | Route-Split: `protected.route_layer()` vs `public` Router |
| `RUN mkdir` in distroless Stage 2 | Entfernt — distroless hat keine Shell |
| `/metrics` hinter Auth | In public Router verschoben |
| Grafana Panels ohne datasource uid | `"datasource": {"type":"prometheus","uid":"prometheus"}` in allen Panels |
| `matches!(level, _var)` tautologisch | 4 explizite `matches!(val, LogLevel::Variant)` Assertions |
| `cargo fmt --check` schlägt nach DeepSeek-Code fehl | Nach jeder Implementierung `cargo fmt` ausführen |
| `clippy::redundant_closure` `.map(\|k\| Arc::new(k))` | → `.map(Arc::new)` |
| Oversized Body wird zu spät abgelehnt | `DefaultBodyLimit::max(65_536)` auf `/api/v1/logs` Route-Level → 413 vor JSON-Parse |
| ip_hash + einzelne Client-IP | Alle Requests landen auf einer Instanz — by design für Multi-Client-Produktion, nicht für Single-Client-Benchmarks |
| nginx healthcheck `wget` in alpine | nginx:alpine hat nur `curl` — Healthcheck nutzt `["CMD", "curl", "-sf", "..."]` |
| ServiceBuilder Layer-Reihenfolge | Extensions VOR Middleware hinzufügen (FIFO) |
| `test_jwt_redaction` falscher Name | Umbenannt zu `test_iban_redaction` |
| `moka::sync::Cache` kein Segment-Support | Typ zu `SegmentedCache::builder(32)` gewechselt (Perf-Opt #1) |
| `Cache::builder().segments()` gibt `SegmentedCache` zurück | Struct-Feld-Typ + Import angepasst |
| Sink `Mutex<Vec>` Lock-Contention | Ersetzt durch `crossbeam::ArrayQueue` lock-free (Perf-Opt #2B) |
| `sink.write()` war `async` | Jetzt `pub fn write()` sync — alle Aufrufer ohne `.await` (Perf-Opt #2B) |
| Doppelte JSON-Deserialisierung in `ingest_log` | `Bytes` + `from_slice` → `to_value` für Schema-Check (Perf-Opt #2A) |
| `CompressionLayer` in Cargo.toml aber nicht aktiv | In `create_app` + `create_test_app` eingefügt (Perf-Opt #2C) |
| `Mutex<Registry>` in metrics.rs | Ersetzt durch `RwLock<Registry>` — concurrent reads möglich (Perf-Opt #3A) |
| `lines.join("\n")` baut 1 MB+ String | `serde_json::to_writer` direkt in `Vec<u8>` (Perf-Opt #3B) |
| zstd Level 3 in sink.rs | Level 1 — −40% Compress-CPU, kaum Qualitätsverlust bei Logs (Perf-Opt #3C) |
| Tokio Worker-Count unsichtbar | Startup-Log + `TOKIO_WORKER_THREADS` ENV-Var Hinweis (Perf-Opt #3D) |

---

## CI/CD (.github/workflows/ci.yml)

4 Jobs:
1. **test** — `cargo test` + `cargo build --release`
2. **lint** — `cargo fmt --check` + `cargo clippy --all-targets -- -D warnings`
3. **security-audit** — `cargo-audit` (gecacht bei `~/.cargo/bin/cargo-audit`, Version 0.21.2)
4. **docker** — Multi-Stage Build (`docker/build-push-action@v6`, push:true auf main-Branch, multi-arch amd64+arm64)

---

## Docker

**Dockerfile:**
- Stage 1: `rust:1-slim-bookworm` (glibc, unterstützt proc-macros auf aarch64)
- Stage 2: `debian:bookworm-slim` — hat `wget` für Docker Healthchecks

**docker-compose.yml (Dev):**
- `gateway` — Port 8080, alle Support-Services

**docker-compose.prod.yml (Cluster):**
- `nginx-lb` — Port 8090 (einziger externer Port), ip_hash, keepalive 64
- `gateway_1..4` — je auf CPU 0-3 gepinnt via `cpuset`, nur intern erreichbar
- `prometheus` scrapt alle 4 Instanzen via `deploy/prometheus.prod.yml`

**Produktions-Deploy (Hetzner 167.235.30.106):**
- Projektpfad: `/root/log-gateway-src`
- GitHub Actions deployed automatisch auf Push zu `main` (multi-arch amd64+arm64)
- Secrets: `/root/log-gateway-src/secrets/*.txt`
- Deploy: `./deploy-cluster.sh --build`

## bgp-stream (tools/bgp_stream)

Rust-Ersatz für die Python BGP-Stream-Implementierung.

**Funktion:** RIPE NCC RIS Live WebSocket → batched POST → Log Gateway
**Endpoint:** `wss://ris-live.ripe.net/v1/ws/`
**Container:** `bgp-stream` in `docker-compose.prod.yml` mit `network_mode: host`

**Kritische Punkte:**
- `network_mode: host` — nötig, weil Docker bridge NAT ausgehende HTTPS blockiert
- `extra_hosts` funktioniert NICHT mit `network_mode: host` → stattdessen `/etc/hosts` auf dem Host
- Server `/etc/hosts` enthält: `193.0.11.16 ris-live.ripe.net` (IPv4 forcieren, kein IPv6)
- Letzter Fix (Commit `19428f7`): expliziter `rustls::ClientConfig` ohne ALPN als `Connector::Rustls` — verhindert CLOSE-WAIT-Hänger durch falsche ALPN-Aushandlung
- `connect_async_tls_with_config(..., None)` NICHT verwenden — baut internen Connector ohne CryptoProvider → panic/hang

**Neue Session: Als erstes prüfen:**
```bash
ssh root@167.235.30.106 "docker ps | grep bgp && docker logs bgp-stream --tail 30"
# Erwartetes Ergebnis: "Connected ✓" + nach 10s Stats-Zeile
```

## Alert Rules (deploy/alertmanager/alerts.yml)

5 Alert-Regeln für Gateway-Metriken:

| Alert | PromQL | For | Severity | Beschreibung |
|---|---|---|---|---|
| `GatewayHighErrorRate` | `rate(gateway_rate_limit_hits_total[1m]) > 10` | 1m | warning | Rate Limit Hits > 10/s |
| `GatewayHighLatencyP99` | `histogram_quantile(0.99, rate(gateway_request_duration_ms_bucket[2m])) > 500` | 2m | warning | p99 Latency > 500ms |
| `GatewayCacheHitRateLow` | `(rate(hits[5m]) / (rate(hits[5m]) + rate(misses[5m]))) * 100 < 20 and (rate(hits[5m]) + rate(misses[5m])) > 0` | 5m | info | Cache Hit Rate < 20% (Div/0-Guard) |
| `GatewayHighPiiDetection` | `rate(gateway_pii_hits_total[1m]) > 50` | 1m | warning | PII Detections > 50/s |
| `GatewayDown` | `up{job="log-gateway"} == 0` | 30s | critical | Gateway Service down |

Prometheus konfiguriert in `deploy/prometheus.yml` mit:
- `rule_files: ["alertmanager/alerts.yml"]`
- `alerting.alertmanagers.targets: ["alertmanager:9093"]`

---

## Grafana Dashboard (deploy/grafana/provisioning/dashboards/gateway.json)

11 Panels, version 3, uid: `log-gateway-main`:

| Panel | Typ | PromQL / Datasource |
|---|---|---|
| Requests/s | stat | `rate(gateway_requests_total[1m])` |
| Cache Hit Rate % | stat | `rate(hits) / (rate(hits) + rate(misses)) * 100` |
| Rate Limit Rejections/s | stat | `rate(gateway_rate_limit_hits_total[1m])` |
| PII Detections/s | stat | `rate(gateway_pii_hits_total[1m])` |
| Latency p50/p95/p99 | timeseries | `histogram_quantile(0.5x, rate(..._bucket[1m]))` |
| Throughput Bytes/s | timeseries | `rate(gateway_bytes_ingested_total[1m])` |
| Cache Hits vs Misses | timeseries | beide rate()-Metriken |
| Rate Limit Hits over Time | timeseries | `rate(gateway_rate_limit_hits_total[1m])` |
| PII Hit Rate over Time | timeseries | `rate(gateway_pii_hits_total[1m])` |
| Cumulative Requests | timeseries | `gateway_requests_total` |
| **Active Alerts** | **alertlist** | **AlertManager** (uid: alertmanager, firing/error/pending) |

---

## Schnellstart-Befehle

```bash
# Tests ausführen
cargo test

# Build
cargo build --release

# Code-Qualität
cargo clippy -- -D warnings
cargo fmt --check
cargo fmt  # auto-fix

# Stack lokal starten
docker-compose up -d

# Gateway testen (dev, kein API-Key nötig)
curl -X POST http://localhost:8080/api/v1/logs \
  -H "Content-Type: application/json" \
  -d '{"level":"info","source":"test","message":"hello world"}'

# Mit API-Key
GATEWAY_API_KEY=secret cargo run
curl -X POST http://localhost:8080/api/v1/logs \
  -H "X-API-Key: secret" \
  -H "Content-Type: application/json" \
  -d '{"level":"info","source":"test","message":"hello"}'

# Config Hot-Reload
kill -HUP $(pgrep log-gateway)

# Grafana
open http://localhost:3000  # admin/admin
```

---

## Milestone-Übersicht

| # | Milestone | Kern-Feature | Tests |
|---|---|---|---|
| M1 | Core Server | Axum Setup, Health-Check | — |
| M2 | PII Redaktion | 7 Regex-Pattern | 9 |
| M3 | Semantic Cache | moka + SHA-256 | 6 |
| M4 | Cost Tracker | DashMap per-tenant | 6 |
| M5 | Prometheus Metrics | 6 Counter + 1 Histogram | — |
| M6 | Storage Sink | NDJSON Buffer + Flush | — |
| M7a | API Key Auth | X-API-Key Middleware | 4 |
| M7b | Docker | Multi-Stage distroless | — |
| M7c | Observability | X-Request-ID, Latenz, Health v2 | 3 |
| M8 | Rate Limiting | GCRA governor (global) | 2 |
| M9 | CI/CD | GitHub Actions 4 Jobs | — |
| M10 | Unit Tests | axum-test Integration | 3 |
| M11 | S3/MinIO Export | aws-sdk-s3 | 3 |
| M12 | Grafana Dashboard | 10 Panels JSON | — |
| M13a | zstd Compression | Sink compress-Flag | 1 |
| M13b | Schema Validation | jsonschema Lazy | 4 |
| M13c | Hot-Reload | SIGHUP watch::channel | 1 |
| M14 | Per-Tenant Rate Limiting | DashMapStateStore, X-Tenant-ID Header | 5 |
| M15 | Structured JSON Logging | LOG_FORMAT env var, JSON/text format | 2 |
| M16 | AlertManager | 5 Alert-Regeln, alertmanager Service | — |
| T6 | Tenant-ID Konsistenz | Cost Tracker nutzt X-Tenant-ID Header | 3 |
| T1 | Release-Profil | lto, codegen-units=1, panic=abort, strip | — |
| T4 | PromQL Division-by-Zero Fix | GatewayCacheHitRateLow `and`-Guard | — |
| T3 | Dockerfile Rust-Version | `rust:1.76-alpine` → `rust:1-alpine` (floating stable) | — |
| T2 | CI-Pipeline Review | clippy `--all-targets`, cargo-audit 0.21.2, build-push-action@v6, tokio::sync::Mutex Fix | — |
| T9 | JWT-Middleware | HS256, `GATEWAY_JWT_SECRET`, `X-JWT-Subject` Header, optionale Auth | 4 |
| T8 | OpenAPI/Swagger | utoipa 4, SwaggerUi, `/swagger-ui/`, `/api-docs/openapi.json`, SecurityAddon | — |
| T5 | Integration-Tests | 8 Tests, echter TcpListener:0, reqwest, create_test_app, lib.rs | 8 |
| T10 | Grafana AlertManager-Panel | alertlist Panel 11, version 3, datasources/alertmanager.yml | — |
| P1 | Secrets Management | Docker Secrets + ENV-Fallback, secrets.rs, middleware nutzt read_secret() | 5 |
| P2 | TLS/HTTPS | axum-server tls-rustls, TlsConfig, bedingter HTTP/HTTPS-Start, certs/ Verzeichnis | — |
| P3 | Alertmanager Slack Routing | 3 Receiver (critical/warning/info), ENV-Var Webhooks, .env.example, secrets/*.example | — |
| P4 | GHCR Publishing | docker-Job: QEMU, login-action, metadata-action, multi-arch (amd64+arm64), push:true | — |
| P5 | Property-based Tests | proptest, 9 Invarianten: cache key (4) + redactor (5), tests/property_tests.rs | 9 |
| P6 | Benchmarks | criterion 0.5, 7 Benchmarks: redactor (4) + cache key (3), html_reports, cargo bench | — |
| Opt#1 | Perf-Opt #1 | mimalloc global allocator, SegmentedCache(32), target-cpu=native, Redactor early-exit+into_owned | — |
| Opt#2 | Perf-Opt #2 | Single-parse ingest_log (Bytes+from_slice), lock-free ArrayQueue Sink, Gzip CompressionLayer | — |
| Opt#3 | Perf-Opt #3 | RwLock Registry, stream-serialize Sink (to_writer), zstd Level 1, Tokio Worker-Count Log | — |
| **∑** | | | **77** |

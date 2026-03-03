# Roadmap — Stufe 3: Alert-Engine & RPKI-Vertiefung

> **Status: 🔵 IN ARBEIT**
> Geschätzter Aufwand: 8–12 Wochen solo / 5–7 Wochen im Team
> Voraussetzung: Stufe 2 abgeschlossen ✅
> Begonnen: 2026-03

---

## Ziel

Aus regelbasierter Anomalie-Erkennung eine vollwertige Alert-Engine machen.
Konfigurierbares Alert-Routing, Silence-Management, Eskalationsstufen,
ML-gestützte Baseline-Erkennung und vollständige RPKI-Abdeckung inkl.
ROA-Zeitreihen.

---

## 3.1 — Erweiterte Alert-Engine

**Zeitaufwand:** 3–4 Wochen
**Voraussetzung:** Stufe 2.2 (Rule-Based Detektoren)

### 3.1.1 — Alert-Konfiguration per API ✅

- [x] `alert_rules` + `alert_history` Tabellen in ClickHouse (`deploy/clickhouse/alert_rules_schema.sql`)
- [x] CRUD-API: GET/POST `/alerts/rules`, PUT/DELETE `/alerts/rules/:id`, GET `/alerts/active`
- [x] `src/alert_manager.rs`: `AlertManagerClient` mit list/create/update/delete/persist/resolve
- [x] `src/alert_api.rs`: 5 Handler mit utoipa-Annotationen
- [x] Soft-Delete via `enabled=false`, ReplacingMergeTree für Updates
- [x] Dynamisches Laden der Rules zur Laufzeit (Hot-Reload via SIGHUP) ✅
  - `hot_reload::reload_signal_rx()` — zweiter SIGHUP-Watch-Channel (`watch::channel::<()>`)
  - `run_alert_logger` erweitert: `alert_manager: Option<Arc<AlertManagerClient>>` + `reload_rx: watch::Receiver<()>`
  - Startup: `list_rules()` → `cached_rules` geladen
  - SIGHUP → `reload_rx.changed()` → `list_rules()` neu laden, bei Fehler alte Rules behalten
  - `check_rules_and_route()`: `confidence >= rule.threshold` → Escalation + `record_alert_rule_triggered()`
  - `lib.rs`: `reload_signal_rx()` + `alert_manager_client.clone()` an `run_alert_logger` übergeben
  - 3 Tests: logger_ohne_manager, reload_signal_ohne_manager, anomaly_ohne_rules

### 3.1.2 — Eskalationsstufen ✅

- [x] `src/escalation.rs`: `EscalationLevel` (Warning 0.5–0.7 / Critical 0.7–0.9 / Emergency >0.9)
- [x] `EscalationRouter::route()` mit persist + metrics + tracing
- [x] `metrics.rs`: `record_escalation()` als Prometheus Family Counter
- [x] `alertmanager.yml`: 3-stufiges Routing + inhibit_rules (Slack Warning/Critical, PagerDuty Emergency)
- [x] `run_alert_logger` erweitert mit `escalation_router: Option<Arc<EscalationRouter>>`
- [x] Automatische Eskalation wenn Alert nicht acknowledged ✅
  - `EscalationConfig { auto_escalation_enabled, check_interval_secs: 60, timeout_secs: 300 }` in `src/config.rs`
  - `config/default.toml`: `[escalation] auto_escalation_enabled = true`
  - `run_auto_escalation(alert_manager, router, interval, timeout)` in `src/escalation.rs`
  - Loop: alle `check_interval_secs` → `list_active_alerts()` → Alter ≥ `timeout_secs` + confidence < 0.9 → bump +0.15, cap 0.95 → `router.route()`
  - Graceful degradation: ClickHouse-Fehler → `warn!` + `continue`
  - `lib.rs`: `tokio::spawn(run_auto_escalation(...))` wenn `auto_escalation_enabled`
  - 4 Tests: bump_confidence, cap_at_0_95, skip_emergency, config_default
  - Fix: unused imports (`uuid::Uuid`, `AlertHistoryEntry`) in `src/escalation.rs` bereinigt

### 3.1.3 — Alert-Deduplication & Silence ✅

- [x] `src/alert_dedup.rs`: SHA-256 Fingerprint + `DedupCache` (moka, TTL 30min, max 100k)
- [x] `alert_silences` Tabelle in ClickHouse (`ORDER BY id`, ReplacingMergeTree)
- [x] `create_silence`, `list_silences`, `is_silenced`, `expire_silence` in `AlertManagerClient`
- [x] Silence-API: POST `/alerts/:id/silence`, GET `/alerts/silences`, DELETE `/alerts/silences/:id`
- [x] Dedup + Silence-Check im `EscalationRouter` vor persist (graceful degradation)

### 3.1.4 — Webhook-Integration

#### 3.1.4-A — Webhook-Sender ✅
- [x] `src/webhook.rs`: `WebhookTarget` enum (Slack / Generic), `WebhookPayload`, `WebhookSender`
- [x] Slack Block Kit Format (text + fields + details sections)
- [x] Generic HTTP mit custom headers + auto Content-Type
- [x] `build_payload()` aus `Anomaly` + `EscalationLevel` + fingerprint
- [x] `EscalationRouter::with_webhooks()` Konstruktor (rückwärtskompatibel)
- [x] Webhook-Loop in `route()` nach logging (fire-and-forget, kein abort bei Fehler)
- [x] 3 Tests: build_payload, slack_text_format, serialization

#### 3.1.4-B — Webhook-Konfiguration ✅
- [x] `WebhookTargetConfig` + `WebhooksConfig` in `src/config.rs` (mit `#[serde(default)]`)
- [x] `GatewayConfig.webhooks: WebhooksConfig` — rückwärtskompatibel via `#[serde(default)]`
- [x] `config/default.toml`: `[webhooks] enabled = false` + Beispiel-Kommentare
- [x] `src/lib.rs`: webhook_targets aus config bauen, `with_webhooks()` einbinden mit Fallback
- [x] 3 Tests: default, slack deserialization, generic deserialization
- [x] Env-Variable-Support via bestehenden `GATEWAY__WEBHOOKS__ENABLED` Mechanismus

---

## 3.2 — ML-gestützte Anomalie-Erkennung

**Zeitaufwand:** 4–5 Wochen
**Risiko:** Hoch (Modell-Qualität, Trainings-Aufwand)
**Impact:** Sehr hoch — reduziert False Positives drastisch

### 3.2.1 — Baseline-Modell

#### 3.2.1-A — EMA-Baseline ✅
- [x] `src/baseline_model.rs`: `PrefixBaseline { ema, variance_ema, sample_count }` + `BaselineModel { alpha, baselines: Arc<DashMap> }`
- [x] `update(prefix, as_path_len)`: EMA + Varianz-EMA nach Welford-Formel (thread-safe via DashMap)
- [x] `z_score(prefix, value) -> Option<f64>`: `None` wenn `sample_count < 5`, sonst `(value - ema) / sqrt(variance_ema + 1e-6)`
- [x] `is_anomaly(prefix, value, threshold) -> bool`: `z_score.abs() > threshold`, false bei None
- [x] `impl Default` für `alpha = 0.1`, `assert!(alpha > 0.0 && alpha <= 1.0)` in `new()`
- [x] `anomaly_detector.rs`: `baseline: Arc<BaselineModel>` in `AnomalyDetector`, `update()` pro Event, confidence boost `+0.1` (cap 0.95) wenn `is_anomaly(..., 3.0)`
- [x] 8 Tests: `test_ema_update_single`, `test_z_score_none_below_5_samples`, `test_z_score_some_after_5_samples`, `test_is_anomaly_false_for_normal`, `test_is_anomaly_true_for_outlier`, `test_default_alpha`, `test_invalid_alpha_zero/negative/gt_one` (should_panic)

- [ ] Saisonalität: Tages-/Wochenmuster berücksichtigen — offen
- [ ] Training: täglich auf letzten 30 Tagen ClickHouse-Daten — offen

### 3.2.2 — Feature Engineering

#### 3.2.2-A — Prefix-Länge + AS-Bekanntheitsgrad ✅
- [x] `src/baseline_model.rs`: `AsKnowledgeTracker { as_to_days: Arc<DashMap<u32, HashSet<String>>> }`
  - `record_as_seen(asn, date)` — thread-safe, DashMap ohne Mutex
  - `days_seen(asn) -> usize`, `is_well_known(asn) -> bool` (≥7 Tage)
  - `knowledge_score(asn) -> f64` (Tage / 30, gecappt 1.0)
- [x] `prefix_features` Modul: `extract_prefix_length(cidr) -> Option<u8>`, `is_unusual_prefix_length(cidr) -> bool`
  - IPv4: außerhalb 8..=24 → unusual; IPv6: außerhalb 32..=48 → unusual
  - Doctest für beide Funktionen
- [x] `BaselineModel` erweitert: `as_knowledge: Arc<AsKnowledgeTracker>`, Accessor `as_knowledge()`
- [x] `record_as_seen(asn, date)` Delegator auf `BaselineModel`
- [x] `compute_confidence_boost(prefix, as_path_len, origin_as) -> f64`:
  - EMA-Anomalie (Z>3.0): +0.10
  - Ungewöhnlicher Prefix: +0.05
  - Bekanntes AS (≥7 Tage): -0.10
  - Cap: `clamp(-0.15, 0.15)`
- [x] `enhance_confidence(base, prefix, as_path_len, origin_as) -> f64`: `(base + boost).clamp(0.0, 1.0)`
- [x] `src/anomaly_detector.rs`: `check()` nutzt `compute_confidence_boost()` + wendet Boost an wenn `> 0.0`
- [x] 15+ Tests: prefix_features, AsKnowledgeTracker, compute_confidence_boost, enhance_confidence
- [x] Fix: `test_compute_confidence_boost_combined` — Testfehler behoben
  - Bug: Modell mit `"10.0.0.0/8"` trainiert aber Boost für `"10.0.0.0/30"` berechnet → kein EMA für /30 → `is_anomaly` false
  - Fix: Modell auf gleichem Prefix `"10.0.0.0/30"` trainieren
  - Fix: `assert_eq!` → `assert!((a - b).abs() < 1e-10)` wegen Floating-Point-Präzision

Noch offen:
- [ ] AS-Path-Länge (aktuell vs. Median der letzten 7 Tage) — Zeitreihen-Feature
- [ ] Zeit seit letztem ANNOUNCE/WITHDRAW — Zeitreihen-Feature
- [ ] RPKI-Status (Valid/Invalid/NotFound) — bereits in 3.3.3-B integriert
- [ ] Geografische AS-Distanz (via CAIDA AS-Rank) — externe API

### 3.2.3 — Model-Serving

#### 3.2.3-A — Täglicher Retraining-Job ✅
- [x] `src/model_trainer.rs`: `ModelTrainer { baseline, clickhouse_url, database, table, http }`
- [x] `train_from_clickhouse()`: SQL-Query letzte 30 Tage → `baseline.update(prefix, as_path_len)` + `baseline.record_as_seen(origin_as, date)` pro Row
  - Query: `SELECT prefix, length(as_path), origin_as, formatDateTime(timestamp, '%Y-%m-%d') FROM {db}.{table} WHERE timestamp >= now() - INTERVAL 30 DAY FORMAT JSONEachRow`
  - Malformed Rows: `warn!` + skip (kein Abbruch)
  - Gibt Anzahl verarbeiteter Rows zurück
- [x] `run_daily(Arc<Self>)`: Background-Loop — `next_midnight()` berechnen → sleep → train → log → wiederholen
  - Fehler von ClickHouse → `warn!` + retry am nächsten Tag
- [x] `next_midnight(now) -> DateTime<Utc>`: pure Funktion, korrekte Jahreswechsel + Leap-Year-Behandlung
- [x] `AnomalyDetector::baseline_arc() -> Arc<BaselineModel>` — neuer Accessor für Trainer
- [x] `lib.rs`: `tokio::spawn(ModelTrainer::run_daily(trainer))` wenn `clickhouse.enabled && anomaly_detector.is_some()`
- [x] 7 Tests: `test_next_midnight_midday`, `test_next_midnight_just_after_midnight`, `test_next_midnight_year_rollover`, `test_next_midnight_leap_day`, `test_train_fails_gracefully_on_connection_error`, `test_baseline_updated_during_training`, `test_trainer_new`

#### 3.2.3-B — Cold-Start Snapshot ✅
- [x] `deploy/clickhouse/baseline_snapshot_schema.sql`:
  - `baseline_snapshots` (ReplacingMergeTree(snapshot_at), ORDER BY prefix)
  - `as_knowledge_snapshots` (ReplacingMergeTree(snapshot_at), ORDER BY asn, `days_seen Array(String)`)
- [x] `PrefixBaseline` Felder `pub` + `BaselineModel::baselines()` Accessor
- [x] `AsKnowledgeTracker::as_to_days()` Accessor
- [x] `save_snapshot() -> Result<usize>`: Iteriert beide DashMaps, POST als JSONEachRow
- [x] `load_snapshot() -> Result<usize>`: `FINAL`-Query → direktes `baselines().insert()` (kein EMA-smooth, direktes Setzen) + `record_as_seen()` pro Tag
- [x] `load_snapshot_on_startup(Arc<Self>)`: einmalig beim Start, Fehler → `warn!` + weiter
- [x] `run_daily()` erweitert: nach Train → `save_snapshot()`, Fehler → `warn!`
- [x] `lib.rs`: `tokio::spawn(load_snapshot_on_startup)` vor `tokio::spawn(run_daily)`
- [x] Fix: `_prefix2` (unused variable warning in Test)
- [x] 5 neue Tests: `test_save_snapshot_fails_gracefully_on_no_server`, `test_load_snapshot_fails_gracefully_on_no_server`, `test_load_snapshot_on_startup_no_panic`, `test_insert_baseline_direct`, `test_snapshot_roundtrip_logic`

#### 3.2.3-C — A/B-Test: Rule-Based vs. ML-Score ✅
- [x] `src/metrics.rs`: 2 neue Histogramme mit Buckets `[0.1..1.0]` als `const CONFIDENCE_BUCKETS`
  - `gateway_rule_based_confidence` (Family, Label: `anomaly_type`)
  - `gateway_ml_enhanced_confidence` (Family, Label: `anomaly_type`)
  - `record_ab_confidence(anomaly_type, rule_based, ml_enhanced)`
- [x] `src/anomaly_detector.rs`:
  - `impl std::fmt::Display for AnomalyType` — `"PossibleHijack"` / `"PrefixFlapping"`
  - `AnomalyDetector.metrics: Option<Arc<GatewayMetrics>>` neues Feld
  - `AnomalyDetector::with_metrics(alert_tx, metrics)` neuer Konstruktor
  - `AnomalyDetector::new()` bleibt unverändert (rückwärtskompatibel, `metrics: None`)
  - `check()`: `rule_based_confidence` vor Enhancement speichern → `tracing::debug!` mit beiden Werten → `record_ab_confidence()` wenn metrics vorhanden → ML-Score weiterhin maßgeblich
- [x] `src/lib.rs`: `AnomalyDetector::with_metrics(alert_tx, Arc::new(metrics.clone()))`
- [x] 4 Tests: `test_record_ab_confidence_both_histograms`, `test_ab_confidence_with_metrics`, `test_ab_confidence_without_metrics`, `test_anomaly_type_display`

#### 3.2.3-D — False-Positive-Rate Monitoring ✅
- [x] `src/metrics.rs`: 2 neue Counter (Family, Label: `anomaly_type`)
  - `gateway_anomalies_total` + `record_anomaly_detected(anomaly_type)`
  - `gateway_false_positives_total` + `record_false_positive(anomaly_type)`
- [x] `src/anomaly_detector.rs`: `check()` — `record_ab_confidence` + `record_anomaly_detected` in einem einzigen `if let`-Block zusammengeführt
- [x] `src/alert_api.rs`: resolve-Handler ruft `state.metrics.record_false_positive("unknown")` nach erfolgreichem Resolve auf
- [x] Fix: zwei redundante `if let Some(ref metrics)`-Blöcke in `check()` zu einem zusammengeführt
- [x] 4 Tests: `test_record_anomaly_detected_increments_counter`, `test_record_false_positive_increments_counter`, `test_anomaly_detected_called_in_check`, `test_false_positive_metrics_in_prometheus_output`

---

## 3.3 — RPKI-Vertiefung

**Zeitaufwand:** 2–3 Wochen
**Voraussetzung:** Stufe 2.3

### 3.3.1 — ROA-Zeitreihen ✅

- [x] `deploy/clickhouse/rpki_roa_history_schema.sql`: `rpki_roa_history` Tabelle (MergeTree, TTL 365 Tage)
- [x] `src/roa_poller.rs`: `RoaEntry`, `RoaDelta`, `RoaAction`, `RoaPoller`
- [x] `fetch_roas()`: GET `{routinator_url}/api/v1/export.json`, ASN-Parsing (AS64512 → 64512)
- [x] `compute_deltas()`: Set-Differenz added/removed, Mutex-gesicherter known-State
- [x] `write_deltas()`: JSONEachRow POST an ClickHouse, früh-return wenn leer
- [x] `poll_once()`: fetch → delta → write, gibt Anzahl Deltas zurück
- [x] `start()`: tokio::spawn Loop alle 5 Minuten, Fehler geloggt ohne abort
- [x] `lib.rs`: RoaPoller gestartet wenn `rpki.enabled && clickhouse.enabled`
- [x] 4 Tests: parse_asn, compute_deltas_added_removed, compute_deltas_initial_load, roa_action_as_str
- [x] ROA-Änderungen als potenzielle Hijack-Vorbereitung erkennen ✅
  - `RoaPoller.anomaly_tx: Option<mpsc::Sender<Anomaly>>` neues Feld
  - `RoaPoller::with_anomaly_sender(routinator_url, clickhouse_url, db, anomaly_tx)` neuer Konstruktor
  - `write_deltas()`: vor HTTP-Request — für jedes `Removed`-Delta → `Anomaly { confidence: 0.65, PossibleHijack, details: "ROA removed — potential hijack preparation" }` senden
  - Graceful degradation: `send`-Fehler (Receiver geschlossen) → `warn!` + continue, kein panic
  - `lib.rs`: `with_anomaly_sender()` wenn Sender verfügbar, sonst `RoaPoller::new()` als Fallback
  - 5 Tests: `test_roa_removed_triggers_anomaly`, `test_roa_added_does_not_trigger_anomaly`, `test_roa_anomaly_confidence_is_warning_level`, `test_no_panic_when_receiver_closed`, `test_poller_without_anomaly_sender`
  - Fix: 4 Test-Bugs von DeepSeek — `Ok(Ok(anomaly))` statt `Ok(Some(anomaly))` (falsche Pattern), `.unwrap()` auf HTTP-Fehler (3× Tests), leere else-Branch die kein Assertion hatte

### 3.3.2 — BGPSec (optional)

- [ ] BGPSec-Signatur-Validierung wenn vorhanden
- [ ] Metrik: Anteil BGPSec-signierter Pfade im Datenstrom

### 3.3.3 — IRR-Abgleich (Internet Routing Registry)

#### 3.3.3-A — IRR-Cache ✅
- [x] `src/irr_cache.rs`: `IrrStatus` enum (Consistent / Inconsistent / NotFound / Unavailable)
- [x] `IrrCache` mit moka (TTL 24h, max 50_000), reqwest Client (5s Timeout)
- [x] `check()`: RIPE REST API `search.json`, URL-Encoding, Accept: application/json
- [x] ASN-Parsing: `"AS64512"` → 64512 (case-insensitive, graceful skip bei Fehler)
- [x] `impl Default for IrrCache` (Clippy-konform)
- [x] `lib.rs`: `irr_cache` in AppState wenn `rpki.enabled`
- [x] `handlers/mod.rs`: `irr_cache: Option<Arc<IrrCache>>` in AppState
- [x] 4 Tests: consistent_detection, inconsistent_detection, parse_ripe_response, cache_new

#### 3.3.3-B — Kombinierter Score ✅
- [x] `check_with_rpki_status()` Signatur um `irr_status: &IrrStatus` erweitert
- [x] Confidence: RPKI invalid + IRR inconsistent → 0.75 (bekanntes AS) / 0.99 (neues AS)
- [x] Confidence: RPKI not-found + IRR inconsistent → 0.90 (statt 0.85 default)
- [x] RPKI valid → 0.30 unabhängig von IRR (unveränderter Pfad)
- [x] Details-String mit IRR-Note wenn inconsistent
- [x] `run_rpki_enrichment()` um `irr_cache: Option<Arc<IrrCache>>` erweitert
- [x] `lib.rs`: `irr_cache.clone()` an `run_rpki_enrichment` übergeben
- [x] 7 Tests: alle Confidence-Pfade + withdraw-guard

---

## 3.4 — Tenant-Management & Multi-Tenancy

**Zeitaufwand:** 2–3 Wochen

### 3.4.1 — Tenant-API

#### 3.4.1-A — Tenant-Manager ✅
- [x] `deploy/clickhouse/tenant_schema.sql`: `tenants` Tabelle (ReplacingMergeTree, Bloom-Filter Indizes)
- [x] `src/tenant_manager.rs`: `Tenant`, `TenantCreate`, `TenantUpdate` mit `ToSchema`
- [x] `TenantManagerClient`: `list`, `create`, `update`, `delete` (soft), `find_by_api_key`, `get_tenant`
- [x] `hash_api_key()`: SHA-256 hex, deterministisch, nie im Log
- [x] `tenant_from_row()`: UUID-Parsing, Timestamp-Fallback auf `Utc::now()`
- [x] `build_url()`: exakt gleiche Encode-Logik wie `alert_manager.rs`
- [x] `lib.rs`: `tenant_manager_client` in AppState wenn `clickhouse.enabled`
- [x] `handlers/mod.rs`: `tenant_manager_client: Option<Arc<TenantManagerClient>>`
- [x] 6 Tests: hash deterministisch, hash unterschiedlich, row→tenant, disabled, invalid timestamp, create hash

#### 3.4.1-B — Tenant-API Handler ✅
- [x] `src/tenant_api.rs`: 5 Handler mit utoipa-Annotationen (list, create, update, delete, get)
- [x] GET/POST `/tenants`, PUT/DELETE/GET `/tenants/:id`
- [x] `TenantApiResponse<T>` + `TenantApiError` Wrapper-Typen
- [x] HTTP 201 bei Create, 400 bei Validierungsfehlern, 404 bei nicht gefundenem Tenant, 503 bei ClickHouse-Fehler
- [x] `info!` Logging bei Create (nie api_key/hash im Log)
- [x] `lib.rs`: 5 Tenant-Routes nach Alert-Routes registriert
- [x] `handlers/mod.rs`: ApiDoc um 5 Paths + 4 Schemas erweitert
- [x] 4 Tests: error_serialization, response_serialization, validation_error_plan, validation_error_rate_limit
- [x] Per-Tenant Alert-Konfiguration ✅
  - `AlertRule.tenant_id: Option<String>` + `#[serde(default)]` in `src/alert_manager.rs`
  - `AlertRuleCreate.tenant_id: Option<String>` + `#[serde(default)]`
  - `deploy/clickhouse/alert_rules_schema.sql`: `tenant_id String DEFAULT ''`
  - `create_rule()`: `None → ""`, `Some(value) → value` beim INSERT
  - `update_rule()` + `delete_rule()`: `tenant_id` wird beibehalten
  - `check_rules_and_route()`: Filter `rule.tenant_id.is_none() || rule.tenant_id.as_deref() == current_tenant_id`
  - `run_alert_logger`: `_tenant_id: Option<String>` Parameter, `Some(&anomaly.tenant_id)` an `check_rules_and_route`
  - 4 Tests: `test_rule_with_matching_tenant_fires`, `test_rule_with_different_tenant_skips`, `test_global_rule_fires_for_any_tenant`, `test_alert_rule_create_with_tenant`
- [ ] Per-Tenant Dashboard in Grafana (Variable-Dropdown) — offen

### 3.4.2 — Quota-Management ✅

- [x] `src/quota_manager.rs`: `QuotaManager` mit moka-Cache (TTL 1s, AtomicU64 pro Tenant)
- [x] `QuotaResult` enum: `Allowed` (< 80%) / `SoftWarning { usage_pct }` (80–100%) / `Exceeded` (> 100%)
- [x] `check_and_increment(tenant_id: Uuid, rate_limit: u32)` — race-condition-frei via AtomicU64 + SeqCst
- [x] `Uuid::new_v5` aus `tenant_id`-String → deterministisches UUID für Quota-Key
- [x] Hard-Quota: `429 Too Many Requests` in `ingest_log` + `ingest_log_batch` bei `Exceeded`
- [x] Soft-Quota: Request durchgelassen + `record_quota_warning(tenant_id)` bei `SoftWarning`
- [x] `metrics.rs`: `record_quota_exceeded(tenant_id)` + `record_quota_warning(tenant_id)` (Family Counter mit tenant-Label)
- [x] Prometheus-Metriken: `gateway_quota_exceeded` + `gateway_quota_warning`
- [x] `AppState.quota_manager: Arc<QuotaManager>` — in `create_app` + `create_test_app`
- [x] `pub use quota_manager::QuotaManager` in `lib.rs`
- [x] 7 Tests: allowed, soft_warning_80pct, exceeded_100pct, concurrent_atomic, rate_limit_1, tenant_isolation, counter_expires_after_ttl
- [x] Fix: Concurrent-Test mit `rate_limit=500` statt 1000 (TTL-Problem + Off-by-one bei exakt 100%)
- [x] Monatliche Kosten-Reports per E-Mail ✅
  - `src/cost_reporter.rs`: `CostReporter { cost_tracker, smtp_config, http }`
  - `generate_report(summary: &[TenantStats]) -> String` — Plaintext mit Header, Tenant-Details, Footer
  - `send_report()`: HTTP POST an SMTP-Gateway (`http://{host}/api/v2/send`), JSON-Payload
  - `run_monthly()`: Background-Loop — `next_month_start()` berechnen → sleep → generate + send → wiederholen
  - `next_month_start(now)` — pure Funktion, Jahreswechsel (Dez→Jan) korrekt behandelt
  - Graceful degradation: `smtp.enabled=false` oder `to=[]` → früh-return ohne Fehler
  - `src/config.rs`: `SmtpConfig { enabled, host, port, username, password, from, to }` mit Default
  - `GatewayConfig.smtp: SmtpConfig` mit `#[serde(default)]`
  - `config/default.toml`: `[smtp]` Section
  - `lib.rs`: `tokio::spawn(reporter.run_monthly())` wenn `smtp.enabled`
  - 6 Tests: `test_generate_report_empty`, `test_generate_report_single_tenant`, `test_next_month_start_from_jan`, `test_next_month_start_from_dec`, `test_next_month_start_end_of_month`, `test_next_month_start_leap_year`
- [x] Rate-Limit aus Tenant-Config statt Hardcode 1000 ✅
  - `Tenant.rate_limit_per_sec: u32` (Default 1000) in `src/tenant_manager.rs`
  - `TenantCreate.rate_limit_per_sec: Option<u32>` + `TenantUpdate.rate_limit_per_sec: Option<u32>`
  - `TenantRow.rate_limit_per_sec: Option<u32>` → `tenant_from_row()`: `unwrap_or(1000)`
  - Validierung: `rate_limit == 0` → Error in `create_tenant` + `update_tenant`
  - `deploy/clickhouse/tenant_schema.sql`: `rate_limit_per_sec UInt32 DEFAULT 1000`
  - `handlers/mod.rs`: `ingest_log` + `ingest_log_batch` — `find_by_api_key()` → `tenant.rate_limit_per_sec`, Fallback 1000
  - Graceful degradation: ClickHouse-Fehler → `warn!` + Default 1000
  - TODO-Kommentar entfernt
  - 3 neue Tests: `test_tenant_create_with_custom_rate_limit`, `test_tenant_update_with_custom_rate_limit`, `test_tenant_row_missing_rate_limit_defaults_to_1000`

---

## 3.5 — Observability-Upgrade

**Zeitaufwand:** 1–2 Wochen

### 3.5-A — Distributed Tracing (OpenTelemetry + Jaeger) ✅

- [x] `Cargo.toml`: `opentelemetry 0.27` + `opentelemetry_sdk` + `opentelemetry-otlp` (tonic) + `opentelemetry-semantic-conventions` + `tracing-opentelemetry 0.28`
- [x] `src/telemetry.rs`: `init_tracer(service_name, otlp_endpoint: Option<&str>)` + `shutdown_tracer()`
- [x] Graceful degradation: kein Endpoint → early-return `Ok(())` (NoopTracer-Verhalten)
- [x] `OnceLock<Mutex<Option<Arc<TracerProvider>>>>` — safe, kein `static mut` / UB
- [x] `TracerProvider` mit `BatchSpanProcessor` + OTLP gRPC (tonic)
- [x] `Resource` mit `SERVICE_NAME` + `SERVICE_VERSION` (semantic conventions)
- [x] `set_global_default(Registry::default().with(telemetry_layer))` — tracing-Integration
- [x] `src/config.rs`: `TelemetryConfig { enabled, otlp_endpoint, service_name }` mit Defaults
- [x] `GatewayConfig.telemetry: TelemetryConfig` mit `#[serde(default)]`
- [x] `config/default.toml`: `[telemetry] enabled = false` + defaults
- [x] `src/main.rs`: `init_tracer` bei Startup wenn `enabled`, `shutdown_tracer` bei Shutdown
- [x] `src/handlers/mod.rs`: `#[instrument(skip(state, body), fields(tenant_id, pii_hits))]` auf `ingest_log`
- [x] `Span::current().record("tenant_id", ...)` nach Header-Extraktion
- [x] `Span::current().record("pii_hits", ...)` nach Redaction (Cache HIT + MISS)
- [x] Fix: `static mut TRACER_PROVIDER` → `OnceLock<Mutex<Option<...>>>` (Clippy `static_mut_refs` Error)
- [x] 5 Tests: noop_when_disabled (None / empty / whitespace), shutdown_without_init, telemetry_config_default, config_deserialization, config_partial_deserialization

### 3.5-B — Structured Log Shipping (Loki) ✅

- [x] `Cargo.toml`: `tracing-loki 0.2` (compat-0-2-1) + `url 2`
- [x] `src/loki_logger.rs`: `build_loki_layer(endpoint, service_name)` → `Option<(Layer, BackgroundTask)>`
- [x] Graceful degradation: leer/ungültige URL → `Ok(None)`, kein Panic
- [x] Labels: `service` (primär via `.label()`), `environment` (RUST_ENV / "production"), `version` (`option_env!`)
- [x] `src/config.rs`: `LokiConfig { enabled, endpoint, service_name }` mit `default_loki_endpoint()` + `default_loki_service_name()`
- [x] `GatewayConfig.loki: LokiConfig` mit `#[serde(default)]`
- [x] `config/default.toml`: `[loki] enabled = false` + defaults
- [x] `src/main.rs`: `init_tracing_with_loki()` — baut Registry mit `fmt_layer` + optionalem `loki_layer`, `tokio::spawn(background_task)`
- [x] LOG_FORMAT="json"/"text" Unterstützung beibehalten
- [x] Fix 1: `use log_gateway::logging` entfernt (unused import → Clippy Error)
- [x] Fix 2: `"service"` doppelt in Labels (direkt + in HashMap) → aus `build_loki_labels` entfernt
- [x] Fix 3: `std::env::var("CARGO_PKG_VERSION")` → `option_env!("CARGO_PKG_VERSION")` (Build-Time-Macro)
- [x] Fix 4: Test-Race-Condition (`set_var` parallel) → Tests ohne globalen Env-State neu geschrieben
- [x] 7 Tests: noop_empty, noop_invalid_url, valid_endpoint, labels_structure, labels_env_fallback, loki_config_default, loki_config_deserialization, loki_config_partial

### 3.5-C — SLO-Dashboard (Prometheus-Metriken) ✅

- [x] `metrics.rs`: `requests_5xx_total: Arc<Counter>` — neues Feld
- [x] `metrics.rs`: `gateway_up: Arc<Gauge>` — neues Feld
- [x] `record_5xx()` — inkrementiert `gateway_5xx_total` Counter
- [x] `record_gateway_up()` — setzt `gateway_up` Gauge auf 1
- [x] Prometheus-Registrierung: `gateway_5xx_total` + `gateway_up` mit Beschreibungen
- [x] `lib.rs`: `metrics.record_gateway_up()` direkt nach `GatewayMetrics::new()` (in `create_app` + `create_test_app`)
- [x] `handlers/mod.rs`: `state.metrics.record_5xx()` bei `INTERNAL_SERVER_ERROR` in `trigger_s3_export`
- [x] Bestehendes `request_duration_ms` Histogramm deckt p99-Latenz bereits ab
- [x] 4 Tests: record_5xx_increments_counter, record_gateway_up_sets_gauge, gateway_up_initial_zero, metrics_output_contains_slo_metrics

---

## Aufwand-Zusammenfassung

| Phase | Aufwand | Risiko | Priorität |
|-------|---------|--------|-----------|
| 3.1 Alert-Engine | 3–4 Wochen | Gering | **Erste** |
| 3.2 ML-Anomalie | 4–5 Wochen | Hoch | **Dritte** |
| 3.3 RPKI-Vertiefung | 2–3 Wochen | Gering | **Zweite** |
| 3.4 Multi-Tenancy | 2–3 Wochen | Mittel | **Vierte** |
| 3.5 Observability | 1–2 Wochen | Gering | **Parallel** |
| **Gesamt** | **12–17 Wochen** | | |

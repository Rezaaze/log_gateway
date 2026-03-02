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
- [ ] Dynamisches Laden der Rules zur Laufzeit (Hot-Reload via SIGHUP) — offen

### 3.1.2 — Eskalationsstufen ✅

- [x] `src/escalation.rs`: `EscalationLevel` (Warning 0.5–0.7 / Critical 0.7–0.9 / Emergency >0.9)
- [x] `EscalationRouter::route()` mit persist + metrics + tracing
- [x] `metrics.rs`: `record_escalation()` als Prometheus Family Counter
- [x] `alertmanager.yml`: 3-stufiges Routing + inhibit_rules (Slack Warning/Critical, PagerDuty Emergency)
- [x] `run_alert_logger` erweitert mit `escalation_router: Option<Arc<EscalationRouter>>`
- [ ] Automatische Eskalation wenn Alert nicht acknowledged — offen

### 3.1.3 — Alert-Deduplication & Silence ✅

- [x] `src/alert_dedup.rs`: SHA-256 Fingerprint + `DedupCache` (moka, TTL 30min, max 100k)
- [x] `alert_silences` Tabelle in ClickHouse (`ORDER BY id`, ReplacingMergeTree)
- [x] `create_silence`, `list_silences`, `is_silenced`, `expire_silence` in `AlertManagerClient`
- [x] Silence-API: POST `/alerts/:id/silence`, GET `/alerts/silences`, DELETE `/alerts/silences/:id`
- [x] Dedup + Silence-Check im `EscalationRouter` vor persist (graceful degradation)

### 3.1.4 — Webhook-Integration

- [ ] Generischer Webhook-Sender (beliebige HTTP-Endpoints)
- [ ] Template-Engine für Alert-Nachrichten (Handlebars o.ä.)
- [ ] Integrationen: Slack, PagerDuty, OpsGenie, MS Teams, Discord

---

## 3.2 — ML-gestützte Anomalie-Erkennung

**Zeitaufwand:** 4–5 Wochen
**Risiko:** Hoch (Modell-Qualität, Trainings-Aufwand)
**Impact:** Sehr hoch — reduziert False Positives drastisch

### 3.2.1 — Baseline-Modell

- [ ] Statistisches Modell (kein Deep Learning):
  - Exponential Moving Average (EMA) für AS-Path-Länge pro Prefix
  - Z-Score-basierte Ausreißer-Erkennung
  - Saisonalität: Tages-/Wochenmuster berücksichtigen
- [ ] Implementierung in Rust (ndarray + linreg crates)
- [ ] Training: täglich auf letzten 30 Tagen ClickHouse-Daten

### 3.2.2 — Feature Engineering

Features pro BGP-Event:
- AS-Path-Länge (aktuell vs. Median der letzten 7 Tage)
- Prefix-Länge
- Zeit seit letztem ANNOUNCE/WITHDRAW
- Bekanntheitsgrad des Origin-AS (Anzahl Tage im Datensatz)
- RPKI-Status (Valid/Invalid/NotFound)
- Geografische AS-Distanz (via CAIDA AS-Rank)

### 3.2.3 — Model-Serving

- [ ] Modell-Artefakt in ClickHouse oder MinIO persistieren
- [ ] Täglicher Retraining-Job (Tokio-Cron-Task)
- [ ] A/B-Test: Rule-Based vs. ML-Score nebeneinander
- [ ] Monitoring: False-Positive-Rate als Grafana-Panel

---

## 3.3 — RPKI-Vertiefung

**Zeitaufwand:** 2–3 Wochen
**Voraussetzung:** Stufe 2.3

### 3.3.1 — ROA-Zeitreihen

- [ ] ROA-Änderungen historisch tracken (Routinator → ClickHouse):
  ```sql
  CREATE TABLE rpki_roa_history (
      timestamp  DateTime64(3),
      prefix     String,
      max_length UInt8,
      origin_as  UInt32,
      trust_anchor LowCardinality(String),  -- RIPE, ARIN, APNIC, ...
      action     LowCardinality(String)     -- added/removed
  ) ENGINE = MergeTree()
  ORDER BY (timestamp, prefix, origin_as);
  ```
- [ ] ROA-Änderungen als potenzielle Hijack-Vorbereitung erkennen

### 3.3.2 — BGPSec (optional)

- [ ] BGPSec-Signatur-Validierung wenn vorhanden
- [ ] Metrik: Anteil BGPSec-signierter Pfade im Datenstrom

### 3.3.3 — IRR-Abgleich (Internet Routing Registry)

- [ ] IRR-Daten von RIPE DB laden (täglicher Refresh via FTP-Bulk-Download)
- [ ] Abgleich Origin-AS mit `route-object` in IRR
- [ ] Kombinierter Score: RPKI + IRR + Historische Baseline

---

## 3.4 — Tenant-Management & Multi-Tenancy

**Zeitaufwand:** 2–3 Wochen

### 3.4.1 — Tenant-API

Aktuell: Tenants nur über `X-Tenant-ID` Header identifiziert.
Ziel: Vollwertige Tenant-Verwaltung.

- [ ] Tenant-Datenbank in ClickHouse:
  ```sql
  CREATE TABLE tenants (
      id          UUID DEFAULT generateUUIDv4(),
      name        String,
      api_key_hash String,   -- SHA-256 des API-Keys
      rate_limit  UInt32,    -- req/s
      plan        LowCardinality(String),  -- free/pro/enterprise
      created_at  DateTime DEFAULT now()
  ) ENGINE = ReplacingMergeTree(created_at)
  ORDER BY id;
  ```
- [ ] CRUD-API für Tenants (Admin-only, separater JWT-Scope)
- [ ] Per-Tenant Alert-Konfiguration (jeder Tenant eigene Rules)
- [ ] Per-Tenant Dashboard in Grafana (via Variable-Dropdown)

### 3.4.2 — Quota-Management

- [ ] Soft-Quota: Warnung bei 80% des Limits
- [ ] Hard-Quota: `429 Too Many Requests` + Alert an Tenant
- [ ] Monatliche Kosten-Reports per E-Mail

---

## 3.5 — Observability-Upgrade

**Zeitaufwand:** 1–2 Wochen

- [ ] Distributed Tracing (OpenTelemetry / Jaeger)
  - Trace-Kontext von HAProxy bis ClickHouse-Write
  - `tracing-opentelemetry` crate in `src/main.rs`
- [ ] Structured Log Shipping (Loki → Grafana)
  - Gateway-Logs direkt in Grafana Loki shippen
  - Korrelation Logs ↔ Metriken ↔ Traces in Grafana
- [ ] SLO-Dashboard:
  - Availability: `gateway_up` / `gateway_total_requests`
  - Error-Rate: `5xx / total`
  - Latenz-SLO: p99 < 50ms

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

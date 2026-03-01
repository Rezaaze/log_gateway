# Roadmap — Stufe 3: Alert-Engine & RPKI-Vertiefung

> **Status: 🟡 VISIONÄR**
> Geschätzter Aufwand: 8–12 Wochen solo / 5–7 Wochen im Team
> Voraussetzung: Stufe 2 abgeschlossen ✅

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

### 3.1.1 — Alert-Konfiguration per API

Aktuell: Alerts sind hart kodiert in `alerts.yml`.
Ziel: Alerts dynamisch über API konfigurierbar.

- [ ] Neue Datenbank-Tabelle in ClickHouse:
  ```sql
  CREATE TABLE alert_rules (
      id          UUID DEFAULT generateUUIDv4(),
      name        String,
      description String,
      rule_type   LowCardinality(String),  -- hijack/flap/leak/custom
      config      String,                  -- JSON-konfigurierter Schwellwert
      enabled     Bool DEFAULT true,
      created_at  DateTime DEFAULT now(),
      updated_at  DateTime DEFAULT now()
  ) ENGINE = ReplacingMergeTree(updated_at)
  ORDER BY id;
  ```
- [ ] CRUD-API-Endpunkte:
  ```
  GET    /api/v1/alerts/rules           # Alle Rules
  POST   /api/v1/alerts/rules           # Neue Rule anlegen
  PUT    /api/v1/alerts/rules/:id       # Rule aktualisieren
  DELETE /api/v1/alerts/rules/:id       # Rule deaktivieren
  GET    /api/v1/alerts/active          # Aktive Alerts
  POST   /api/v1/alerts/:id/silence     # Alert stumm schalten
  ```
- [ ] Dynamisches Laden der Rules zur Laufzeit (Hot-Reload via SIGHUP)

### 3.1.2 — Eskalationsstufen

- [ ] 3-stufiges Eskalationsmodell:
  ```
  Stufe 1 (Warning):  Confidence 0.5–0.7 → Slack #bgp-alerts
  Stufe 2 (Critical): Confidence 0.7–0.9 → Slack #bgp-critical + E-Mail
  Stufe 3 (Emergency): Confidence > 0.9 → PagerDuty + alle Kanäle
  ```
- [ ] Alertmanager-Routing in `deploy/alertmanager/alertmanager.yml` erweitern
- [ ] Automatische Eskalation wenn Alert nach T Minuten nicht acknowledged

### 3.1.3 — Alert-Deduplication & Silence

- [ ] Fingerprint-basierte Deduplication (gleicher Prefix + AS → ein Alert)
- [ ] Silence-API: Alert für X Stunden unterdrücken (Maintenance-Windows)
- [ ] Alert-History in ClickHouse persistieren:
  ```sql
  CREATE TABLE alert_history (
      id           UUID,
      rule_id      UUID,
      alert_type   String,
      prefix       String,
      origin_as    UInt32,
      confidence   Float64,
      status       LowCardinality(String),  -- firing/resolved/silenced
      fired_at     DateTime64(3),
      resolved_at  Nullable(DateTime64(3))
  ) ENGINE = MergeTree()
  ORDER BY (fired_at, alert_type);
  ```

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

# Roadmap — Stufe 2: Query-Engine & Anomalie-Erkennung

> **Status: 🔵 GEPLANT**
> Geschätzter Aufwand: 10–15 Wochen solo / 6–8 Wochen im Team
> Voraussetzung: Stufe 1 abgeschlossen ✅

---

## Ziel

Die gesammelten BGP-Daten aus einem reinen Write-Only-Store in eine
abfragbare, analysierbare Plattform verwandeln. Erste automatische
Anomalie-Erkennung (Route Hijacks, BGP Leaks, Prefix Flapping).

---

## 2.1 — ClickHouse als Query-Engine

**Zeitaufwand:** 4–6 Wochen
**Risiko:** Mittel (neues System, Lernkurve)
**Impact:** Hoch — macht alle gespeicherten Daten abfragbar

### Warum ClickHouse?

- Spaltenorientiert → ideal für BGP-Zeitreihen (hohe Kompression, schnelle Aggregationen)
- Sub-Sekunden-Queries über Milliarden Rows
- Natives HTTP-Interface → kein neuer SDK-Client nötig
- Kompression ~10:1 gegenüber NDJSON

### 2.1.1 — Infrastruktur

- [ ] ClickHouse-Service in `docker-compose.prod.yml` ergänzen:
  ```yaml
  clickhouse:
    image: clickhouse/clickhouse-server:latest
    ports:
      - "8123:8123"   # HTTP API
      - "9009:9009"   # Native Protocol
    volumes:
      - clickhouse-data:/var/lib/clickhouse
    networks:
      - gateway-net
  ```
- [ ] Volume `clickhouse-data` in Compose registrieren
- [ ] ClickHouse-Konfig: Retention 90 Tage, Kompression `zstd`

### 2.1.2 — Tabellen-Schema

```sql
CREATE TABLE bgp_events (
    timestamp   DateTime64(3, 'UTC'),
    event_type  LowCardinality(String),   -- ANNOUNCE / WITHDRAW
    prefix      String,                   -- z.B. 1.2.3.0/24
    origin_as   UInt32,                   -- Origin AS-Nummer
    as_path     Array(UInt32),            -- [13335, 3356, 1234]
    peer_asn    UInt32,
    peer_ip     String,
    community   Array(String),
    source      LowCardinality(String),   -- "ris-live"
    tenant_id   LowCardinality(String)
) ENGINE = MergeTree()
PARTITION BY toYYYYMM(timestamp)
ORDER BY (origin_as, prefix, timestamp)
TTL timestamp + INTERVAL 90 DAY;
```

- [ ] Migrations-Skript in `deploy/clickhouse/schema.sql`
- [ ] Automatische Schema-Anwendung im CI/CD-Deploy-Job

### 2.1.3 — ClickHouse-Exporter (Rust)

Neue Datei: `src/clickhouse_exporter.rs`

- [ ] Parallel zum bestehenden `sink.rs` (kein Breaking Change)
- [ ] Batch-INSERT via ClickHouse HTTP API (`reqwest`, JSON)
- [ ] Konfigurierbar über `config/default.toml`:
  ```toml
  [clickhouse]
  enabled = false
  url = "http://clickhouse:8123"
  database = "bgp"
  batch_size = 1000
  flush_interval_secs = 5
  ```
- [ ] Feature-Flag `GATEWAY__CLICKHOUSE__ENABLED` als ENV-Variable
- [ ] Backpressure: bei ClickHouse-Timeout → Drop mit Metric-Increment

### 2.1.4 — Neue API-Endpunkte

```
GET  /api/v1/bgp/prefixes/:prefix/history   # Zeitverlauf eines Prefix
GET  /api/v1/bgp/asn/:asn/prefixes          # Alle Prefixe eines AS
GET  /api/v1/bgp/events?from=&to=&type=     # Gefilterte Events mit Pagination
GET  /api/v1/bgp/stats/top-as?limit=10      # Top-AS nach Event-Volumen
```

- [ ] Query-Parameter: `from`, `to` (ISO 8601), `type` (announce/withdraw), `limit`, `offset`
- [ ] Response-Pagination (cursor-based)
- [ ] OpenAPI-Dokumentation via utoipa erweitern

---

## 2.2 — Anomalie-Erkennung (Rule-Based)

**Zeitaufwand:** 3–4 Wochen
**Risiko:** Gering (Rust, bekannte Patterns)
**Impact:** Sehr hoch — Kerndifferenziator der Plattform

### 2.2.1 — Detector-Architektur

Neue Datei: `src/anomaly_detector.rs`

- [ ] Trait `Detector` mit Methode `fn check(&self, event: &BgpEvent) -> Option<Anomaly>`
- [ ] In-Memory State-Store für historische Baseline (moka, TTL 24h)
- [ ] Async-Pipeline: BGP-Event → Detector-Chain → Alert-Queue

### 2.2.2 — Regel 1: Route Hijack Detection

```
Trigger: Prefix X wird von AS Y announced
         AS Y hat Prefix X nie zuvor angekündigt
         AS Y erscheint nicht in der AS-Path-History von X
```

- [ ] Historische AS-Path-Map im Memory: `HashMap<Prefix, HashSet<AsNumber>>`
- [ ] Abgleich gegen ClickHouse-History beim Start (Warmup)
- [ ] Confidence-Score: 0.0–1.0 (basierend auf Anzahl bekannter AS-Paths)
- [ ] Alert nur wenn Confidence > 0.8 (vermeidet False Positives bei neuen Prefixen)

### 2.2.3 — Regel 2: BGP Leak Detection

```
Trigger: AS Z (Stub-AS, kein Transit) advertised plötzlich
         Prefixe mit AS-Path-Länge > 2 (Transit-typisch)
         Prefixe mit Präfix-Länge /8–/16
```

- [ ] AS-Typ-Klassifikation (Stub / Transit) via CAIDA AS-Rank API (täglicher Refresh)
- [ ] Leak-Score basierend auf Präfix-Größe und AS-Path-Länge

### 2.2.4 — Regel 3: Prefix Flapping

```
Trigger: Prefix X hat > N ANNOUNCE/WITHDRAW-Wechsel
         innerhalb von T Minuten
```

- [ ] Sliding-Window Counter (Tokio-Channel + Timer)
- [ ] Konfigurierbare Schwellwerte:
  ```toml
  [anomaly.flap]
  threshold = 10       # Wechsel
  window_secs = 300    # 5 Minuten
  ```

### 2.2.5 — Regel 4: Ungewöhnliche AS-Path-Länge

```
Trigger: AS-Path für bekannten Prefix plötzlich > 3 Hops
         länger als historischer Median
```

- [ ] Median-Berechnung über letzte 1000 Events pro Prefix (Rolling Window)

### 2.2.6 — Alert-Pipeline

- [ ] `Anomaly`-Struct: `{type, prefix, asn, confidence, detected_at, details}`
- [ ] Anomaly → Alertmanager-Webhook (`POST /api/alertmanager/alerts`)
- [ ] Neue Prometheus-Metriken in `src/metrics.rs`:
  ```
  bgp_anomaly_hijack_detected{prefix, origin_as}   Counter
  bgp_anomaly_flap_detected{prefix}                Counter
  bgp_anomaly_leak_detected{as}                    Counter
  bgp_unique_prefixes_seen                         Gauge
  bgp_unique_as_seen                               Gauge
  ```
- [ ] Alert-Rules in `deploy/alertmanager/alerts.yml` ergänzen:
  ```yaml
  - alert: BGPRoutePossibleHijack
    expr: bgp_anomaly_hijack_detected > 0
    for: 1m
    labels:
      severity: critical
  ```

---

## 2.3 — RPKI-Validierung

**Zeitaufwand:** 2–3 Wochen
**Risiko:** Gering (externe Datenquelle, gut dokumentiert)
**Impact:** Hoch — reduziert False Positives erheblich

### 2.3.1 — Routinator als RPKI-Validator

- [ ] Routinator-Service in `docker-compose.prod.yml`:
  ```yaml
  routinator:
    image: nlnetlabs/routinator:latest
    ports:
      - "8323:8323"   # HTTP API (ROA-Liste als JSON)
    networks:
      - gateway-net
    restart: unless-stopped
  ```
- [ ] Initiales ROA-Fetch beim Start (Bootstrapping dauert ~5 Min)

### 2.3.2 — RPKI-Cache (Rust)

Neue Datei: `src/rpki_cache.rs`

- [ ] ROA-Liste von `http://routinator:8323/json` laden
- [ ] In-Memory Cache (moka, stündlicher Refresh via Tokio-Timer)
- [ ] RPKI-Status-Enum:
  ```rust
  pub enum RpkiStatus {
      Valid,           // ROA vorhanden, AS + Prefix korrekt
      InvalidAsn,      // ROA vorhanden, aber anderes AS
      InvalidLength,   // ROA vorhanden, Prefix-Länge falsch
      NotFound,        // Kein ROA (nicht zwingend Fehler)
  }
  ```
- [ ] Funktion: `fn validate(prefix: &str, origin_as: u32) -> RpkiStatus`

### 2.3.3 — Integration mit Anomalie-Erkennung

- [ ] RPKI-Status als zusätzliches Signal in Hijack-Detector:
  - `RpkiStatus::Invalid + neues AS` → **kritischer Alert** (Confidence 1.0)
  - `RpkiStatus::Invalid + bekanntes AS` → **Warnung** (Confidence 0.6)
  - `RpkiStatus::Valid` → Confidence reduzieren (wahrscheinlich legitim)
- [ ] RPKI-Status in BGP-Event-Response und ClickHouse-Schema ergänzen

---

## 2.4 — Dashboard-Erweiterung

**Zeitaufwand:** 1–2 Wochen
**Risiko:** Sehr gering
**Impact:** Mittel — Sichtbarkeit der neuen Features

### 2.4.1 — Neue Grafana-Panels

Erweiterung von `deploy/grafana/provisioning/dashboards/gateway.json`:

| Panel | Typ | Datasource |
|-------|-----|------------|
| BGP Events/s nach Typ (ANNOUNCE/WITHDRAW) | Time Series | Prometheus |
| Top 10 Origin-AS nach Volume | Bar Chart | ClickHouse |
| RPKI Status Verteilung | Pie Chart | Prometheus |
| Aktive Anomaly-Alerts | Alert List | Alertmanager |
| Flapping Prefixe (24h) | Table | ClickHouse |
| Hijack-Erkennungen Timeline | Time Series | Prometheus |

### 2.4.2 — ClickHouse Grafana Plugin

- [ ] `grafana-clickhouse-datasource` Plugin in Grafana-Container einbinden
- [ ] Datasource-Provisioning in `deploy/grafana/provisioning/datasources/clickhouse.yml`

---

## Abhängigkeiten & Reihenfolge

```
2.1 ClickHouse Setup & Exporter
    └── 2.1.4 Neue API-Endpunkte
        └── 2.2 Anomalie-Erkennung
            │   (braucht historische Daten aus ClickHouse als Baseline)
            └── 2.3 RPKI-Validierung (parallel zu 2.2 möglich)
                └── 2.4 Dashboard-Erweiterung (letzte Phase)
```

---

## Was sich NICHT ändert

- `bgp-stream` bleibt unverändert
- Gateway-Cluster (4× Instanzen + HAProxy) bleibt identisch
- Bestehende API-Endpunkte bleiben kompatibel (keine Breaking Changes)
- GitHub Actions CI/CD läuft weiter automatisch
- NDJSON-Sink bleibt als Backup/Fallback erhalten

---

## Aufwand-Zusammenfassung

| Phase | Aufwand | Priorität |
|-------|---------|-----------|
| 2.1 ClickHouse | 4–6 Wochen | **Erste** — alles andere baut darauf auf |
| 2.2 Anomalie-Erkennung | 3–4 Wochen | **Zweite** — Kern-Feature |
| 2.3 RPKI | 2–3 Wochen | **Dritte** — Qualitätsverbesserung |
| 2.4 Dashboards | 1–2 Wochen | **Letzte** — Abschluss |
| **Gesamt** | **10–15 Wochen** | |

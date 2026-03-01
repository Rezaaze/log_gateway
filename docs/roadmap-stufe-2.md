# Roadmap — Stufe 2: Query-Engine & Anomalie-Erkennung

> **Status: ✅ ABGESCHLOSSEN**
> Implementiert: Phase 2.1 – 2.4 vollständig
> Voraussetzung: Stufe 1 abgeschlossen ✅

---

## Implementierungs-Notizen

### Abweichungen & Fixes gegenüber ursprünglicher Planung

| # | Problem | Fix |
|---|---------|-----|
| 1 | ClickHouseExporter: Query-String nicht URL-encoded → alle INSERTs schlugen fehl | Leerzeichen/Sonderzeichen via `.replace()` kodiert |
| 2 | S3-Exporter: `if enabled { None } else { None }` — Feature-Flag wurde ignoriert | Kommentar klargestellt, S3 arbeitet on-demand |
| 3 | RPKI: `run_rpki_enrichment` + `check_with_rpki` riefen je einmal Routinator auf → 2 HTTP-Requests pro Event | `check_with_rpki_status()` nimmt vorher ermittelten `RpkiStatus` — 1 Request pro Event |
| 4 | ClickHouse Port 9000 fehlte in `docker-compose.prod.yml` → Grafana-Plugin konnte sich nie verbinden | `"9000:9000"` ergänzt |
| 5 | Buffer-Drop im ClickHouseExporter war unsichtbar (kein Log, keine Metrik) | `record_clickhouse_flush_error()` + `tracing::warn!` beim Drop |
| 6 | Gateway-Services hatten kein `depends_on: clickhouse` → HijackDetector-Warmup schlug beim ersten Start fehl | `condition: service_healthy` für alle 4 Gateway-Instanzen |
| 7 | RPKI Confidence-Logik: bekanntes AS + RPKI invalid hatte 0.95 statt 0.6 | Korrigiert auf 0.6 gemäß Roadmap 2.3.3 |
| 8 | Routinator-Port 8323 fehlte in `docker-compose.prod.yml` | `"8323:8323"` ergänzt |
| 9 | `run_rpki_enrichment` rief `record_rpki_valid/invalid` nie auf | Metrics-Parameter zu `run_rpki_enrichment` hinzugefügt |
| 10 | Grafana Piechart Panel 4: `values: false` → leeres Diagramm | `values: true`, `fields: "/^total$/"` |
| 11 | `orgId: 1` fehlte in `clickhouse.yml` | Ergänzt |

### Neue Dateien
- `src/clickhouse_exporter.rs` — Batch-INSERT, ArrayQueue, Backpressure
- `src/bgp_query.rs` — 4 BGP-Query-Endpunkte, utoipa-annotiert
- `src/anomaly_detector.rs` — Detector-Trait, HijackDetector, FlappingDetector, run_alert_logger, run_rpki_enrichment
- `src/rpki_cache.rs` — moka-Cache, Routinator HTTP-Client, graceful degradation
- `deploy/clickhouse/schema.sql` — BGP-Events-Tabelle, TTL 90 Tage
- `deploy/clickhouse/init.sql` — Datenbank-Init
- `deploy/grafana/provisioning/dashboards/bgp_clickhouse.json` — ClickHouse Analytics Dashboard
- `deploy/grafana/provisioning/datasources/clickhouse.yml` — ClickHouse Datasource

### Geänderte Dateien
- `src/lib.rs` — create_app() async, alle neuen Module verdrahtet
- `src/handlers/mod.rs` — AppState um rpki_tx, clickhouse_exporter, bgp_query_client erweitert
- `src/metrics.rs` — 4 neue Counter: hijack, flap, rpki_valid, rpki_invalid
- `src/config.rs` — ClickHouseConfig, RpkiConfig
- `config/default.toml` — [clickhouse] + [rpki] Blöcke
- `docker-compose.prod.yml` — ClickHouse, Routinator, Grafana-Plugin, Port 9000, depends_on
- `deploy/grafana/provisioning/dashboards/gateway.json` — Row "BGP Intelligence" + 6 neue Panels

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

- [x] ClickHouse-Service in `docker-compose.prod.yml` ergänzen
- [x] Volume `clickhouse-data` in Compose registrieren
- [x] ClickHouse-Konfig: Retention 90 Tage, TTL via MergeTree
- [x] `depends_on: clickhouse (service_healthy)` für alle 4 Gateway-Services

### 2.1.2 — Tabellen-Schema

- [x] Migrations-Skript in `deploy/clickhouse/schema.sql`
- [x] `deploy/clickhouse/init.sql` via `docker-entrypoint-initdb.d`

### 2.1.3 — ClickHouse-Exporter (Rust)

- [x] `src/clickhouse_exporter.rs` — parallel zu `sink.rs`, kein Breaking Change
- [x] Batch-INSERT via ClickHouse HTTP API (`reqwest`, JSONEachRow), URL-encoded
- [x] Konfigurierbar via `config/default.toml` + ENV `GATEWAY__CLICKHOUSE__ENABLED`
- [x] Backpressure: Buffer voll → Drop + `record_clickhouse_flush_error()` + `tracing::warn!`
- [x] Retry-Logik: 2 Versuche mit 100ms Pause

### 2.1.4 — Neue API-Endpunkte

- [x] `GET /api/v1/bgp/prefixes/:prefix/history`
- [x] `GET /api/v1/bgp/asn/:asn/prefixes`
- [x] `GET /api/v1/bgp/events?from=&to=&type=&limit=&offset=`
- [x] `GET /api/v1/bgp/stats/top-as?limit=10`
- [x] Input-Validierung: Prefix sanitization, event_type whitelist
- [x] OpenAPI-Dokumentation via utoipa

---

## 2.2 — Anomalie-Erkennung (Rule-Based)

**Zeitaufwand:** 3–4 Wochen
**Risiko:** Gering (Rust, bekannte Patterns)
**Impact:** Sehr hoch — Kerndifferenziator der Plattform

### 2.2.1 — Detector-Architektur

- [x] Trait `Detector`: `fn check(&self, event: &BgpClickHouseRecord) -> Option<Anomaly>`
- [x] `AnomalyDetector` orchestriert Detector-Chain
- [x] Async Alert-Queue via `tokio::sync::mpsc`

### 2.2.2 — Regel 1: Route Hijack Detection

- [x] `HijackDetector`: `DashMap<Prefix, HashSet<AsNumber>>`
- [x] ClickHouse-Warmup beim Start (letzte 7 Tage)
- [x] Confidence-Score 0.85 bei neuem AS, anpassbar durch RPKI

### 2.2.3 — Regel 2: BGP Leak Detection

- [ ] AS-Typ-Klassifikation via CAIDA AS-Rank API — **offen, Stufe 2 Next-Sprint**

### 2.2.4 — Regel 3: Prefix Flapping

- [x] `FlappingDetector`: Sliding-Window (5 Min, Threshold 10 Events)
- [x] `DashMap<Prefix, VecDeque<DateTime>>`, Confidence 0.75

### 2.2.5 — Regel 4: Ungewöhnliche AS-Path-Länge

- [ ] Median-Berechnung Rolling Window — **offen, Stufe 2 Next-Sprint**

### 2.2.6 — Alert-Pipeline

- [x] `Anomaly`-Struct: `{id, type, prefix, origin_as, confidence, detected_at, details}`
- [x] `run_alert_logger` Task: log + `record_bgp_anomaly_hijack/flap`
- [x] Metriken: `gateway_bgp_anomaly_hijack_total`, `gateway_bgp_anomaly_flap_total`

---

## 2.3 — RPKI-Validierung

**Zeitaufwand:** 2–3 Wochen
**Risiko:** Gering (externe Datenquelle, gut dokumentiert)
**Impact:** Hoch — reduziert False Positives erheblich

### 2.3.1 — Routinator als RPKI-Validator

- [x] Routinator-Service in `docker-compose.prod.yml` mit Port 8323 + Volume
- [x] Healthcheck mit 300s start_period (ROA-Fetch dauert ~5 Min)

### 2.3.2 — RPKI-Cache (Rust)

- [x] `src/rpki_cache.rs`: moka-Cache (100k Einträge, TTL 1h)
- [x] `RpkiStatus`: Valid / InvalidAsn / InvalidLength / NotFound / Unavailable
- [x] `validate(prefix, origin_as)` async — graceful degradation → Unavailable bei Fehler
- [x] URL-Encoding: Prefix-Slash via `%2F` kodiert

### 2.3.3 — Integration mit Anomalie-Erkennung

- [x] `check_with_rpki_status()` nimmt vorher ermittelten Status (kein Doppel-Request)
- [x] Confidence-Logik:
  - Neues AS + RPKI invalid → 0.97
  - Neues AS + RPKI valid → 0.3
  - Bekanntes AS + RPKI invalid → 0.6 (Warnung)
- [x] `run_rpki_enrichment` Task mit Metrics (`rpki_valid_total`, `rpki_invalid_total`)
- [ ] RPKI-Status im ClickHouse-Schema — **offen, erfordert Schema-Migration**

---

## 2.4 — Dashboard-Erweiterung

**Zeitaufwand:** 1–2 Wochen
**Risiko:** Sehr gering
**Impact:** Mittel — Sichtbarkeit der neuen Features

### 2.4.1 — Neue Grafana-Panels

- [x] Row "BGP Intelligence" in `gateway.json`
- [x] Panel: BGP Events/s (timeseries, Prometheus)
- [x] Panel: RPKI Status Verteilung (piechart, Prometheus, Valid=green/Invalid=red)
- [x] Panel: Hijack-Erkennungen Timeline (timeseries, thresholds)
- [x] Panel: Prefix Flapping Ereignisse (timeseries)
- [x] Panel: RPKI Validierungsrate (timeseries)
- [x] Panel: Aktive Anomalien gesamt (stat, colorMode=background)

### 2.4.2 — ClickHouse Grafana Plugin

- [x] `GF_INSTALL_PLUGINS: "grafana-clickhouse-datasource"` im Grafana-Service
- [x] `deploy/grafana/provisioning/datasources/clickhouse.yml` (uid=clickhouse, port=9000)
- [x] `deploy/grafana/provisioning/dashboards/bgp_clickhouse.json`:
  - Top 10 Origin-AS (barchart)
  - Flapping Prefixe (table)
  - BGP Events Timeline (timeseries)
  - ANNOUNCE vs WITHDRAW (piechart)

---

## Abhängigkeiten & Reihenfolge

```
2.1 ClickHouse Setup & Exporter ✅
    └── 2.1.4 Neue API-Endpunkte ✅
        └── 2.2 Anomalie-Erkennung ✅
            │   (braucht historische Daten aus ClickHouse als Baseline)
            └── 2.3 RPKI-Validierung ✅
                └── 2.4 Dashboard-Erweiterung ✅
```

---

## Offene Punkte (Next-Sprint)

| Punkt | Beschreibung | Priorität |
|-------|-------------|-----------|
| BGP Leak Detection | AS-Typ via CAIDA AS-Rank API | Mittel |
| AS-Path-Längen-Anomalie | Rolling Median über 1000 Events | Niedrig |
| RPKI im ClickHouse-Schema | `rpki_status` Spalte in `bgp_events` | Mittel |

---

## Was sich NICHT geändert hat

- `bgp-stream` bleibt unverändert
- Gateway-Cluster (4× Instanzen + HAProxy) bleibt identisch
- Bestehende API-Endpunkte bleiben kompatibel (keine Breaking Changes)
- GitHub Actions CI/CD läuft weiter automatisch
- NDJSON-Sink bleibt als Backup/Fallback erhalten

---

## Aufwand-Zusammenfassung

| Phase | Aufwand | Status |
|-------|---------|--------|
| 2.1 ClickHouse | 4–6 Wochen | ✅ Abgeschlossen |
| 2.2 Anomalie-Erkennung | 3–4 Wochen | ✅ Abgeschlossen (2 Regeln offen) |
| 2.3 RPKI | 2–3 Wochen | ✅ Abgeschlossen |
| 2.4 Dashboards | 1–2 Wochen | ✅ Abgeschlossen |

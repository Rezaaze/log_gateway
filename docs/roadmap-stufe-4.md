# Roadmap — Stufe 4: Multi-Feed & AS-Relationship-Graph

> **Status: 🟠 LANGFRISTIG**
> Geschätzter Aufwand: 16–24 Wochen solo / 10–14 Wochen im Team
> Voraussetzung: Stufe 3 abgeschlossen ✅

---

## Ziel

Von einer Single-Source BGP-Plattform (RIPE RIS) zu einer vollständigen
BGP-Intelligence-Plattform mit mehreren Datenquellen, globalem AS-Relationship-Graph
und einem öffentlich nutzbaren API-Produkt.

---

## 4.1 — Multi-Feed-Ingestion

**Zeitaufwand:** 4–6 Wochen
**Impact:** Kritisch — blinde Flecken eliminieren

### Problem

RIPE RIS sieht nur ~900 Peer-Router. Regionale BGP-Events in Nordamerika,
Asien oder Afrika können komplett unbemerkt bleiben.

### 4.1.1 — RouteViews-Integration

- [ ] Zweiter BGP-Stream-Service: `tools/routeviews_stream/`
  - Architektur: identisch zu `bgp-stream` (Rust, WebSocket/MRT-Parser)
  - Datenquelle: RouteViews MRT-Dumps via HTTP-Polling alle 15 Min
  - MRT-Parser: `bgpkit-parser` crate
- [ ] Deduplizierung: gleicher Prefix + AS + Timestamp → einfach ignorieren
- [ ] Source-Tagging: `source = "routeviews"` vs. `source = "ris-live"`
- [ ] Coverage-Map in Grafana: welche Regionen aus welchem Feed sichtbar

### 4.1.2 — Direktes Peering (langfristig)

- [ ] BGP-Session direkt mit großen IXPs (DE-CIX, AMS-IX)
  - Erfordert: AS-Nummer, IP-Transit-Vereinbarung
  - Technisch: `bgpd` (FRRouting) als Sidecar → Rust-Parser
- [ ] Kostenabschätzung IXP-Port: ~500–2.000 EUR/Monat
- [ ] Nur sinnvoll ab kommerziellem Betrieb

### 4.1.3 — Streaming-Normalisierung

- [ ] Einheitliches internes Event-Format unabhängig von der Quelle:
  ```rust
  pub struct BgpEvent {
      pub id:          Uuid,
      pub timestamp:   DateTime<Utc>,
      pub source:      FeedSource,    // RisLive / RouteViews / DirectPeer
      pub event_type:  EventType,     // Announce / Withdraw
      pub prefix:      IpNetwork,
      pub origin_as:   u32,
      pub as_path:     Vec<u32>,
      pub peer_asn:    u32,
      pub peer_ip:     IpAddr,
      pub communities: Vec<Community>,
      pub rpki_status: RpkiStatus,
  }
  ```
- [ ] IP-Typ-Unterstützung: IPv4 + IPv6 gleichwertig

---

## 4.2 — AS-Relationship-Graph

**Zeitaufwand:** 4–6 Wochen
**Impact:** Sehr hoch — Grundlage für präzise Leak- und Hijack-Erkennung

### Problem

Ohne Wissen über AS-Beziehungen (Provider/Customer/Peer) ist es unmöglich,
BGP Leaks zuverlässig zu erkennen.

### 4.2.1 — CAIDA AS-Rank Integration

- [ ] Täglicher Download: `http://data.caida.org/datasets/as-relationships/`
- [ ] Datenformat: `as1 as2 relationship` (0=peer, -1=provider-customer)
- [ ] Import in ClickHouse:
  ```sql
  CREATE TABLE as_relationships (
      date       Date,
      as1        UInt32,
      as2        UInt32,
      relation   Int8,   -- 0: peer, -1: provider->customer, 1: customer->provider
      source     LowCardinality(String)
  ) ENGINE = ReplacingMergeTree(date)
  ORDER BY (as1, as2);
  ```
- [ ] Täglicher Refresh-Job (Tokio-Cron-Task)

### 4.2.2 — Graph-Datenbank (optional)

Für komplexe Graph-Traversal-Queries (z.B. "finde alle Kunden-AS von AS 3356"):

- [ ] Neo4j oder ArangoDB als optionaler Service
- [ ] Alternativ: ClickHouse-eigene Graph-Queries (array join tricks)
- [ ] Entscheidung nach Performance-Benchmark

### 4.2.3 — Valley-Free Routing Validation

BGP-Routen müssen dem "Valley-Free"-Prinzip folgen:
`Customer-Pfade → Provider → Provider → Customer-Pfade`

- [ ] Validator-Funktion: `fn is_valley_free(as_path: &[u32], relationships: &AsRelationshipMap) -> bool`
- [ ] Valley-Free-Verletzung → automatisch BGP-Leak-Alert

### 4.2.4 — AS-Kontext in Alerts

- [ ] AS-Name und Beschreibung via WHOIS/RDAP einbinden
- [ ] Geografische Lokation des AS (via CAIDA AS-Rank + MaxMind GeoIP)
- [ ] Alert-Nachricht mit Kontext:
  ```
  🚨 Possible BGP Hijack
  Prefix:    1.2.3.0/24 (Telekom Deutschland)
  Hijacker:  AS99999 (Unknown, DE) — niemals zuvor gesehen
  Legitimate: AS13184 (Telekom, Frankfurt)
  RPKI:      INVALID (ROA exists for AS13184)
  Confidence: 0.97
  ```

---

## 4.3 — Historische Analyse & Reporting

**Zeitaufwand:** 3–4 Wochen

### 4.3.1 — BGP-Event-Replay

- [ ] API-Endpunkt zum Replay historischer BGP-Events:
  ```
  POST /api/v1/bgp/replay
  Body: { "from": "2024-01-01", "to": "2024-01-02", "prefix": "1.2.3.0/24" }
  ```
- [ ] Streaming-Response (Server-Sent Events / WebSocket)
- [ ] Anwendungsfall: forensische Analyse nach Incidents

### 4.3.2 — Automatische Reports

- [ ] Wöchentlicher BGP-Stabilitäts-Report (PDF):
  - Top 10 instabilste Prefixe (Flapping)
  - Neue Prefixe pro AS
  - RPKI-Adoption-Rate (% Valid / Invalid / NotFound)
  - Anomalie-Zusammenfassung
- [ ] Report-Generator in Rust (latex oder typst für PDF, oder HTML→PDF)
- [ ] Versand per E-Mail + Ablage in MinIO

### 4.3.3 — Incident-Tracking

- [ ] Incidents aus Alert-Gruppen zusammenfassen
- [ ] Incident-Timeline: wann begann Hijack, wie lange dauerte er
- [ ] Automatisches Post-Mortem-Dokument (Markdown-Template)

---

## 4.4 — Öffentliche API & Monetarisierung

**Zeitaufwand:** 3–4 Wochen
**Voraussetzung:** Alle vorherigen Stufen stabil

### 4.4.1 — API-Tier-Modell

| Tier | Preis | Features |
|------|-------|---------|
| Free | 0 EUR | 1.000 Queries/Tag, 7 Tage History, kein Alert |
| Pro | 99 EUR/Monat | 100.000 Queries/Tag, 90 Tage History, 10 Alert-Rules |
| Enterprise | Auf Anfrage | Unbegrenzt, 2 Jahre History, Custom Feeds, SLA |

### 4.4.2 — Developer Portal

- [ ] Öffentliche API-Dokumentation (OpenAPI + Swagger UI — bereits vorhanden)
- [ ] API-Key-Self-Service-Portal (einfache Web-UI)
- [ ] Playground: interaktive API-Abfragen im Browser
- [ ] SDK-Generierung: Python + JavaScript (openapi-generator)

### 4.4.3 — Billing-Integration

- [ ] Usage-Tracking (bereits: `cost_tracker.rs`)
- [ ] Stripe-Integration für Zahlungsabwicklung
- [ ] Automatische Rechnungstellung

---

## 4.5 — Infrastruktur-Skalierung

**Zeitaufwand:** 2–3 Wochen (parallel zu anderen Phasen)

### 4.5.1 — Horizontale Skalierung

Aktuell: 1 Server, 4 Gateway-Instanzen, 1 ClickHouse-Node.
Ziel: Multi-Node-Setup.

- [ ] ClickHouse-Cluster (3 Nodes, Sharding nach `origin_as`)
- [ ] HAProxy auf separatem Server (kein SPOF)
- [ ] Hetzner Load Balancer (managed, kein Self-Hosted HAProxy)
- [ ] Kubernetes-Migration (optional, wenn >10 Nodes)

### 4.5.2 — Disaster Recovery

- [ ] ClickHouse-Backup täglich nach S3 (MinIO oder Hetzner Object Storage)
- [ ] RTO (Recovery Time Objective): < 1 Stunde
- [ ] RPO (Recovery Point Objective): < 15 Minuten
- [ ] Automatisierter Backup-Test monatlich (Restore-Test)

### 4.5.3 — CDN & Geo-Distribution

- [ ] API-Endpunkte über Cloudflare Workers (Edge Caching für Read-Queries)
- [ ] BGP-Stream-Ingestion in zwei Regionen (EU + US) für Latenz-Reduktion

---

## Aufwand-Zusammenfassung

| Phase | Aufwand | Risiko | Priorität |
|-------|---------|--------|-----------|
| 4.1 Multi-Feed | 4–6 Wochen | Mittel | **Erste** |
| 4.2 AS-Graph | 4–6 Wochen | Mittel | **Zweite** |
| 4.3 Historische Analyse | 3–4 Wochen | Gering | **Dritte** |
| 4.4 Öffentliche API | 3–4 Wochen | Gering | **Vierte** |
| 4.5 Infrastruktur | 2–3 Wochen | Mittel | **Parallel** |
| **Gesamt** | **16–23 Wochen** | | |

---

## Gesamtüberblick aller Stufen

| Stufe | Status | Schwerpunkt | Aufwand |
|-------|--------|-------------|---------|
| 1 | ✅ Fertig | Ingestion & Storage | — |
| 2 | 🔵 Geplant | Query-Engine & Anomalie-Erkennung | 10–15 Wochen |
| 3 | 🟡 Visionär | Alert-Engine & ML | 12–17 Wochen |
| 4 | 🟠 Langfristig | Multi-Feed & Produkt | 16–23 Wochen |

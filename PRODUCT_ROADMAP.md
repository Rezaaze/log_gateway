# PrefixGuard — BGP Hijack Detection Engine
## Produktvision & Roadmap

**Stand:** 07.03.2026
**Ziel:** Das schnellste, präziseste BGP-Prefix-Monitoring der Welt — als API-Produkt.

---

## 1. Das Problem

BGP-Hijacking bedeutet: Jemand kündigt deine IP-Adressen im globalen Internet an.
Dein Traffic fließt durch den Angreifer. Du merkst es nicht.

```
Normal:    Nutzer → dein AS → dein Server
Hijack:    Nutzer → Angreifer-AS → (dein Server, oder nirgendwo)
```

**Bekannte Fälle:**
- 2018: Amazon Route 53 gekapert → $150.000 Krypto gestohlen in 2h
- 2021: Facebook offline 6h durch BGP-Fehlkonfiguration (Selbst-Hijack)
- 2022: Russische Telekom leitet US-Militär-Routen um (18 Minuten unbemerkt)
- 2023: Türk Telekom leitet globalen Traffic durch sich selbst

**Warum Firmen kein Schutz haben:**
- BGP war nie für Sicherheit designed — Vertrauen als Protokoll-Grundlage
- RPKI existiert (globale Signatur-DB), aber kaum jemand monitort aktiv dagegen
- Bestehende Tools prüfen alle 5–15 Minuten, nicht pro Event
- Kein Produkt kombiniert RPKI + IRR + Echtzeit + API

---

## 2. Das Produkt

**PrefixGuard** — Real-time BGP Prefix Security API

```
Du gibst an:  deine ASN + deine Prefixe
Du bekommst: Webhook/Alert wenn jemand deine Routen stiehlt
             API-Abfrage ob ein Prefix gerade safe ist
             Dashboard: Deine Prefixe — grün oder rot
```

**Technischer Kern:** Jeder BGP-Event (~27.500/sec global) wird gegen
825.000 RPKI-Einträge validiert — in unter 336 Nanosekunden, in-memory.

---

## 3. Was bereits gebaut ist

> Ehrlicher Stand: ~80% der Infrastruktur existiert. Die Orchestrierung fehlt.

### ✅ Fertig und produktionsreif

| Datei | Was es kann | Performance |
|---|---|---|
| `src/rpki_cache.rs` | 825k VRPs in HashMap, validate() synchron, Exponential Backoff Refresh | 336 ns/Lookup |
| `src/irr_cache.rs` | RIPE Whois Konsistenz-Check, 24h TTL Cache | async, 50k Entries |
| `src/anomaly_detector.rs` (HijackDetector) | Neue AS für Prefix → Hijack, enriched mit RPKI+IRR → Confidence 0.30–0.99 | synchron |
| `src/anomaly_detector.rs` (FlappingDetector) | 50 Events + 6 Richtungswechsel / 5min → Flapping | synchron |
| `src/alert_dedup.rs` | SHA-256 Fingerprint, 30min TTL, verhindert Alert-Flood | <1µs |
| `src/roa_poller.rs` | ROA-Änderungen erkennen (added/removed), 5min Polling | produktiv |
| `src/webhook.rs` | Slack Block Kit + Generic HTTP Webhooks, Level-basiert | produktiv |
| `tools/bgp_stream/` | RIPE NCC RIS Live → NATS, 27.500 Events/sec, Batching | produktiv |

### ⚠️ Vorhanden aber fehlerhaft / tot

| Datei | Problem |
|---|---|
| `src/model_trainer.rs` | Liest aus `bgp_events` (ClickHouse) — die Tabelle wird nicht mehr beschrieben |
| `src/bgp_query.rs` | 4 API-Endpoints lesen aus toten Daten (letzter Event: 04.03.2026) |
| `src/clickhouse_exporter.rs` | BGP-Events wurden hier geschrieben — jetzt übernimmt NATS |

### ❌ Fehlt komplett (das größte Loch)

```
src/detector_loop.rs — existiert nicht
```

Kein Code der NATS-Consumer → RPKI → IRR → HijackDetector → FlappingDetector
→ Dedup → Webhook zusammenhält. Die Komponenten sind isolierte Inseln.

---

## 4. Roadmap

### Phase 1 — Kern fertigstellen
**Ziel:** Alles was gebaut wurde, läuft end-to-end. Kein totes Code.
**Dauer:** 2–3 Wochen

#### 1.1 — Orchestrierungs-Loop bauen (`src/detector_loop.rs`)

Das ist der wichtigste fehlende Teil. Pseudocode:

```
NATS Consumer (bgp.events, queue group "detectors")
  └─ für jeden BgpEvent:
       1. rpki_cache.validate(prefix, origin_as)           → RpkiStatus
       2. irr_cache.check(prefix, origin_as)               → IrrStatus (async)
       3. hijack_detector.check_with_rpki_status(event, rpki, irr)  → Option<Anomaly>
       4. flapping_detector.check(event)                   → Option<Anomaly>
       5. für jede Anomaly:
            a. fingerprint = compute_fingerprint(type, prefix, as)
            b. if dedup_cache.is_new(fingerprint):
                 webhook_sender.send(build_payload(anomaly))
```

#### 1.2 — Model-Trainer reparieren

`model_trainer.rs` liest aus `bgp.bgp_events` (ClickHouse, tot).
Fix: Baseline-Snapshots als JSON-Datei speichern/laden.

```rust
// Alt: ClickHouse SELECT ... FROM bgp_events
// Neu: Datei /data/snapshots/baseline_{date}.json
// Format: { "prefix": "10.0.0.0/8", "known_asns": [64512, 65536] }
```

#### 1.3 — Nicht-BGP-Code auslagern

Der Log-Gateway (HTTP-Ingestion, PII, Rate-Limiting, Tenant-Costs, S3-Export)
gehört nicht zum Produkt. Entweder:
- **Option A:** Separates Crate (`log-gateway/` bleibt, `bgp-shield/` neu)
- **Option B:** Feature-Flags (`--features bgp-detection` vs `--features log-gateway`)

**Empfehlung:** Option A — saubere Trennung, eigenes Repo.

#### 1.4 — End-to-End Integration Test

```rust
// tests/e2e_detection_test.rs
// 1. Mock NATS mit bekanntem Hijack-Event
// 2. Mock RPKI-Cache mit InvalidAsn
// 3. Prüfe: Webhook wird innerhalb 100ms aufgerufen
// 4. Prüfe: Zweiter identischer Event → kein zweiter Webhook (Dedup)
```

**Deliverable Phase 1:** System läuft: RIS Live → Erkennung → Webhook. Vollständiger Test.

---

### Phase 2 — Produkt-API
**Ziel:** Kunden können sich anmelden, Prefixe registrieren, Alerts bekommen.
**Dauer:** 4–6 Wochen

#### 2.1 — Monitoring-Registrierung API

```
POST /v1/monitors
Body: { "asn": 64512, "prefixes": ["203.0.113.0/24", "10.0.0.0/8"] }
→ { "monitor_id": "mon_abc123", "api_key": "pg_live_..." }

GET /v1/monitors/{id}/status
→ { "prefixes": [{ "prefix": "203.0.113.0/24", "status": "safe", "last_checked": "..." }] }
```

#### 2.2 — Webhook-Verwaltung

```
POST /v1/monitors/{id}/webhooks
Body: { "url": "https://...", "type": "slack", "events": ["hijack", "flapping"] }

DELETE /v1/monitors/{id}/webhooks/{webhook_id}
```

#### 2.3 — Live-Status API (wichtigster Endpunkt)

```
GET /v1/check?prefix=203.0.113.0/24&asn=64512
→ {
    "rpki": "valid",
    "irr":  "consistent",
    "status": "safe",
    "latency_ms": 1
  }
```

Das ist der Endpunkt für den jemand €50/Monat zahlt.
Latenz: <5ms (RPKI im RAM, IRR gecacht).

#### 2.4 — Auth und Rate-Limiting

- API-Key Auth (Bearer Token)
- 1.000 Checks/Monat Free Tier
- 100.000 Checks/Monat Pro Tier
- Unlimited Enterprise

**Deliverable Phase 2:** Funktionierender SaaS-API, erster zahlender Kunde möglich.

---

### Phase 3 — Qualität & Präzision
**Ziel:** False-Positive-Rate unter 1%. Weitere Detektoren.
**Dauer:** 4–6 Wochen

#### 3.1 — Route-Leak-Detection

BGP Route Leak: AS kündigt Routen an die er nicht ankündigen sollte
(z.B. Transit-Route an anderen Provider weitergegeben).

```
Merkmal: AS-Path enthält unerwartete Transit-AS
Signat:  Prefix plötzlich über viel mehr AS-Hops erreichbar
```

#### 3.2 — AS-Path Anomalie

```rust
// Wenn AS-Path-Länge plötzlich > 2× historischer Durchschnitt
// → Könnte Routing-Manipulation sein
struct AsPathAnomalyDetector {
    path_lengths: DashMap<String, EmaModel>,  // prefix → EMA der Pfadlänge
}
```

#### 3.3 — Confidence-Kalibrierung

Backtesting gegen bekannte Hijack-Datenbanken:
- CAIDA BGP Hijacks Dataset
- RIPE Atlas Anomaly Reports
- Eigene gesammelte Events

Ziel: False-Positive-Rate messbar unter 1%.

#### 3.4 — Baseline-Persistierung

```
/data/snapshots/
  ├─ known_prefixes_{date}.json   # Welche ASN kündigt welches Prefix an
  ├─ baseline_{date}.json          # EMA-Modell pro Prefix
  └─ as_knowledge_{date}.json      # Wie lange kennen wir dieses AS
```

Bei Neustart: Snapshot laden, kein Kaltstart.

#### 3.5 — Multi-Region BGP-Sicht

RIPE RIS Live ist eine Perspektive. Für vollständiges Bild:
- Route Views (Oregon, Amsterdam, Sydney)
- RIPE RIS Live (Frankfurt) ← aktuell
- CAIDA BGPStream ← optional

Mehr Vantage Points = mehr Sicherheit gegen lokale Hijacks.

**Deliverable Phase 3:** <1% False-Positive, 3 Detektoren, persistentes Lernen.

---

### Phase 4 — Go-to-Market
**Ziel:** Zahlende Kunden, Self-Service Onboarding.
**Dauer:** 4–6 Wochen

#### 4.1 — Dashboard (minimal)

```
Anforderungen (kein Framework-Overkill):
- Login via Magic Link (kein Passwort)
- Liste der eigenen Prefixe mit Status (grün/rot)
- Alert-History (letzte 30 Tage)
- Webhook-Verwaltung
- API-Key anzeigen/rotieren
```

#### 4.2 — Onboarding-Flow

1. Registrierung mit E-Mail
2. ASN eingeben → System erkennt Prefixe automatisch via RIPE
3. Webhook-URL oder Slack eingeben
4. Test-Alert empfangen
5. Fertig

#### 4.3 — Dokumentation

- API-Referenz (OpenAPI / Redoc)
- Quickstart: "In 5 Minuten deine ersten Alerts"
- Erklärung: Was ist RPKI? Was bedeuten die Alerts?
- Case Studies: Amazon 2018, Facebook 2021

#### 4.4 — Pricing

| Tier | Preis | Limits |
|---|---|---|
| Free | €0 | 3 Prefixe, E-Mail Alerts, 24h Delay |
| Starter | €49/Monat | 20 Prefixe, Webhooks, Real-time |
| Pro | €199/Monat | 200 Prefixe, API-Zugang, SLA 99.9% |
| Enterprise | Auf Anfrage | Unbegrenzt, Dedicated, Custom Integration |

**Deliverable Phase 4:** Erster zahlender Kunde. Produkt öffentlich zugänglich.

---

## 5. Technische Zielarchitektur

```
Internet
    │
    ▼
RIPE NCC RIS Live WebSocket (wss://ris-live.ripe.net/v1/ws/)
    │  27.500 BGP Events/sec
    ▼
bgp-stream (Rust)
    │  Batched NATS Publish
    ▼
NATS JetStream (bgp.events)
    │  Queue Group: "detectors"
    ▼
detector_loop (Rust) ←── rpki_cache (825k VRPs im RAM, 336ns/Lookup)
    │                 ←── irr_cache (RIPE Whois, 24h TTL)
    │                 ←── hijack_detector (check_with_rpki_status)
    │                 ←── flapping_detector (50 events + 6 changes)
    │
    ▼
alert_dedup (SHA-256, 30min TTL)
    │
    ▼
webhook_sender → Slack / HTTP / PagerDuty / E-Mail
    │
    ▼
Prometheus (Metriken: events/sec, anomalies/sec, false-positive-rate)
    │
    ▼
Grafana Dashboard (intern: Systemgesundheit)
    │
    ▼
Kunden-Dashboard (extern: Prefix-Status, Alert-History)
```

---

## 6. Was dieses Produkt besser macht als alles andere

| Eigenschaft | BGPmon | RIPE Stat | Cloudflare Radar | **PrefixGuard** |
|---|---|---|---|---|
| Erkennungs-Latenz | 5–15 Min | manuell | ~1 Min | **<100ms** |
| RPKI-Validierung | ❌ | teilweise | ❌ | **✅ 825k VRPs** |
| IRR Cross-Check | ❌ | ❌ | ❌ | **✅ RIPE Whois** |
| API-Zugang | eingeschränkt | ❌ | eingeschränkt | **✅ vollständig** |
| Confidence Score | ❌ | ❌ | ❌ | **✅ 0.30–0.99** |
| Open Source Kern | ❌ | ❌ | ❌ | **✅ (Rust)** |
| Events/sec | ~100 | N/A | unbekannt | **27.500** |

---

## 7. Metriken für Erfolg

### Technisch
- [ ] End-to-End Latenz: BGP-Event → Webhook < 500ms (p99)
- [ ] False-Positive-Rate: < 1% (gemessen gegen Referenz-Datensatz)
- [ ] Verfügbarkeit: > 99.9% (52 Min Downtime/Monat)
- [ ] RPKI Coverage: 100% der globalen VRPs (825k+)
- [ ] Durchsatz: > 50.000 Events/sec ohne Lastabwurf

### Produkt
- [ ] Onboarding-Zeit: < 5 Minuten bis erster Alert
- [ ] API-Latenz: < 5ms für `/v1/check` (p95)
- [ ] 10 zahlende Kunden bis Ende Phase 4
- [ ] 1 Enterprise-Kunde (ISP oder Bank)

---

## 8. Sofort-Maßnahmen (diese Woche)

Reihenfolge nach Impact:

1. **`src/detector_loop.rs` bauen** — verbindet alle Komponenten
   Geschätzter Aufwand: 3–5 Tage
   Blockiert: alles andere in Phase 1

2. **`src/model_trainer.rs` reparieren** — JSON statt ClickHouse
   Geschätzter Aufwand: 1–2 Tage
   Blockiert: Kaltstart-Problem (Detector lernt nicht persistent)

3. **End-to-End Test schreiben** — bevor Kunden draufschauen
   Geschätzter Aufwand: 1–2 Tage

4. **`bgp_events` ClickHouse-Tabelle droppen** — 2 GiB frei, kein Risiko
   Geschätzter Aufwand: 5 Minuten

5. **BGP-Kern in eigenes Crate / Repo** — klare Produktgrenze
   Geschätzter Aufwand: 1 Tag

---

*Dokument gepflegt in: `/log-gateway/PRODUCT_ROADMAP.md`*
*Nächste Überprüfung: nach Abschluss Phase 1*

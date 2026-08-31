# log-gateway — Entwicklungs-Roadmap

> ⚠️ **VERWORFEN (Stand 31.08.2026):** Dieser Entwurf ("kein persistenter
> Storage, kein ClickHouse, rein stateless") widerspricht dem, was tatsächlich
> gebaut wurde — ClickHouse-Export/Query-Layer, Tenant/Quota/Cost-Billing und
> die Alert-Escalation-Pipeline sind fest im System verankert. Alle 21
> Checkboxen in diesem Dokument sind unerledigt geblieben; es gibt keinen
> Hinweis, dass seit 2026-03-07 daran weitergearbeitet wurde. Maßgeblich ist
> **`TRUSTWAVE_ROADMAP.md`**. Dieses Dokument bleibt nur als historischer
> Alternativentwurf erhalten.

---

# log-gateway — Entwicklungs-Roadmap (verworfener Entwurf)
> Erstellungsdatum: 2026-03-07
> Basis: NATS JetStream Streaming-Architektur (kein persistenter Storage)
> Prinzip: Stateless Detection, kryptografische Wahrheit, 0% False Positives

---

## Architektur-Ziel (End State)

```
RIPE RIS Live (WebSocket)
        │
        ▼
  bgp-stream (Rust)
        │  ~12.000 events/sec
        ▼
  NATS JetStream "BGP_EVENTS"
        │  WorkQueue, 500MB, 24h
        ┌─────────┴──────────┐
        ▼                    ▼
  gateway_1-4           BGPalerter
  (RPKI-First           (Referenz-
   Detector)             Validator)
        │                    │
        └─────────┬──────────┘
                  │  Nur wenn BEIDE übereinstimmen
                  ▼
          Grafana OnCall
          (Incident Routing)
                  │
                  ▼
          bgp.tools API
          (Alert Context, kein Storage)
```

---

## Phase 1 — RPKI-First Detection Fix
**Dauer:** 1–2 Tage
**Priorität:** 🔴 Kritisch
**Ziel:** False Positives von ~1.000.000/Tag auf <200/Tag reduzieren

### Hintergrund
Der aktuelle Detector meldet Hijacks auch wenn RPKI nicht verfügbar ist (503/429).
Ohne verlässliche RPKI-Daten ist jede Entscheidung eine Vermutung.

### Tasks

#### 1.1 Routinator VRP-Dump Integration (2h)
Statt HTTP-Request pro Event → komplette ROA-Tabelle alle 10min laden und in-memory validieren.

**Änderung:** `src/rpki_cache.rs`
```rust
// Neu: VRP-Tabelle alle 10min vom Routinator-Dump laden
GET http://routinator:8323/api/v1/vrps
→ Vec<Vrp> { prefix, max_length, asn }
→ in Arc<RwLock<VrpTable>> speichern

// Validierung: O(log n) Lookup, kein HTTP overhead, kein 503
fn validate(prefix: &str, asn: u32) -> RpkiStatus {
    // local lookup gegen VrpTable
}
```

**Warum:** Eliminiert alle 503-Fehler beim Start und alle Latenz-Spikes.
Routinator baut die Tabelle im Hintergrund auf — erst wenn sie fertig ist wird sie aktiviert.

**Erfolgskriterium:** Keine 503/429 Errors in gateway logs. RPKI-Validierung <1µs pro Event.

---

#### 1.2 Detection Logic Fix (2h)
**Änderung:** `src/detector_runner.rs`

```
Aktuell:
    RPKI 503 → trotzdem Hijack melden (85% confidence) ← FALSCH

Neu:
    RPKI Valid        → kein Alert (return None)
    RPKI Invalid      → Alert (95% confidence) ← echter Hijack
    RPKI Unknown      → kein Alert (kein ROA = kein Beweis)
    RPKI Fehler       → kein Alert (skip, kein Raten)
```

**Erfolgskriterium:** `detector_anomalies_detected_total_total` sinkt von
Millionen auf <200/Tag. Nur RPKI Invalid Events triggern Alerts.

---

#### 1.3 Metrik-Namen-Fix (30min)
`_total_total` Doppelung bereinigen.

**Änderung:** `src/metrics_exporter.rs`
```rust
// Statt:
Counter::new("detector_events_processed_total", ...)
// → Prometheus: detector_events_processed_total_total ← falsch

// Richtig:
Counter::new("detector_events_processed", ...)
// → Prometheus: detector_events_processed_total ← korrekt
```

**Erfolgskriterium:** Alle Metriken in Prometheus haben `_total` genau einmal.

---

#### Phase 1 Abschluss-Checkliste
- [ ] Routinator VRP-Dump lädt alle 10min ohne Fehler
- [ ] Keine 503/429 in Gateway-Logs
- [ ] Alerts/Tag < 200
- [ ] False Positive Rate < 5%
- [ ] Metrik-Namen korrekt in Grafana
- [ ] Alle Tests grün, Push zu main

---

## Phase 2 — IRRd Local Mirror
**Dauer:** 1 Tag
**Priorität:** 🔴 Hoch
**Ziel:** IRR-Validierung ohne Rate Limiting, <1ms Response Time

### Hintergrund
RIPE WHOIS API limitiert auf ~100 req/min → 429 bei 12.000 events/sec.
IRRd spiegelt alle IRR-Datenbanken (RIPE, ARIN, APNIC, LACNIC, RADB) lokal.
Keine Rate Limits, Whois-Protokoll auf Port 43, sub-Millisekunde.

### Tasks

#### 2.1 IRRd Docker Service einrichten (2h)
**Änderung:** `docker-compose.prod.yml` + `deploy/irrd/irrd.yaml`

```yaml
irrd:
  image: ghcr.io/irrdnet/irrd:latest
  container_name: irrd
  volumes:
    - ./deploy/irrd/irrd.yaml:/etc/irrd.yaml:ro
    - irrd-data:/var/lib/irrd
  networks:
    - gateway-net
  restart: unless-stopped
  healthcheck:
    test: ["CMD-SHELL", "echo '' | nc -w2 localhost 43 || exit 1"]
    interval: 30s
    timeout: 5s
    retries: 3
    start_period: 300s  # initialer DB-Download ~5min
```

**IRRd Konfiguration** `deploy/irrd/irrd.yaml`:
```yaml
sources:
  RIPE:
    import_source: "ftp://ftp.ripe.net/ripe/dbase/ripe.db.gz"
  ARIN:
    import_source: "ftp://ftp.arin.net/pub/rps/arin.db"
  APNIC:
    import_source: "ftp://ftp.apnic.net/pub/apnic/whois/apnic.db.gz"
```

---

#### 2.2 IRR Cache auf Whois umstellen (2h)
**Änderung:** `src/irr_cache.rs`

```rust
// Statt: HTTPS REST API → 429
// Neu: Whois TCP Port 43 → unlimitiert, lokal

async fn query_irrd(prefix: &str, asn: u32) -> IrrStatus {
    let mut stream = TcpStream::connect("irrd:43").await?;
    stream.write_all(format!("!r{},{}\n", prefix, asn).as_bytes()).await?;
    // parse response
}
```

**Erfolgskriterium:** Keine 429 IRR-Errors in Logs. IRR-Lookup <2ms.

---

#### 2.3 Deploy-Skript anpassen (30min)
IRRd in CI/CD Deploy aufnehmen. Initialer Download (~500MB) dauert ~5min —
healthcheck `start_period: 300s` verhindert Restart-Loop.

---

#### Phase 2 Abschluss-Checkliste
- [ ] IRRd Container läuft und ist healthy
- [ ] RIPE, ARIN, APNIC Datenbanken gespiegelt
- [ ] Keine 429 IRR-Errors in Gateway-Logs
- [ ] IRR-Lookup < 2ms

---

## Phase 3 — Grafana OnCall Integration
**Dauer:** 1 Tag
**Priorität:** 🔴 Hoch
**Ziel:** Alerts erreichen tatsächlich Menschen zur richtigen Zeit

### Hintergrund
Alertmanager feuert Alerts — aber wer sieht sie? Ohne On-Call Routing
gehen Alerts nachts ins Leere. Grafana OnCall ist kostenlos und bereits
in die vorhandene Grafana-Instanz integrierbar.

### Tasks

#### 3.1 Grafana OnCall Container (1h)
**Änderung:** `docker-compose.prod.yml`

```yaml
oncall:
  image: grafana/oncall:latest
  container_name: oncall
  environment:
    - GRAFANA_API_URL=http://grafana:3000
    - SECRET_KEY=${ONCALL_SECRET_KEY}
    - DATABASE_TYPE=sqlite3
  volumes:
    - oncall-data:/var/lib/oncall
  networks:
    - gateway-net
  depends_on:
    - grafana
```

---

#### 3.2 Alert Routing Konfiguration (2h)

**Eskalationspfad:**
```
Alert feuert
    │
    ├─ Severity: critical → sofort Telegram + SMS
    │
    ├─ Severity: warning  → Telegram, 10min kein Ack → SMS
    │
    └─ Severity: info     → nur Grafana Dashboard
```

**Alertmanager Integration** `deploy/alertmanager/alertmanager.yml`:
```yaml
receivers:
  - name: oncall
    webhook_configs:
      - url: 'http://oncall:8080/integrations/v1/alertmanager/'

route:
  group_by: ['alertname', 'prefix']
  group_wait: 30s
  group_interval: 5m
  repeat_interval: 4h    # kein Spam
  receiver: oncall
```

---

#### 3.3 Alert Grouping (1h)
1000 einzelne Hijack-Alerts → 1 Incident "BGP Hijack Campaign"

```yaml
# Alertmanager grouping
group_by: ['alertname', 'origin_as']
group_wait: 60s     # sammle 60s Events vom selben AS
```

**Erfolgskriterium:** Pro Hijack-Kampagne ein Incident, nicht 10.000 einzelne Alerts.

---

#### Phase 3 Abschluss-Checkliste
- [ ] Grafana OnCall läuft und ist mit Grafana verbunden
- [ ] Telegram Webhook konfiguriert und getestet
- [ ] Test-Alert kommt innerhalb 60s an
- [ ] Alert Grouping reduziert Noise

---

## Phase 4 — bgp.tools Context API
**Dauer:** 4 Stunden
**Priorität:** 🟡 Mittel
**Ziel:** Jeden Alert mit Kontext anreichern ohne eigene Datenhaltung

### Hintergrund
Ein Alert "AS64512 hijacked 192.0.2.0/24" ist wenig hilfreich.
bgp.tools liefert sofort: AS-Name, Land, seit wann aktiv, RPKI-Status,
bekannte Prefixes — alles ohne eigenen Storage.

### Tasks

#### 4.1 bgp.tools Client (2h)
**Neue Datei:** `src/bgp_context.rs`

```rust
// Nur für bestätigte Alerts aufrufen, nicht für jeden Event
pub async fn enrich_alert(prefix: &str, asn: u32) -> Option<BgpContext> {
    let url = format!("https://bgp.tools/prefix/{}/json", prefix);
    let resp: BgpContext = client.get(&url).send().await?.json().await?;
    Some(resp)
}

pub struct BgpContext {
    pub as_name: String,       // "ACME-NETWORKS"
    pub country: String,       // "DE"
    pub rpki_status: String,   // "invalid"
    pub irr_routes: Vec<String>,
    pub origins: Vec<u32>,     // bekannte Origin-ASes
}
```

---

#### 4.2 Alert-Text anreichern (1h)
Statt:
```
ALERT: BGP Hijack detected
prefix=192.0.2.0/24 origin_as=64512
```

Nach Enrichment:
```
🚨 BGP Hijack — RPKI Invalid
Prefix:    192.0.2.0/24
Angreifer: AS64512 (UNKNOWN-ASN, RU)
Erwartet:  AS13335 (CLOUDFLARENET, US)
RPKI:      Invalid — ROA vorhanden für AS13335
IRR:       Kein Route Object für AS64512
bgp.tools: https://bgp.tools/prefix/192.0.2.0/24
```

---

#### Phase 4 Abschluss-Checkliste
- [ ] bgp.tools API antwortet (kein API-Key nötig)
- [ ] Alerts enthalten AS-Name, Land, RPKI-Status
- [ ] Rate Limiting: max 1 bgp.tools Request pro Alert (nicht pro Event)

---

## Phase 5 — BGPalerter als Referenz-Validator
**Dauer:** 1–2 Tage
**Priorität:** 🟡 Mittel
**Ziel:** Detection-Qualität durch unabhängige Validierung erhöhen

### Hintergrund
BGPalerter (NTT) ist eine battle-tested Detection Engine mit ausgereiften
Algorithmen für Hijack, Route Leak, Prefix Visibility. Wenn dein Detector
UND BGPalerter dasselbe melden → sehr hohe Wahrscheinlichkeit echter Hijack.

### Tasks

#### 5.1 BGPalerter Container (2h)
**Änderung:** `docker-compose.prod.yml`

```yaml
bgpalerter:
  image: nttgin/bgpalerter:latest
  container_name: bgpalerter
  volumes:
    - ./deploy/bgpalerter:/config
  networks:
    - gateway-net
```

**Konfiguration** `deploy/bgpalerter/prefixes.yml`:
```yaml
# Zu überwachende Prefixes (eigene + kritische Infrastruktur)
192.0.2.0/24:
  description: "Test Prefix"
  asn: [64512]
  ignoreMoreSpecifics: false
```

---

#### 5.2 Dual-Validation Logic (3h)
**Neue Datei:** `src/dual_validator.rs`

```
Regel:
    Dein Detector meldet Hijack → speichere in NATS KV (TTL: 60s)
    BGPalerter meldet Hijack    → prüfe ob in NATS KV vorhanden

    BEIDE haben gemeldet → HIGH confidence Alert (eskalieren)
    Nur einer hat gemeldet → MEDIUM (loggen, kein Incident)
```

**NATS KV als Shared State** (kein externer Storage nötig):
```rust
let kv = js.create_key_value(Config {
    bucket: "hijack-candidates".to_string(),
    ttl: Duration::from_secs(60),
    ..Default::default()
}).await?;

// Detector: put candidate
kv.put(&prefix_key, b"detected").await?;

// BGPalerter webhook handler: check
if kv.get(&prefix_key).await?.is_some() {
    // Dual confirmation → echter Alert
}
```

---

#### Phase 5 Abschluss-Checkliste
- [ ] BGPalerter läuft und verbindet sich mit RIPE RIS
- [ ] Webhook von BGPalerter → Gateway erreichbar
- [ ] Dual-Validation funktioniert (NATS KV TTL-basiert)
- [ ] Test-Hijack löst Dual-Alert aus

---

## Übersicht & Zeitplan

```
Woche 1          Woche 2          Woche 3
────────────     ────────────     ────────────
Phase 1          Phase 2          Phase 4
RPKI-Fix         IRRd local       bgp.tools
(1-2 Tage)       (1 Tag)          (4h)

                 Phase 3          Phase 5
                 OnCall           BGPalerter
                 (1 Tag)          (1-2 Tage)
```

---

## Erwartete Verbesserungen nach vollständiger Implementierung

| Metrik                   | Jetzt         | Nach Phase 1 | Nach Phase 1-5 |
|--------------------------|---------------|--------------|----------------|
| False Positives/Tag      | ~1.000.000    | <200         | <20            |
| False Positive Rate      | ~99,9%        | <5%          | <1%            |
| Erkennungsrate (RPKI)    | unklar        | ~80%         | ~90%           |
| IRR Response Zeit        | 200ms + 429   | <2ms         | <2ms           |
| RPKI Response Zeit       | 50ms + 503    | <0,1ms       | <0,1ms         |
| Alert Routing            | niemand sieht | Telegram     | OnCall+Eskala  |
| Alert Kontext            | nur Prefix/AS | +bgp.tools   | vollständig    |
| Detection Confidence     | schwach       | hoch (RPKI)  | sehr hoch      |

---

## Technische Constraints (unveränderlich)

- ✅ **Kein persistenter Storage** — kein ClickHouse, kein PostgreSQL
- ✅ **NATS JetStream** als einzige Daten-Schicht (WorkQueue, 500MB, 24h TTL)
- ✅ **NATS KV** für flüchtigen Shared State zwischen Gateways (TTL-basiert)
- ✅ **Stateless Detection** — jeder Event wird isoliert bewertet
- ✅ **Kryptografische Wahrheit** — RPKI Invalid ist Ground Truth, keine Heuristik
- ✅ **ARM64 native** — alle Images müssen für aarch64 verfügbar sein

---

## Nicht in dieser Roadmap (bewusst ausgelassen)

| Feature | Grund |
|---|---|
| ML-basierte Detection | Kein labeled Dataset, Bootstrapping-Problem |
| Historical Baseline (30-Tage) | Widerspricht No-Storage Architektur |
| Eigene RPKI CA (Krill) | Overkill, Routinator reicht |
| ClickHouse Re-Integration | Storage-Problem bleibt, kein Mehrwert |
| BGP Full Table (eigener Router) | Zu komplex, RIPE RIS reicht für Detection |

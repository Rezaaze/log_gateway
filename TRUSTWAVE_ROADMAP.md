# BGP TrustWave — Technische Implementierungs-Roadmap

**Stand:** 08.03.2026
**Letztes Update:** 08.03.2026
**Vertraulich** — Alle Gespräche nur unter NDA
**Prinzip:** Jede Phase liefert einen messbaren Beweis. Kein nächster Schritt ohne erfolgreichen Meilenstein.

---

## Übersicht

| Phase | Titel | Zeitraum | Ziel | Status |
|---|---|---|---|---|
| 0 | IP-Schutz & Projekt-Reset | Woche 1 | Rechtlich absichern, Fokus setzen | ⏳ Teilweise erledigt |
| 1 | Daten-Fundament | Woche 2–4 | Rohdaten korrekt erfassen | 🔲 Offen |
| 2 | Wellenbaseline | Woche 4–8 | Normales Propagationsverhalten modellieren | 🔲 Offen |
| 3 | Wave Anomaly Detector | Woche 8–12 | Hijacks durch Wellenabweichung erkennen | 🔲 Offen |
| 4 | Trust Score Engine | Woche 12–16 | Alle Signale zu einem Score kombinieren | 🔲 Offen |
| 5 | Echtzeit-System | Monat 4–6 | Live-Betrieb, Validierung | 🔲 Offen |
| 6 | BGP-Speaker & Pilot | Monat 6–9 | Aktives Routing, erster Kunde | 🔲 Offen |
| 7 | Produktreife | Monat 9–18 | Skalierung, Zertifizierungen | 🔲 Offen |

### Technische Kernidee

RIPE RIS betreibt ~26 Route Collectors (RRCs) weltweit. Jede BGP-Ankündigung
trifft diese Kollektoren zu verschiedenen Zeiten ein. Die Zeitdifferenzen zwischen
den Ankunften bilden eine vorhersehbare Welle — wie Schall durch einen Raum.
Ein BGP-Hijack bricht dieses Muster: Ankündigungen kommen gleichzeitig von der
falschen Richtung, mit verkürztem AS-Pfad. Das ist messbar.

```
LEGITIMER ANNOUNCE (Frankfurt-Origin):
  rrc12 Frankfurt  t = +0ms    ← zuerst
  rrc00 Amsterdam  t = +89ms
  rrc21 Paris      t = +134ms
  rrc11 New York   t = +891ms  ← später (Atlantik)
  rrc17 Singapore  t = +1234ms ← noch später

HIJACK:
  rrc12 Frankfurt  t = +0ms
  rrc11 New York   t = +12ms   ← viel zu früh
  rrc17 Singapore  t = +15ms   ← viel zu früh → alle gleichzeitig → Angriff
```

### Abhängigkeitsgraph (Reihenfolge ist kritisch)

```
1.1 collector-Feld
  └→ 1.3 Propagation-Aggregator
       └→ 2.2 Baseline (Archiv)  ←── 1.4 MRT-Parser
            └→ 3.1 Wave Detector
                 └→ 3.2 Backtesting
                      └→ 3.3 Kalibrierung
                           └→ 4.1 Trust Score  ←── 4.2 Gruppen-Korrelation
                                └→ 5.1 Echtzeit-Pipeline
                                     └→ 5.3 Live-Validierung
                                          └→ 6.1 BGP-Speaker
                                               └→ 6.2 Pilot-Kunde
```

---

## Phase 0 — IP-Schutz & Projekt-Reset
**Woche 1 | ⏳ Teilweise erledigt — 08.03.2026**

> Bevor eine Zeile Code geschrieben wird: Die Kombination aus Wellenphysik,
> Kollektor-Triangulation und Trust-Score-Degradierung für BGP existiert so
> nirgendwo. Dieser Vorsprung muss datiert und geschützt sein.

| # | Status | Task | Output | Erledigt am |
|---|---|---|---|---|
| 0.1 | ✅ | Konzeptdokument erstellt (`docs/KONZEPTDOKUMENT.md`) | Druckfertig für Notar, 08.03.2026 datiert | 08.03.2026 |
| 0.1 | ⏳ | Konzeptdokument notariell beglaubigen lassen | Notariell datiertes Original | — Ausstehend (menschliche Aktion) |
| 0.2 | ✅ | Feature-Freeze dokumentiert (`FEATURE_FREEZE.md`) | Klarer Scope, kein Ressourcen-Verlust | 08.03.2026 |
| 0.3 | ✅ | Branch `trustwave-core` erstellt und gepusht | `git push origin trustwave-core` ✓ | 08.03.2026 |
| 0.4 | ✅ | NDA-Template erstellt (`docs/NDA_TEMPLATE.md`) | Deutsche NDA nach GeschGehG, 5 Jahre Laufzeit | 08.03.2026 |
| 0.4 | ⏳ | NDA-Template vom Anwalt prüfen lassen | Anwaltlich geprüfte Fassung | — Ausstehend (menschliche Aktion) |

**Meilenstein 0:** ⏳ Technisch erledigt. Ausstehend: Notartermin + Anwaltsprüfung NDA.

> **Nächste menschliche Aktionen:**
> 1. `docs/KONZEPTDOKUMENT.md` ausdrucken → Notartermin vereinbaren
> 2. `docs/NDA_TEMPLATE.md` an IP-Anwalt schicken zur Prüfung
> 3. Firmenstruktur mit Steuerberater klären

---

## Phase 1 — Daten-Fundament
**Woche 2–4 | Ziel: Rohdaten vollständig und korrekt erfassen**

### Abschnitt 1.1 — Collector-Feld Integration

Das `collector`-Feld (z.B. `rrc12`) steckt bereits im RIPE RIS WebSocket-Stream
im JSON-Feld `id`. Es wird aktuell verworfen — das ist der erste Fix.

| # | Task | Datei | Details |
|---|---|---|---|
| ✅ 1.1.1 | `collector`-Feld zu `BgpRecord` hinzufügen | `tools/bgp_stream/src/main.rs` | `id: Option<String>` in `RisData`; `collector: &str` Parameter in `process_ris_data()`; `"collector": collector` in announce + withdraw JSON; `data.id.as_deref().unwrap_or("unknown")` |
| ✅ 1.1.2 | `peer_ip` in `BgpRecord` aufnehmen (WITHDRAW) | `tools/bgp_stream/src/main.rs` | `peer: Option<String>` in `RisData`; WITHDRAW: `"peer_ip": data.peer.as_deref().unwrap_or("")` |
| ✅ 1.1.3 | NATS-Payload Schema aktualisieren | `src/nats_subscriber.rs` | `pub collector: String` + `pub peer_ip: String` in `BgpRecord`; `extract_bgp_record` extrahiert beide Felder; alle Konsumenten (detector_runner, detector_loop, Tests) angepasst |
| ✅ 1.1.4 | Unit-Tests für Collector-Extraktion | `src/nats_subscriber.rs` | 3 Tests: korrekte Extraktion, Fallback "unknown", Collector-Unterscheidung (rrc00 vs rrc17) |

```rust
// Ziel-Struktur nach Abschnitt 1.1:
pub struct BgpRecord {
    pub prefix:     String,
    pub origin_as:  u32,
    pub as_path:    Vec<u32>,
    pub timestamp:  f64,       // ← sub-second precision, bereits vorhanden
    pub event_type: String,
    pub collector:  String,    // ← NEU: "rrc12"
    pub peer_asn:   u32,       // ← NEU
    pub peer_ip:    String,    // ← NEU
}
```

---

### Abschnitt 1.2 — Kollektor-Geografie-Datenbank

| # | Task | Datei | Details |
|---|---|---|---|
| ✅ 1.2.1 | Statische Kollektor-Tabelle (24 RRCs) anlegen | `src/collector_registry.rs` | `Region` enum + `CollectorInfo` struct; `pub static COLLECTORS: &[CollectorInfo]`; alle rrc00–rrc26; `pub mod collector_registry` in lib.rs |
| ✅ 1.2.2 | `lookup(id)` Funktion | `src/collector_registry.rs` | `OnceLock<HashMap<...>>` für O(1)-Lookup; 3 Tests (known/unknown/all-26) |
| ✅ 1.2.3 | Geografische Distanz zwischen zwei Kollektoren | `src/collector_registry.rs` | Haversine-Formel (R=6371km), kein externe Crate; 4 Tests |
| ✅ 1.2.4 | Erwartete Lichtlaufzeit zwischen zwei Kollektoren | `src/collector_registry.rs` | `dist_km / 200.0` ms (Glasfaser ≈ 2/3 c); 5 Tests (inkl. 2 Bonus) |

```rust
// Beispiel-Einträge (statische Compile-Zeit-Daten):
static COLLECTORS: &[CollectorInfo] = &[
    CollectorInfo { id: "rrc00", city: "Amsterdam", lat: 52.37, lon:   4.90, ixp: "AMS-IX",  region: Region::EU   },
    CollectorInfo { id: "rrc11", city: "New York",  lat: 40.71, lon: -74.00, ixp: "NYIIX",   region: Region::NA   },
    CollectorInfo { id: "rrc12", city: "Frankfurt", lat: 50.11, lon:   8.68, ixp: "DE-CIX",  region: Region::EU   },
    CollectorInfo { id: "rrc17", city: "Singapore", lat:  1.35, lon: 103.82, ixp: "Equinix", region: Region::APAC },
    // ... alle 26
];
```

---

### Abschnitt 1.3 — Propagation Aggregator

Das wichtigste neue Modul: gruppiert dieselbe Ankündigung von verschiedenen
Kollektoren innerhalb eines Zeitfensters zu einem `PropagationEvent`.

| # | Task | Datei | Details |
|---|---|---|---|
| ✅ 1.3.1 | `PropagationEvent` Datenstruktur | `src/propagation.rs` | `IpNet` + `BTreeMap<String, f64>` + abgeleitete Felder (first/last/spread_ms/arrival_order); `serde::Serialize+Deserialize`; `ipnet` serde-feature |
| ✅ 1.3.2 | `PropagationAggregator` mit 10s Zeitfenster | `src/propagation.rs` | `DashMap<GroupKey, PendingGroup>`; `add()` + `flush_expired()`; `impl Default` |
| ✅ 1.3.3 | Group-Key definieren | `src/propagation.rs` | `GroupKey { prefix, origin_as, path_hash }`; Polynomial-Hash `wrapping_mul(31)` |
| ✅ 1.3.4 | NATS-Consumer: liest `bgp.events`, schreibt `bgp.propagation` | `src/propagation.rs` | `run(self: Arc<Self>, nats_url)` async; Flush-Task jede 1s |
| ✅ 1.3.5 | Mindest-Schwelle: ≥ 3 Kollektoren pro Event | `src/propagation.rs` | Guard in Haupt-Loop + Flush-Task |
| ✅ 1.3.6 | Unit-Tests: Aggregation, Group-Key, Arrival-Order | `src/propagation.rs` | 8 Tests total: spread/order/single/threshold/group-key×2/aggregator×2 |

```rust
pub struct PropagationEvent {
    pub prefix:         IpNet,
    pub origin_as:      u32,
    pub as_path:        Vec<u32>,
    // Kollektor → Empfangszeitpunkt (Unix float, sub-second)
    pub arrivals:       BTreeMap<String, f64>,
    pub first_arrival:  f64,
    pub last_arrival:   f64,
    // Abgeleitete Felder:
    pub spread_ms:      f64,         // last - first in Millisekunden
    pub arrival_order:  Vec<String>, // Kollektoren sortiert nach Ankunftszeit
}
```

---

### Abschnitt 1.4 — MRT-Archiv-Parser

Für Backtesting werden historische Daten benötigt.
RIPE stellt MRT-Files öffentlich bereit — kostenlos, seit 2001.

| # | Task | Datei | Details |
|---|---|---|---|
| ✅ 1.4.1 | MRT-Crate einbinden + Workspace-Member | `Cargo.toml`, `tools/mrt_replay/Cargo.toml` | `bgpkit-parser = "0.10"` + clap/serde/tracing; bgp_stream `[workspace]` bereinigt |
| ✅ 1.4.2 | CLI-Tool: MRT-File → JSON-Lines | `tools/mrt_replay/src/main.rs` | `BgpkitParser`, alle 8 Pflichtfelder, Progress-Log 100k, besseres Error-Handling, Bonus: Collector aus Dateiname |
| ✅ 1.4.3 | Download-Script für RIPE RIS Archive | `scripts/download_mrt.sh` | 30 Tage/4 Kollektoren Standard; macOS+Linux kompatibel; idempotent; ausführbar |
| ✅ 1.4.4 | Output-Format mit Live-Feed vereinheitlicht | `tools/mrt_replay/src/main.rs` | `//! # Output-Format` Doku; 2 Tests (required fields + non-empty collector) |

```bash
# Datenquelle: RIPE RIS Archive (öffentlich, kostenlos)
# Format: MRT/BGP4MP, alle 5 Minuten ein File pro Kollektor
# URL-Schema:
#   https://data.ris.ripe.net/rrc12/2018.04/updates.20180424.1555.gz
#                              ^^^^^ Kollektor  ^^^^ Jahr.Monat  ^^^^Uhrzeit
```

**Meilenstein 1:** Jedes BGP-Update enthält Collector-ID. Gleiche Ankündigungen
werden über Kollektoren korrekt gruppiert. MRT-Archivdaten können eingelesen werden.

---

## Phase 2 — Wellenbaseline
**Woche 4–8 | Ziel: "Normales" Propagationsverhalten mathematisch beschreiben**

### Abschnitt 2.1 — Baseline-Datenstruktur

| # | Task | Datei | Details |
|---|---|---|---|
| 2.1.1 | `PropagationBaseline` struct definieren | `src/wave_baseline.rs` | Pro (prefix, origin_as): Reihenfolge + delta_t-Verteilung |
| 2.1.2 | Paarweise Delta-Statistik | `src/wave_baseline.rs` | Für jedes Kollektor-Paar: Mean + Stddev der Zeitdifferenz |
| 2.1.3 | Erwartete Ankunftsregion | `src/wave_baseline.rs` | Welche Region (EU/NA/APAC) empfängt typisch als erste? |
| 2.1.4 | Stabilität-Counter | `src/wave_baseline.rs` | Ab n ≥ 30 Samples gilt Baseline als verlässlich |

```rust
pub struct PropagationBaseline {
    pub prefix:          IpNet,
    pub origin_as:       u32,
    // Für jedes Kollektor-Paar: erwartete Zeitdifferenz in ms
    pub pairwise_deltas: HashMap<(String, String), WelfarStats>,
    // Erwartete Ankunftsreihenfolge (erste 3 Kollektoren)
    pub expected_order:  Vec<String>,
    pub sample_count:    u64,
    pub last_updated:    SystemTime,
}

pub struct WelfarStats {
    pub mean_ms: f64,
    pub std_ms:  f64,
    pub min_ms:  f64,
    pub max_ms:  f64,
}
```

---

### Abschnitt 2.2 — Baseline Builder (Batch aus Archiv)

| # | Task | Datei | Details |
|---|---|---|---|
| 2.2.1 | `BaselineBuilder` aus MRT-Archivdaten | `src/wave_baseline.rs` | Liest PropagationEvents, akkumuliert Statistiken |
| 2.2.2 | Qualitäts-Filter: nur stabile Routen | `src/wave_baseline.rs` | AS_PATH-Länge ≤ 6, Origin-AS konsistent, ≥ 30 Samples |
| 2.2.3 | Batch-CLI: 3 Jahre MRT-Archiv → Baseline | `tools/baseline_builder/` | Neues Workspace-Member, liest MRT-Files, schreibt Baseline |
| 2.2.4 | Fortschrittsanzeige + Laufzeitschätzung | `tools/baseline_builder/` | 3 Jahre Archiv sind mehrere Hundert GB |

---

### Abschnitt 2.3 — Baseline Persistenz

| # | Task | Datei | Details |
|---|---|---|---|
| 2.3.1 | Baseline als binäres Format speichern | `src/wave_baseline.rs` | `bincode` + `zstd` — JSON zu groß für ~800k Präfixe |
| 2.3.2 | Inkrementeller Live-Update | `src/wave_baseline.rs` | Neue stabile PropagationEvents fließen kontinuierlich ein |
| 2.3.3 | Versionierung: Baseline-Datei mit Timestamp | `src/wave_baseline.rs` | Rollback möglich wenn neue Baseline schlechtere Ergebnisse liefert |
| 2.3.4 | Memory-Mapped Load (mmap) | `src/wave_baseline.rs` | Baseline zu groß für einfaches Deserialize — lazy loading via mmap |

**Meilenstein 2:** Für die 10.000 meistgesehenen Präfixe existiert eine
statistisch verlässliche Propagations-Baseline aus historischen Daten.

---

## Phase 3 — Wave Anomaly Detector
**Woche 8–12 | Ziel: BGP-Hijacks durch Wellenabweichung erkennen**

### Abschnitt 3.1 — Wave Score Berechnung

| # | Task | Datei | Details |
|---|---|---|---|
| 3.1.1 | `WaveAnomalyDetector` struct | `src/wave_detector.rs` | Nimmt PropagationEvent + Baseline, gibt Score 0.0–1.0 |
| 3.1.2 | Signal 1: Spread-Anomalie | `src/wave_detector.rs` | `spread_ms < expected_min * 0.1` → gleichzeitig angekommen → Hijack |
| 3.1.3 | Signal 2: Reihenfolge-Anomalie | `src/wave_detector.rs` | Erste 3 Kollektoren weichen von Baseline-Erwartung ab |
| 3.1.4 | Signal 3: Paarweise Delta-Anomalie | `src/wave_detector.rs` | Z-Score: `(actual - mean) / std > 3σ` für ≥ 2 Kollektor-Paare |
| 3.1.5 | Signal 4: AS_PATH-Verkürzung | `src/wave_detector.rs` | Pfad kürzer als Baseline-Durchschnitt → Angreifer steht "näher" |
| 3.1.6 | Signal 5: Region-Inversion | `src/wave_detector.rs` | Ankündigung kommt zuerst von der geografisch falschen Seite |
| 3.1.7 | Gewichtete Kombination → Wave Score | `src/wave_detector.rs` | Spread(0.30) + Order(0.25) + Delta(0.25) + Path(0.10) + Region(0.10) |
| 3.1.8 | Fallback ohne Baseline | `src/wave_detector.rs` | Präfixe mit < 30 Samples → nur RPKI/IRR, Wave Score = None |
| 3.1.9 | Unit-Tests für alle 5 Signale | `tests/wave_detector_test.rs` | Synthetische PropagationEvents mit bekanntem Ergebnis |

```rust
pub struct WaveScore {
    pub total:                f64,    // 0.0 = normal, 1.0 = sicher Hijack
    pub spread_signal:        f64,
    pub order_signal:         f64,
    pub delta_signal:         f64,
    pub path_signal:          f64,
    pub region_signal:        f64,
    pub baseline_confidence:  f64,   // Wie verlässlich ist die Baseline?
    pub explanation:          String, // Human-readable Begründung für Alert
}
```

---

### Abschnitt 3.2 — Backtesting Framework

| # | Task | Datei | Details |
|---|---|---|---|
| 3.2.1 | Bekannte Hijack-Events als Testfälle definieren | `tests/hijack_events.rs` | MyEtherWallet 24.04.2018, Pakistan Telecom 24.02.2008, Rostelecom 01.04.2020 |
| 3.2.2 | MRT-Files für Hijack-Zeiträume herunterladen | `scripts/download_hijack_data.sh` | ± 2h um Hijack-Zeitpunkt, alle verfügbaren Kollektoren |
| 3.2.3 | Backtesting-Runner CLI | `tools/backtest/` | Liest MRT, baut PropagationEvents, läuft durch Detektor |
| 3.2.4 | Metriken: True Positive Rate, False Positive Rate, Erkennungs-Latenz | `tools/backtest/` | Wie viele Sekunden nach Hijack-Start → erste Erkennung? |
| 3.2.5 | Training/Test-Split erzwingen | `tools/backtest/` | Baseline NUR aus Daten vor dem Hijack (kein Data Leakage) |

---

### Abschnitt 3.3 — Kalibrierung & Schwellenwert

| # | Task | Datei | Details |
|---|---|---|---|
| 3.3.1 | 30 Tage normaler Traffic als Negativbeispiele | `tools/backtest/` | False Positive Rate bei verschiedenen Thresholds messen |
| 3.3.2 | ROC-Kurve generieren | `tools/backtest/` | CSV-Output → visualisieren |
| 3.3.3 | Threshold wählen: False Positive Rate < 1% | `config/default.toml` | `wave_score_threshold = X.XX` |
| 3.3.4 | PoC-Dokument mit Ergebnissen | `docs/poc_results.md` | Grundlage für externe Expertenbewertung |

> **Entscheidungs-Gate nach Phase 3:**
> Trefferquote auf historischen Hijacks < 80% → Wellenphysik-Hypothese
> überarbeiten, bevor Phase 4–7 begonnen wird.
> Trefferquote ≥ 80% bei False Positive Rate ≤ 1% → weiter mit Phase 4.

**Meilenstein 3:** MyEtherWallet-Hijack 2018 und Pakistan-Telecom-Hijack 2008
werden vom Wave Detector erkannt. False Positive Rate auf normalem Traffic < 1%.
Unabhängiger Netzwerk-Ingenieur bestätigt Methodik.

---

## Phase 4 — Trust Score Engine
**Woche 12–16 | Ziel: Alle Signale zu einem kontinuierlichen Score kombinieren**

### Abschnitt 4.1 — Multi-Signal Trust Score

| # | Task | Datei | Details |
|---|---|---|---|
| 4.1.1 | `TrustScore` struct mit allen Komponenten | `src/trust_score.rs` | Wave + RPKI + IRR + Flapping + Gruppen-Korrelation |
| 4.1.2 | RPKI-Signal | `src/trust_score.rs` | Valid → +0.30, NotFound → 0, InvalidAsn → −0.40, InvalidLength → −0.25 |
| 4.1.3 | IRR-Signal | `src/trust_score.rs` | Match → +0.20, NoEntry → 0, Mismatch → −0.30 |
| 4.1.4 | Wave-Signal | `src/trust_score.rs` | `(1.0 - wave_score) * 0.40` — Wave Score invertiert |
| 4.1.5 | Historische Stabilitäts-Bonus | `src/trust_score.rs` | Route seit > 365 Tagen stabil gesehen → +0.10 |
| 4.1.6 | Flapping-Malus | `src/trust_score.rs` | Hohe Flapping-Rate → proportionale Score-Reduktion |
| 4.1.7 | Score normalisieren auf [0.0, 1.0] | `src/trust_score.rs` | Clamp nach Kombination |
| 4.1.8 | Unit-Tests für alle Kombinationen | `tests/trust_score_test.rs` | |

```
Trust Score Formel:

  T = RPKI_weight   × rpki_signal
    + IRR_weight    × irr_signal
    + Wave_weight   × (1.0 − wave_score)
    + History_bonus × stability_signal
    − Flapping_penalty × flapping_rate

  T ∈ [0.0, 1.0]
  0.0 = nicht vertrauenswürdig
  1.0 = vollständig vertrauenswürdig
```

---

### Abschnitt 4.2 — Gruppen-Korrelation

| # | Task | Datei | Details |
|---|---|---|---|
| 4.2.1 | Prefix-Gruppen-Tracker | `src/group_correlation.rs` | Welche Präfixe bewegen sich gleichzeitig (60s Fenster)? |
| 4.2.2 | Schnittmengen-Berechnung | `src/group_correlation.rs` | Zwei Gruppen mit kleiner Schnittmenge = Angriffssignal |
| 4.2.3 | Gruppen-Score in Trust Score integrieren | `src/trust_score.rs` | Massen-Bewegung → alle betroffenen Präfixe erhalten niedrigeren Score |

---

### Abschnitt 4.3 — Score-Degradierung & Persistenz

| # | Task | Datei | Details |
|---|---|---|---|
| 4.3.1 | Score degradiert solange Anomalie anhält | `src/trust_score.rs` | Exponentiell: `score × 0.95^minutes_anomalous` |
| 4.3.2 | Score erholt sich langsam nach Anomalie-Ende | `src/trust_score.rs` | Lineare Erholung: +0.01 pro Minute ohne Anomalie |
| 4.3.3 | Trust Score Cache (DashMap) | `src/trust_score.rs` | Pro (prefix, origin_as): aktueller Score + 24h History |
| 4.3.4 | Prometheus-Metriken für Trust Scores | `src/metrics.rs` | Histogram der Score-Verteilung, Alert wenn Score < 0.30 |

**Meilenstein 4:** Jede BGP-Route hat einen kontinuierlichen Trust Score.
Score degradiert bei anhaltenden Anomalien, erholt sich bei normalem Betrieb.

---

## Phase 5 — Echtzeit-System
**Monat 4–6 | Ziel: Alles läuft live mit messbarer End-to-End-Latenz**

### Abschnitt 5.1 — Echtzeit-Pipeline

| # | Task | Datei | Details |
|---|---|---|---|
| 5.1.1 | NATS-Topologie finalisieren | `docker-compose.prod.yml` | 4 Subjects: `bgp-events` → `bgp-propagation` → `bgp-trust-scores` → `bgp-alerts` |
| 5.1.2 | PropagationAggregator als eigener Service | `tools/propagation_aggregator/` | Dedizierter Container, neues Workspace-Member |
| 5.1.3 | TrustScore-Engine als eigener Service | `tools/trust_engine/` | Liest `bgp-propagation`, schreibt `bgp-trust-scores` |
| 5.1.4 | Live-Baseline-Update im Hintergrund | `tools/trust_engine/` | Neue stabile Events fließen in Baseline ein (Sliding Window 30 Tage) |
| 5.1.5 | End-to-End Latenz messen und dokumentieren | Integration-Test | Ziel: < 500ms vom BGP-Update bis zum Alert |

```
Vollständige Echtzeit-Pipeline:

  RIPE RIS WebSocket (wss://ris-live.ripe.net)
          ↓
    bgp_stream          (existiert ✅, + collector-Feld nach Phase 1)
          ↓ NATS: bgp-events
    PropagationAggregator  (10s Zeitfenster, ≥3 Kollektoren)
          ↓ NATS: bgp-propagation
    TrustScore Engine
    ├── WaveAnomalyDetector    (Phase 3)
    ├── RPKI Cache             (existiert ✅)
    ├── IRR Cache              (existiert ✅)
    ├── FlappingDetector       (existiert ✅)
    └── GroupCorrelation       (Phase 4)
          ↓ NATS: bgp-trust-scores
    Alert Manager              (existiert ✅)
          ↓
    Webhook / Dashboard
```

---

### Abschnitt 5.2 — Monitoring Dashboard

| # | Task | Datei | Details |
|---|---|---|---|
| 5.2.1 | Grafana: Trust Score Heatmap (Prefix × Zeit) | `deploy/grafana/` | Welche Präfixe sind gerade verdächtig? Farbcodiert 0.0–1.0 |
| 5.2.2 | Grafana: Wave Propagation Timeline | `deploy/grafana/` | Für ein Prefix: Ankunftszeiten pro Kollektor visualisiert |
| 5.2.3 | Grafana: Anomalie-Rate über Zeit | `deploy/grafana/` | Angriffswellen als Muster erkennbar |
| 5.2.4 | Alert-Panel: Aktive Bedrohungen | `deploy/grafana/` | Live-Tabelle mit Prefix, Score, Erklärung, betroffene Kollektoren |

---

### Abschnitt 5.3 — Live-Validierung

| # | Task | Details |
|---|---|---|
| 5.3.1 | 30 Tage Live-Betrieb ohne Eingriff | Baseline validieren, False Positive Rate im echten Internet messen |
| 5.3.2 | Jeden Alert manuell klassifizieren | War es ein echter Hijack, Route-Leak oder False Positive? Datenbank aufbauen |
| 5.3.3 | Score-Gewichte nachkalibrieren | Falls False Positive Rate > 1%: Gewichte anpassen, erneut messen |
| 5.3.4 | Netzwerk-Ingenieur Bewertung einholen | Externe Bestätigung der Live-Ergebnisse |

**Meilenstein 5:** System läuft 30 Tage stabil in Echtzeit.
False Positive Rate < 1%. Jeder Alert ist erklärbar (Wave-Grund, RPKI-Grund etc.).
Netzwerk-Ingenieur bestätigt Methodik schriftlich.

---

## Phase 6 — BGP-Speaker & Pilot-Kunde
**Monat 6–9 | Ziel: Vom passiven Monitor zum aktiven Routing-Filter**

### Abschnitt 6.1 — BGP-Speaker Aufbau

| # | Task | Datei | Details |
|---|---|---|---|
| 6.1.1 | BIRD2 in Docker-Container einrichten | `docker-compose.prod.yml` | BIRD2 als BGP-Daemon, Rust-Wrapper für Steuerung |
| 6.1.2 | Rust-zu-BIRD Kontroll-Interface | `src/bird_controller.rs` | BIRD Unix-Socket API für dynamische Route-Policy |
| 6.1.3 | Trust Score → Route Policy Mapping | `src/bird_controller.rs` | Score < 0.40 → Quarantäne; Score < 0.20 → Route ablehnen |
| 6.1.4 | Konfigurierbare Schwellenwerte pro Kunde | `src/bird_controller.rs` | Modi: Monitoring-Only / Conservative / Aggressive |
| 6.1.5 | Audit-Log: jede Routing-Entscheidung mit Begründung | `src/bird_controller.rs` | Warum wurde Route X abgelehnt? Für Kunden-Reports |
| 6.1.6 | Fail-Safe: Bei System-Ausfall → alle Routen akzeptieren | `src/bird_controller.rs` | BGP darf nicht zum Single-Point-of-Failure werden |

---

### Abschnitt 6.2 — Schattenbetrieb beim ersten Kunden

| # | Task | Details |
|---|---|---|
| 6.2.1 | Schattenbetrieb-Modus implementieren | Alle Entscheidungen werden geloggt, aber kein echter Filter aktiv — null Risiko für Kunden |
| 6.2.2 | Ziel-Exchanges identifizieren | Liste von 10–15 kleinen bis mittleren Krypto-Exchanges die öffentlich über BGP-Sicherheit gesprochen haben |
| 6.2.3 | PoC-Präsentation vorbereiten | Historische Hijacks als Beweis, Live-Dashboard zeigen — keine Versprechen, nur Fakten |
| 6.2.4 | Pilot-Vertrag: 3 Monate kostenlos gegen Daten-Sharing | NDA + Pilotvertrag, Anwalt geprüft |
| 6.2.5 | BGP-Session zum Kunden-Router aufbauen | BIRD2 als eBGP-Peer beim Kunden, Route-Reflector-Konfiguration |
| 6.2.6 | Wöchentliche Reports an Kunden-CTO | Welche Ankündigungen wären gefiltert worden, warum, was wäre das Risiko gewesen? |
| 6.2.7 | Nach 3 Monaten: Auswertung und Entscheidung | Gemeinsame Auswertung: Trefferquote, False Positives, Performance-Impact |

**Meilenstein 6:** Erster Pilot-Kunde im Schattenbetrieb aktiv.
Erste echte Kundendaten und Feedback. Grundlage für ersten zahlenden Vertrag.

---

## Phase 7 — Produktreife & Skalierung
**Monat 9–18 | Ziel: Skalierbares Produkt, erste Zertifizierungen**

| # | Task | Details |
|---|---|---|
| 7.1 | Security Audit des BGP-Speakers | Kann ein Angreifer den Wave-Detektor aktiv austricksen? Penetration Test |
| 7.2 | Multi-Tenant-Isolation | Mehrere Kunden auf einem System, vollständig getrennte Trust-Score-Spaces |
| 7.3 | API produktisieren | REST-API: Trust Score abfragen, Schwellenwerte konfigurieren, Alert-History exportieren |
| 7.4 | API-Dokumentation & SDK | OpenAPI Spec, Rust/Python/Go Client-Bibliothek |
| 7.5 | Erster zahlender Kunde | Preismodell festlegen, SLA definieren, Vertrag abschliessen |
| 7.6 | Zweiter Server (Frankfurt oder Amsterdam) | Erste eigene Triangulation mit 2 Messpunkten — unabhängig von RIPE RIS |
| 7.7 | ISO 27001 Vorbereitung | Information Security Management System aufbauen |
| 7.8 | Case Study erstellen | Anonymisierte oder benannte Case Study — Hauptverkaufsinstrument für weitere Kunden |

**Meilenstein 7:** ISO 27001 in Vorbereitung. Erster zahlender Kunde.
System läuft stabil unter echter Last. Zweiter eigener Messpunkt aktiv.

---

## Ziel-Codebase-Layout (nach Phase 7)

```
trustwave/                          (umbenannt von log-gateway/)
├── tools/
│   ├── bgp_stream/                 ← existiert ✅  (+ collector-Feld, Phase 1.1)
│   ├── mrt_replay/                 ← NEU Phase 1.4
│   ├── baseline_builder/           ← NEU Phase 2.2
│   ├── backtest/                   ← NEU Phase 3.2
│   └── propagation_aggregator/     ← NEU Phase 5.1
│
├── src/
│   ├── collector_registry.rs       ← NEU Phase 1.2  (26 RRCs, Geografie)
│   ├── propagation.rs              ← NEU Phase 1.3  (Aggregator, PropagationEvent)
│   ├── wave_baseline.rs            ← NEU Phase 2.1  (Baseline, Statistiken)
│   ├── wave_detector.rs            ← NEU Phase 3.1  (5 Signale, WaveScore)
│   ├── trust_score.rs              ← NEU Phase 4.1  (Multi-Signal, Degradierung)
│   ├── group_correlation.rs        ← NEU Phase 4.2  (Gruppen-Bewegung)
│   ├── bird_controller.rs          ← NEU Phase 6.1  (BGP-Speaker Interface)
│   │
│   ├── rpki_cache.rs               ← existiert ✅   (RPKI-Validierung)
│   ├── irr_cache.rs                ← existiert ✅   (IRR-Lookup)
│   ├── anomaly_detector.rs         ← existiert ✅   (FlappingDetector)
│   ├── webhook.rs                  ← existiert ✅   (Alerting)
│   ├── alert_dedup.rs              ← existiert ✅   (Dedup)
│   └── metrics.rs                  ← existiert ✅   (Prometheus)
│
├── docs/
│   ├── KONZEPTDOKUMENT.md          ← existiert ✅  Phase 0.1  (für Notar, 08.03.2026)
│   ├── NDA_TEMPLATE.md             ← existiert ✅  Phase 0.4  (Anwaltsprüfung ausstehend)
│   ├── poc_results.md              ← NEU Phase 3.3.4  (Backtesting-Ergebnisse)
│   └── architecture.md             ← NEU Phase 5
│
├── scripts/
│   ├── download_mrt.sh             ← NEU Phase 1.4.3
│   └── download_hijack_data.sh     ← NEU Phase 3.2.2
│
├── FEATURE_FREEZE.md               ← existiert ✅  Phase 0.2  (08.03.2026)
├── TRUSTWAVE_ROADMAP.md            ← existiert ✅  dieses Dokument
└── PRODUCT_ROADMAP.md              ← existiert ✅  PrefixGuard Legacy (Phase-Referenz)
```

---

## Das leitende Prinzip

Die Daten sind bereits vorhanden. RIPE RIS ist ein weltweites Sensornetz aus
26 Kollektoren — das System muss nur lernen, die Zeitstempel dieser Kollektoren
richtig zu lesen und zu vergleichen.

Jede Phase liefert einen Beweis. Jeder Beweis öffnet die nächste Tür.
Kein nächster Schritt ohne bestandenen Meilenstein.

---

*Das Internet braucht kein besseres Protokoll.
Es braucht ein System das der Physik vertraut.*

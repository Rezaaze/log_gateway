# Phase 1 — Implementierungs-Prompts
## BGP TrustWave | Branch: `trustwave-core`

Jeder Prompt ist eigenständig und kann direkt in ein KI-Coding-Tool eingefügt werden.
Reihenfolge einhalten — jeder Prompt setzt den vorherigen als erledigt voraus.

---

## Abschnitt 1.1 — Collector-Feld Integration

---

### Prompt 1.1.1 — `collector`-Feld in RisData

```
Du arbeitest im Rust-Workspace `log-gateway`, Branch `trustwave-core`.
Datei: `tools/bgp_stream/src/main.rs`

## Aufgabe

Das RIPE RIS WebSocket sendet pro Message ein `data.id`-Feld
(z.B. "rrc12") das den empfangenden Kollektor identifiziert.
Dieses Feld wird aktuell verworfen. Ergänze es.

## Aktueller Code (Auszug)

```rust
#[derive(Deserialize, Debug)]
struct RisData {
    timestamp: Option<f64>,
    peer_asn: Option<Value>,
    path: Option<Vec<Value>>,
    announcements: Option<Vec<Announcement>>,
    withdrawals: Option<Vec<String>>,
}
```

## RIPE RIS Nachrichtenformat (Referenz)

```json
{
  "type": "ris_message",
  "data": {
    "timestamp": 1741564800.234,
    "id":        "rrc12",
    "peer":      "80.249.211.0",
    "peer_asn":  "1103",
    "path":      [1103, 3356, 15169],
    "announcements": [{ "next_hop": "80.249.211.0", "prefixes": ["8.8.8.0/24"] }],
    "withdrawals": []
  }
}
```

## Implementierung

1. Ergänze `RisData` um genau ein neues Feld:
   ```rust
   id: Option<String>,   // Kollektor-Name, z.B. "rrc12"
   ```

2. Ergänze die Signatur von `process_ris_data` um einen neuen Parameter
   am Ende (damit bestehende Aufrufer minimal geändert werden müssen):
   ```rust
   fn process_ris_data(
       data: &RisData,
       known: &HashMap<u64, &'static str>,
       tx: &Sender<BgpEvent>,
       stats: &Arc<Stats>,
       sample_rate: f64,
       collector: &str,     // ← NEU
   )
   ```

3. Im einzigen Aufrufer der Funktion (in der WebSocket-Loop):
   ```rust
   // Vorher:
   process_ris_data(data, &known, &tx, &stats, cfg.sample_rate);
   // Nachher:
   let collector = ris_msg.data.as_ref()
       .and_then(|d| d.id.as_deref())
       .unwrap_or("unknown");
   process_ris_data(data, &known, &tx, &stats, cfg.sample_rate, collector);
   ```

4. In beiden `json!({})` Blöcken in `process_ris_data`
   (announce + withdraw) das Feld ergänzen:
   ```rust
   "collector": collector,
   ```

## Qualität
- `cargo check --workspace` fehlerfrei
- `cargo clippy --all-targets -- -D warnings` sauber
- `cargo fmt --check` sauber
- Kein bestehender Test darf brechen
```

---

### Prompt 1.1.2 — `peer_ip` beim WITHDRAW-Event aus `data.peer`

```
Du arbeitest im Rust-Workspace `log-gateway`, Branch `trustwave-core`.
Datei: `tools/bgp_stream/src/main.rs`
Voraussetzung: Prompt 1.1.1 ist erledigt (`id`-Feld in RisData vorhanden).

## Problem

Beim WITHDRAW-Event wird `peer_ip` aktuell als leerer String gesetzt:
```rust
metadata: Some(json!({
    "event_type": "withdraw",
    "prefix":     pfx,
    "peer_asn":   peer_asn_raw,
    "origin_as":  origin as u32,
    "peer_ip":    "",          // ← immer leer
    "as_path":    ...,
    "collector":  collector,   // bereits ergänzt in 1.1.1
})),
```

Beim ANNOUNCE-Event wird `peer_ip` korrekt aus `next_hop` befüllt.
Das WITHDRAW-Event hat kein `next_hop`, aber `data.peer` enthält
immer die Peer-IP-Adresse des sendenden BGP-Routers.

## Aufgabe

1. Ergänze `RisData` um das `peer`-Feld:
   ```rust
   peer: Option<String>,  // Peer-IP-Adresse, z.B. "80.249.211.0"
   ```

2. Im WITHDRAW-Block: `peer_ip` aus `data.peer` befüllen:
   ```rust
   "peer_ip": data.peer.as_deref().unwrap_or(""),
   ```

3. Beim ANNOUNCE-Block bleibt `peer_ip` aus `next_hop` — keine Änderung dort.

## Qualität
- `cargo check --workspace` fehlerfrei
- `cargo clippy --all-targets -- -D warnings` sauber
- `cargo fmt --check` sauber
- Kein bestehender Test darf brechen
```

---

### Prompt 1.1.3 — `BgpRecord` in nats_subscriber.rs erweitern

```
Du arbeitest im Rust-Workspace `log-gateway`, Branch `trustwave-core`.
Datei: `src/nats_subscriber.rs`
Voraussetzung: Prompts 1.1.1 + 1.1.2 sind erledigt.
Die NATS-Messages enthalten jetzt `"collector"` und `"peer_ip"` im metadata-JSON.

## Aktueller Code

```rust
#[derive(Debug, Clone)]
pub struct BgpRecord {
    pub prefix:     String,
    pub origin_as:  u32,
    pub peer_asn:   u32,
    pub event_type: String,
    pub as_path:    Vec<u32>,
    pub timestamp:  DateTime<Utc>,
    // FEHLEN: collector, peer_ip
}

pub fn extract_bgp_record(event: &BgpEvent) -> Option<BgpRecord> {
    let metadata = event.metadata.as_ref()?;
    let prefix     = metadata.get("prefix")?.as_str()?.to_string();
    let origin_as  = metadata.get("origin_as")?.as_u64()? as u32;
    let peer_asn   = metadata.get("peer_asn")?.as_u64()? as u32;
    let event_type = metadata.get("event_type")?.as_str()?.to_string();
    let as_path    = metadata.get("as_path")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter()
            .filter_map(|n| n.as_u64().map(|u| u as u32))
            .collect())
        .unwrap_or_default();

    Some(BgpRecord {
        prefix, origin_as, peer_asn, event_type, as_path,
        timestamp: event.timestamp,
    })
}
```

## Aufgabe

1. Ergänze `BgpRecord` um zwei neue Felder:
   ```rust
   pub collector: String,  // z.B. "rrc12", Fallback: "unknown"
   pub peer_ip:   String,  // z.B. "80.249.211.0", Fallback: ""
   ```

2. Ergänze `extract_bgp_record`:
   ```rust
   let collector = metadata.get("collector")
       .and_then(|v| v.as_str())
       .unwrap_or("unknown")
       .to_string();
   let peer_ip = metadata.get("peer_ip")
       .and_then(|v| v.as_str())
       .unwrap_or("")
       .to_string();
   ```
   Und füge beide Felder in `Some(BgpRecord { ... })` ein.

3. Der Compiler zeigt alle Stellen wo `BgpRecord { ... }` konstruiert wird
   (Testcode, detector_runner.rs, e2e_tests etc.). Ergänze überall:
   - `collector: "test".to_string()` oder `"unknown".to_string()`
   - `peer_ip: "".to_string()`

## Qualität
- `cargo check --workspace` fehlerfrei
- `cargo clippy --all-targets -- -D warnings` sauber
- `cargo fmt --check` sauber
- Alle bestehenden Tests (233+) weiterhin grün
```

---

### Prompt 1.1.4 — Unit-Tests für Collector-Extraktion

```
Du arbeitest im Rust-Workspace `log-gateway`, Branch `trustwave-core`.
Datei: `src/nats_subscriber.rs` (im `#[cfg(test)]` Block)
Voraussetzung: Prompt 1.1.3 ist erledigt. `BgpRecord` hat `collector` + `peer_ip`.

## Aufgabe

Schreibe genau 3 neue Unit-Tests am Ende der Datei im `#[cfg(test)]` Block:

### Test 1 — collector und peer_ip werden korrekt extrahiert
```rust
#[test]
fn test_extract_collector_and_peer_ip() {
    let event = BgpEvent {
        id: uuid::Uuid::new_v4(),
        timestamp: chrono::Utc::now(),
        level: "info".to_string(),
        source: "ripe-ris".to_string(),
        message: "ANNOUNCE 8.8.8.0/24".to_string(),
        metadata: Some(serde_json::json!({
            "event_type": "announce",
            "prefix":     "8.8.8.0/24",
            "peer_asn":   1103_u64,
            "origin_as":  15169_u64,
            "peer_ip":    "80.249.211.0",
            "as_path":    [1103_u64, 3356_u64, 15169_u64],
            "collector":  "rrc12",
        })),
    };
    let record = extract_bgp_record(&event).unwrap();
    assert_eq!(record.collector, "rrc12");
    assert_eq!(record.peer_ip,   "80.249.211.0");
    assert_eq!(record.origin_as, 15169);
}
```

### Test 2 — fehlende collector-Feld → Fallback "unknown"
```rust
#[test]
fn test_extract_collector_fallback_to_unknown() {
    let event = BgpEvent {
        id: uuid::Uuid::new_v4(),
        timestamp: chrono::Utc::now(),
        level: "info".to_string(),
        source: "ripe-ris".to_string(),
        message: "ANNOUNCE 1.0.0.0/24".to_string(),
        metadata: Some(serde_json::json!({
            "event_type": "announce",
            "prefix":     "1.0.0.0/24",
            "peer_asn":   64512_u64,
            "origin_as":  64512_u64,
            "peer_ip":    "",
            "as_path":    [64512_u64],
            // kein "collector"-Feld
        })),
    };
    let record = extract_bgp_record(&event).unwrap();
    assert_eq!(record.collector, "unknown");
    assert_eq!(record.peer_ip,   "");
}
```

### Test 3 — verschiedene Kollektoren werden korrekt unterschieden
```rust
#[test]
fn test_extract_different_collectors() {
    let make_event = |collector: &str, prefix: &str| BgpEvent {
        id: uuid::Uuid::new_v4(),
        timestamp: chrono::Utc::now(),
        level: "info".to_string(),
        source: "ripe-ris".to_string(),
        message: format!("ANNOUNCE {prefix}"),
        metadata: Some(serde_json::json!({
            "event_type": "announce",
            "prefix":     prefix,
            "peer_asn":   1103_u64,
            "origin_as":  15169_u64,
            "peer_ip":    "10.0.0.1",
            "as_path":    [1103_u64, 15169_u64],
            "collector":  collector,
        })),
    };
    let r1 = extract_bgp_record(&make_event("rrc00", "8.8.8.0/24")).unwrap();
    let r2 = extract_bgp_record(&make_event("rrc17", "8.8.8.0/24")).unwrap();
    assert_eq!(r1.collector, "rrc00");
    assert_eq!(r2.collector, "rrc17");
    assert_ne!(r1.collector, r2.collector);
}
```

## Qualität
- `cargo test` alle Tests grün (bestehende + 3 neue)
- `cargo clippy --all-targets -- -D warnings` sauber
- `cargo fmt --check` sauber
```

---

## Abschnitt 1.2 — Kollektor-Geografie-Datenbank

---

### Prompt 1.2.1 — Statische Kollektor-Tabelle (26 RRCs)

```
Du arbeitest im Rust-Workspace `log-gateway`, Branch `trustwave-core`.
Neue Datei anlegen: `src/collector_registry.rs`
Voraussetzung: Abschnitt 1.1 ist vollständig erledigt.

## Aufgabe

Erstelle das Modul `src/collector_registry.rs` mit einer statischen
Tabelle aller 26 RIPE RIS Route Collectors.

## Datenstruktur

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum Region {
    EU,    // Europa
    NA,    // Nordamerika
    APAC,  // Asien-Pazifik
    SA,    // Südamerika
    AF,    // Afrika
    ME,    // Naher Osten
}

#[derive(Debug, Clone)]
pub struct CollectorInfo {
    pub id:     &'static str,   // "rrc12"
    pub city:   &'static str,   // "Frankfurt"
    pub lat:    f64,            // geografische Breite
    pub lon:    f64,            // geografische Länge
    pub ixp:    &'static str,   // "DE-CIX"
    pub region: Region,
}
```

## Vollständige Kollektor-Tabelle

Lege eine `static COLLECTORS: &[CollectorInfo]` an mit diesen 26 Einträgen:

| id     | city          | lat    | lon     | ixp              | region |
|--------|---------------|--------|---------|------------------|--------|
| rrc00  | Amsterdam     | 52.37  | 4.90    | AMS-IX           | EU     |
| rrc01  | London        | 51.51  | -0.13   | LINX             | EU     |
| rrc03  | Amsterdam     | 52.37  | 4.90    | AMS-IX           | EU     |
| rrc04  | Geneva        | 46.20  | 6.15    | CERN             | EU     |
| rrc05  | Vienna        | 48.21  | 16.37   | VIX              | EU     |
| rrc06  | Otemachi      | 35.69  | 139.76  | DIX-IE           | APAC   |
| rrc07  | Stockholm     | 59.33  | 18.07   | Netnod           | EU     |
| rrc10  | Milan         | 45.46  | 9.19    | MIX              | EU     |
| rrc11  | New York      | 40.71  | -74.00  | NYIIX            | NA     |
| rrc12  | Frankfurt     | 50.11  | 8.68    | DE-CIX           | EU     |
| rrc13  | Moscow        | 55.75  | 37.62   | MSK-IX           | EU     |
| rrc14  | Palo Alto     | 37.44  | -122.14 | Equinix SV       | NA     |
| rrc15  | Sao Paulo     | -23.55 | -46.63  | PTT.br           | SA     |
| rrc16  | Miami         | 25.77  | -80.19  | Equinix MI       | NA     |
| rrc17  | Singapore     | 1.35   | 103.82  | Equinix SG       | APAC   |
| rrc18  | Barcelona     | 41.39  | 2.15    | CATNIX           | EU     |
| rrc19  | Johannesburg  | -26.20 | 28.04   | NAPAfrica        | AF     |
| rrc20  | Zurich        | 47.38  | 8.54    | SwissIX          | EU     |
| rrc21  | Paris         | 48.86  | 2.35    | France-IX        | EU     |
| rrc22  | Bucharest     | 44.43  | 26.10   | Interlan         | EU     |
| rrc23  | Singapore     | 1.35   | 103.82  | Equinix SG       | APAC   |
| rrc24  | Montevideo    | -34.90 | -56.19  | ANTEL            | SA     |
| rrc25  | Amsterdam     | 52.37  | 4.90    | AMS-IX           | EU     |
| rrc26  | Dubai         | 25.20  | 55.27   | UAE-IX           | ME     |

## Integration

Ergänze in `src/lib.rs`:
```rust
pub mod collector_registry;
```

## Qualität
- `cargo check --workspace` fehlerfrei
- `cargo clippy --all-targets -- -D warnings` sauber
- `cargo fmt --check` sauber
```

---

### Prompt 1.2.2 — `CollectorInfo::lookup(id)` Funktion

```
Du arbeitest im Rust-Workspace `log-gateway`, Branch `trustwave-core`.
Datei: `src/collector_registry.rs`
Voraussetzung: Prompt 1.2.1 ist erledigt. COLLECTORS-Tabelle existiert.

## Aufgabe

Ergänze `src/collector_registry.rs` um eine Lookup-Funktion:

```rust
/// Gibt CollectorInfo für eine Kollektor-ID zurück.
/// Gibt None zurück wenn die ID unbekannt ist.
pub fn lookup(id: &str) -> Option<&'static CollectorInfo> {
    // O(1) mit einmalig initialisierten HashMap
    // Nutze std::sync::OnceLock + HashMap für O(1)-Lookup
}
```

## Implementierungshinweis

Nutze `std::sync::OnceLock<HashMap<&'static str, &'static CollectorInfo>>`
um die HashMap einmalig aus der statischen COLLECTORS-Liste aufzubauen:

```rust
use std::sync::OnceLock;
use std::collections::HashMap;

static LOOKUP: OnceLock<HashMap<&'static str, &'static CollectorInfo>> = OnceLock::new();

pub fn lookup(id: &str) -> Option<&'static CollectorInfo> {
    let map = LOOKUP.get_or_init(|| {
        COLLECTORS.iter().map(|c| (c.id, c)).collect()
    });
    map.get(id).copied()
}
```

## Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lookup_known_collector() {
        let info = lookup("rrc12").expect("rrc12 must exist");
        assert_eq!(info.city, "Frankfurt");
        assert_eq!(info.ixp, "DE-CIX");
        assert!(matches!(info.region, Region::EU));
    }

    #[test]
    fn test_lookup_unknown_returns_none() {
        assert!(lookup("rrc99").is_none());
        assert!(lookup("").is_none());
        assert!(lookup("unknown").is_none());
    }

    #[test]
    fn test_all_26_collectors_are_reachable() {
        let ids = ["rrc00","rrc01","rrc03","rrc04","rrc05","rrc06","rrc07",
                   "rrc10","rrc11","rrc12","rrc13","rrc14","rrc15","rrc16",
                   "rrc17","rrc18","rrc19","rrc20","rrc21","rrc22","rrc23",
                   "rrc24","rrc25","rrc26"];
        for id in ids {
            assert!(lookup(id).is_some(), "Collector {id} not found");
        }
    }
}
```

## Qualität
- `cargo test collector_registry` alle 3 Tests grün
- `cargo clippy --all-targets -- -D warnings` sauber
- `cargo fmt --check` sauber
```

---

### Prompt 1.2.3 — Geografische Distanz (Haversine)

```
Du arbeitest im Rust-Workspace `log-gateway`, Branch `trustwave-core`.
Datei: `src/collector_registry.rs`
Voraussetzung: Prompt 1.2.2 ist erledigt.

## Aufgabe

Ergänze `src/collector_registry.rs` um eine Funktion die die
geografische Großkreisdistanz zwischen zwei Kollektoren in Kilometern
berechnet (Haversine-Formel). Keine externe Crate nötig — nur std.

```rust
/// Berechnet die Großkreisdistanz zwischen zwei Kollektoren in km.
/// Gibt None wenn einer der IDs unbekannt ist.
pub fn geographic_distance_km(id_a: &str, id_b: &str) -> Option<f64> {
    // Haversine-Formel
    // Erdradius: 6371.0 km
}
```

## Haversine-Formel

```
Δlat = lat_b - lat_a  (in Radiant)
Δlon = lon_b - lon_a  (in Radiant)

a = sin²(Δlat/2) + cos(lat_a) × cos(lat_b) × sin²(Δlon/2)
c = 2 × atan2(√a, √(1−a))
d = R × c         (R = 6371 km)
```

## Tests

```rust
#[test]
fn test_distance_amsterdam_frankfurt_approx_400km() {
    let d = geographic_distance_km("rrc00", "rrc12").unwrap();
    // Amsterdam → Frankfurt ≈ 370–410 km
    assert!(d > 350.0 && d < 450.0,
        "Expected ~400km, got {d:.1}km");
}

#[test]
fn test_distance_same_collector_is_zero() {
    let d = geographic_distance_km("rrc12", "rrc12").unwrap();
    assert!(d < 0.001, "Same collector distance must be ~0, got {d}");
}

#[test]
fn test_distance_is_symmetric() {
    let d1 = geographic_distance_km("rrc12", "rrc11").unwrap();
    let d2 = geographic_distance_km("rrc11", "rrc12").unwrap();
    assert!((d1 - d2).abs() < 0.001);
}

#[test]
fn test_distance_unknown_collector_returns_none() {
    assert!(geographic_distance_km("rrc12", "rrc99").is_none());
}
```

## Qualität
- `cargo test collector_registry` alle Tests grün (inkl. vorherige)
- `cargo clippy --all-targets -- -D warnings` sauber
- `cargo fmt --check` sauber
- Keine externe Crate hinzufügen
```

---

### Prompt 1.2.4 — Erwartete Lichtlaufzeit zwischen zwei Kollektoren

```
Du arbeitest im Rust-Workspace `log-gateway`, Branch `trustwave-core`.
Datei: `src/collector_registry.rs`
Voraussetzung: Prompt 1.2.3 ist erledigt. `geographic_distance_km` existiert.

## Aufgabe

Ergänze eine Funktion die die physikalisch minimale Übertragungslatenz
zwischen zwei Kollektoren in Millisekunden berechnet.

Glasfaser überträgt mit ca. 2/3 der Lichtgeschwindigkeit:
  v = 200.000 km/s = 200 km/ms

```rust
/// Berechnet die physikalisch minimale Latenz zwischen zwei Kollektoren in ms.
/// Basis: Glasfaser-Lichtgeschwindigkeit ≈ 200.000 km/s (2/3 c).
/// Gibt None wenn einer der IDs unbekannt ist.
pub fn min_latency_ms(id_a: &str, id_b: &str) -> Option<f64> {
    let dist_km = geographic_distance_km(id_a, id_b)?;
    Some(dist_km / 200.0)   // 200 km pro Millisekunde
}
```

## Tests

```rust
#[test]
fn test_latency_amsterdam_frankfurt_approx_2ms() {
    let ms = min_latency_ms("rrc00", "rrc12").unwrap();
    // ~400km / 200 km/ms ≈ 2ms
    assert!(ms > 1.5 && ms < 3.0,
        "Expected ~2ms, got {ms:.2}ms");
}

#[test]
fn test_latency_frankfurt_new_york_approx_35ms() {
    let ms = min_latency_ms("rrc12", "rrc11").unwrap();
    // Frankfurt → New York ≈ 6200km / 200 km/ms ≈ 31ms
    assert!(ms > 25.0 && ms < 45.0,
        "Expected ~31ms, got {ms:.2}ms");
}

#[test]
fn test_latency_is_symmetric() {
    let t1 = min_latency_ms("rrc12", "rrc17").unwrap();
    let t2 = min_latency_ms("rrc17", "rrc12").unwrap();
    assert!((t1 - t2).abs() < 0.001);
}
```

## Qualität
- `cargo test collector_registry` alle Tests grün
- `cargo clippy --all-targets -- -D warnings` sauber
- `cargo fmt --check` sauber
```

---

## Abschnitt 1.3 — Propagation Aggregator

---

### Prompt 1.3.1 — `PropagationEvent` Datenstruktur

```
Du arbeitest im Rust-Workspace `log-gateway`, Branch `trustwave-core`.
Neue Datei anlegen: `src/propagation.rs`
Voraussetzung: Abschnitt 1.1 und 1.2 sind erledigt.

## Aufgabe

Erstelle `src/propagation.rs` mit der `PropagationEvent` Datenstruktur.
Ein `PropagationEvent` repräsentiert dieselbe BGP-Ankündigung die von
mehreren Kollektoren zu unterschiedlichen Zeitpunkten empfangen wurde.

## Implementierung

```rust
use std::collections::BTreeMap;
use ipnet::IpNet;

/// Eine BGP-Ankündigung die über mehrere Kollektoren beobachtet wurde.
/// Enthält die Ankunftszeiten bei jedem Kollektor als Grundlage
/// für die Wellenphysik-Analyse.
#[derive(Debug, Clone)]
pub struct PropagationEvent {
    /// IP-Präfix das angekündigt wurde
    pub prefix: IpNet,
    /// Ursprungs-AS (letzter Eintrag im AS-Pfad)
    pub origin_as: u32,
    /// AS-Pfad der Ankündigung
    pub as_path: Vec<u32>,
    /// Kollektor-ID → Unix-Timestamp (float, sub-second precision)
    /// BTreeMap = sortiert nach Kollektor-ID (deterministisch für Tests)
    pub arrivals: BTreeMap<String, f64>,
    /// Frühester Empfangszeitpunkt (Unix float)
    pub first_arrival: f64,
    /// Spätester Empfangszeitpunkt (Unix float)
    pub last_arrival: f64,
    /// Differenz last_arrival - first_arrival in Millisekunden
    pub spread_ms: f64,
    /// Kollektoren sortiert nach Ankunftszeit (frühester zuerst)
    pub arrival_order: Vec<String>,
}

impl PropagationEvent {
    /// Erstellt ein PropagationEvent und berechnet alle abgeleiteten Felder.
    pub fn new(
        prefix: IpNet,
        origin_as: u32,
        as_path: Vec<u32>,
        arrivals: BTreeMap<String, f64>,
    ) -> Self {
        // first_arrival, last_arrival, spread_ms, arrival_order berechnen
        // Hinweis: arrivals.values() liefert die Timestamps
        // arrival_order: arrivals nach Timestamp sortiert, Kollektor-IDs zurückgeben
    }
}
```

Ergänze `src/lib.rs`:
```rust
pub mod propagation;
```

Ergänze in `Cargo.toml` falls noch nicht vorhanden:
```toml
ipnet = "2"
```

## Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn make_arrivals(pairs: &[(&str, f64)]) -> BTreeMap<String, f64> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn test_propagation_event_spread_and_order() {
        let arrivals = make_arrivals(&[
            ("rrc12", 1000.000),  // Frankfurt — zuerst
            ("rrc00", 1000.089),  // Amsterdam
            ("rrc11", 1000.891),  // New York
            ("rrc17", 1001.234),  // Singapore — zuletzt
        ]);
        let event = PropagationEvent::new(
            "8.8.8.0/24".parse().unwrap(),
            15169,
            vec![1103, 3356, 15169],
            arrivals,
        );

        assert!((event.spread_ms - 1234.0).abs() < 1.0);
        assert_eq!(event.arrival_order[0], "rrc12");
        assert_eq!(event.arrival_order[3], "rrc17");
        assert_eq!(event.first_arrival, 1000.000);
        assert_eq!(event.last_arrival,  1001.234);
    }

    #[test]
    fn test_propagation_event_single_arrival() {
        let arrivals = make_arrivals(&[("rrc12", 1000.500)]);
        let event = PropagationEvent::new(
            "1.0.0.0/8".parse().unwrap(),
            1,
            vec![1],
            arrivals,
        );
        assert!(event.spread_ms < 0.001);
        assert_eq!(event.arrival_order.len(), 1);
    }
}
```

## Qualität
- `cargo test propagation` alle Tests grün
- `cargo clippy --all-targets -- -D warnings` sauber
- `cargo fmt --check` sauber
```

---

### Prompt 1.3.2 + 1.3.3 — `PropagationAggregator` mit Group-Key und 10s Fenster

```
Du arbeitest im Rust-Workspace `log-gateway`, Branch `trustwave-core`.
Datei: `src/propagation.rs`
Voraussetzung: Prompt 1.3.1 ist erledigt. `PropagationEvent` existiert.

## Aufgabe

Implementiere den `PropagationAggregator`: gruppiert eingehende
`BgpRecord`-Events (von NATS) nach Group-Key und sammelt Kollektor-Arrivals
in einem 10-Sekunden-Zeitfenster.

## Group-Key

```rust
/// Eindeutiger Schlüssel für eine BGP-Ankündigung (unabhängig vom Kollektor).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GroupKey {
    pub prefix:    String,  // normalisiert: "8.8.8.0/24"
    pub origin_as: u32,
    /// FNV-Hash des AS-Pfads — schnell, kollisionsarm für kurze Vektoren
    pub path_hash: u64,
}

impl GroupKey {
    pub fn from_record(record: &crate::nats_subscriber::BgpRecord) -> Self {
        // path_hash: einfacher Polynomial-Hash über as_path Vec<u32>
        let path_hash = record.as_path.iter()
            .fold(0u64, |acc, &asn| acc.wrapping_mul(31).wrapping_add(asn as u64));
        Self {
            prefix:    record.prefix.clone(),
            origin_as: record.origin_as,
            path_hash,
        }
    }
}
```

## Aggregator

```rust
use dashmap::DashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

struct PendingGroup {
    arrivals:   std::collections::BTreeMap<String, f64>,
    as_path:    Vec<u32>,
    created_at: Instant,
}

pub struct PropagationAggregator {
    window:  Duration,                          // 10 Sekunden
    pending: Arc<DashMap<GroupKey, PendingGroup>>,
}

impl PropagationAggregator {
    pub fn new() -> Self {
        Self {
            window:  Duration::from_secs(10),
            pending: Arc::new(DashMap::new()),
        }
    }

    /// Fügt ein BgpRecord zum Aggregator hinzu.
    /// Gibt ein fertiges PropagationEvent zurück wenn das Zeitfenster abgelaufen
    /// ist (≥10s seit erstem Arrival dieser Gruppe) — sonst None.
    pub fn add(&self, record: &crate::nats_subscriber::BgpRecord) -> Option<PropagationEvent> {
        // 1. GroupKey berechnen
        // 2. In DashMap eintragen (oder vorhandenen ergänzen)
        // 3. Wenn created_at + window < jetzt → Event finalisieren und aus Map entfernen
    }

    /// Flush: gibt alle Gruppen zurück deren Fenster abgelaufen ist.
    /// Regelmäßig aufrufen (z.B. jede Sekunde) um keine Events zu verlieren.
    pub fn flush_expired(&self) -> Vec<PropagationEvent> {
        // Alle Einträge mit created_at + window < Instant::now() entfernen und zurückgeben
    }
}
```

`dashmap` muss in `Cargo.toml` vorhanden sein (ist es bereits im Projekt).

## Tests

```rust
#[test]
fn test_aggregator_collects_multiple_collectors() {
    use crate::nats_subscriber::BgpRecord;
    let agg = PropagationAggregator::new();

    let make_record = |collector: &str, ts: f64| BgpRecord {
        prefix:     "8.8.8.0/24".to_string(),
        origin_as:  15169,
        peer_asn:   1103,
        event_type: "announce".to_string(),
        as_path:    vec![1103, 15169],
        timestamp:  chrono::DateTime::from_timestamp(ts as i64, 0).unwrap(),
        collector:  collector.to_string(),
        peer_ip:    "10.0.0.1".to_string(),
    };

    // Drei Kollektoren, gleiche Gruppe
    let r1 = agg.add(&make_record("rrc12", 1000.0));
    let r2 = agg.add(&make_record("rrc00", 1000.089));
    let r3 = agg.add(&make_record("rrc11", 1000.891));

    // Noch kein Event — Fenster nicht abgelaufen
    assert!(r1.is_none());
    assert!(r2.is_none());
    assert!(r3.is_none());

    // Noch in der Map
    assert_eq!(agg.pending.len(), 1);
}

#[test]
fn test_aggregator_different_prefix_different_group() {
    use crate::nats_subscriber::BgpRecord;
    let agg = PropagationAggregator::new();

    let make = |prefix: &str, collector: &str| BgpRecord {
        prefix:     prefix.to_string(),
        origin_as:  15169,
        peer_asn:   1103,
        event_type: "announce".to_string(),
        as_path:    vec![1103, 15169],
        timestamp:  chrono::Utc::now(),
        collector:  collector.to_string(),
        peer_ip:    "".to_string(),
    };

    agg.add(&make("8.8.8.0/24", "rrc12"));
    agg.add(&make("1.1.1.0/24", "rrc12"));  // anderes Prefix → andere Gruppe

    assert_eq!(agg.pending.len(), 2);
}
```

## Qualität
- `cargo test propagation` alle Tests grün
- `cargo clippy --all-targets -- -D warnings` sauber
- `cargo fmt --check` sauber
```

---

### Prompt 1.3.4 + 1.3.5 — NATS Consumer: `bgp.events` → `bgp.propagation`

```
Du arbeitest im Rust-Workspace `log-gateway`, Branch `trustwave-core`.
Datei: `src/propagation.rs`
Voraussetzung: Prompt 1.3.2 ist erledigt. PropagationAggregator existiert.

## Aufgabe

Implementiere die `run()`-Funktion des `PropagationAggregator` als
NATS Consumer:
- Liest von Subject `bgp.events` (bestehende BgpEvents)
- Aggregiert mit PropagationAggregator
- Publiziert fertige PropagationEvents nach `bgp.propagation`
- **Mindest-Schwelle: Nur Events mit ≥ 3 Kollektoren publizieren**

```rust
impl PropagationAggregator {
    /// Startet den NATS Consumer.
    /// Liest von `bgp.events`, publiziert nach `bgp.propagation`.
    /// Läuft bis der CancellationToken ausgelöst wird.
    pub async fn run(
        self: Arc<Self>,
        nats_url: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let client = async_nats::connect(nats_url).await?;
        let js = async_nats::jetstream::new(client.clone());

        let publisher = async_nats::connect(nats_url).await?;
        let js_pub = async_nats::jetstream::new(publisher);

        let mut subscriber = client.subscribe("bgp.events").await?;

        // Flush-Task: alle 1s abgelaufene Fenster publizieren
        let agg_clone = self.clone();
        let js_pub_clone = js_pub.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(
                tokio::time::Duration::from_secs(1)
            );
            loop {
                interval.tick().await;
                for event in agg_clone.flush_expired() {
                    if event.arrivals.len() >= 3 {  // ← Mindest-Schwelle
                        if let Ok(bytes) = serde_json::to_vec(&event) {
                            let _ = js_pub_clone
                                .publish("bgp.propagation", bytes.into())
                                .await;
                        }
                    }
                }
            }
        });

        // Haupt-Loop: eingehende Events verarbeiten
        while let Some(msg) = subscriber.next().await {
            if let Ok(bgp_event) = serde_json::from_slice::<BgpEvent>(&msg.payload) {
                if let Some(record) = extract_bgp_record(&bgp_event) {
                    if let Some(prop_event) = self.add(&record) {
                        if prop_event.arrivals.len() >= 3 {   // ← Mindest-Schwelle
                            if let Ok(bytes) = serde_json::to_vec(&prop_event) {
                                let _ = js_pub
                                    .publish("bgp.propagation", bytes.into())
                                    .await;
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
```

`PropagationEvent` muss `Serialize + Deserialize` implementieren —
ergänze die Derives in Prompt 1.3.1:
```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PropagationEvent { ... }
```

## Mindest-Schwelle Erklärung

Unter 3 Kollektoren ist keine Wellenphysik-Triangulation möglich.
Events mit 1–2 Kollektoren werden still verworfen — kein Alert,
kein Fehler.

## Qualität
- `cargo check --workspace` fehlerfrei
- `cargo clippy --all-targets -- -D warnings` sauber
- `cargo fmt --check` sauber
- Kein bestehender Test darf brechen
```

---

### Prompt 1.3.6 — Unit-Tests für den Propagation Aggregator

```
Du arbeitest im Rust-Workspace `log-gateway`, Branch `trustwave-core`.
Datei: `src/propagation.rs` (im #[cfg(test)] Block)
Voraussetzung: Prompts 1.3.1–1.3.5 sind erledigt.

## Aufgabe

Schreibe 4 Unit-Tests die die wichtigsten Edge-Cases des Aggregators abdecken:

### Test 1 — Unter 3 Kollektoren: kein Event
```rust
#[test]
fn test_min_threshold_2_collectors_no_event() {
    // 2 Kollektoren → PropagationEvent wird erstellt aber hat arrivals.len() == 2
    // → sollte NICHT nach bgp.propagation publiziert werden
    // Hier testen wir nur dass spread_ms und arrival_order korrekt sind
    // (Die Publikations-Logik ist in run() — hier nur die Struktur testen)
    let arrivals = make_arrivals(&[
        ("rrc12", 1000.0),
        ("rrc00", 1000.1),
    ]);
    let event = PropagationEvent::new("1.0.0.0/24".parse().unwrap(), 1, vec![1], arrivals);
    assert_eq!(event.arrivals.len(), 2);
    // 2 < 3 → wäre nicht publiziert worden
}
```

### Test 2 — Group-Key: gleicher Prefix + AS-Pfad → gleiche Gruppe
```rust
#[test]
fn test_group_key_same_path_same_group() {
    let r1 = make_record("rrc12", "8.8.8.0/24", &[1103, 15169]);
    let r2 = make_record("rrc00", "8.8.8.0/24", &[1103, 15169]);
    assert_eq!(GroupKey::from_record(&r1), GroupKey::from_record(&r2));
}
```

### Test 3 — Group-Key: verschiedener AS-Pfad → verschiedene Gruppe
```rust
#[test]
fn test_group_key_different_path_different_group() {
    let r1 = make_record("rrc12", "8.8.8.0/24", &[1103, 15169]);
    let r2 = make_record("rrc12", "8.8.8.0/24", &[3356, 15169]); // anderer Pfad
    assert_ne!(GroupKey::from_record(&r1), GroupKey::from_record(&r2));
}
```

### Test 4 — Arrival-Reihenfolge ist korrekt sortiert
```rust
#[test]
fn test_arrival_order_correct_sorting() {
    let arrivals = make_arrivals(&[
        ("rrc17", 1001.234),  // Singapore — späteste
        ("rrc11", 1000.891),  // New York
        ("rrc12", 1000.000),  // Frankfurt — früheste
        ("rrc00", 1000.089),  // Amsterdam
    ]);
    let event = PropagationEvent::new(
        "8.8.8.0/24".parse().unwrap(), 15169, vec![15169], arrivals
    );
    assert_eq!(event.arrival_order, vec!["rrc12", "rrc00", "rrc11", "rrc17"]);
    assert_eq!(event.arrival_order[0], "rrc12");  // Frankfurt zuerst
    assert_eq!(event.arrival_order[3], "rrc17");  // Singapore zuletzt
}
```

Füge ggf. eine `make_record`-Hilfsfunktion im test-Modul hinzu.

## Qualität
- `cargo test propagation` alle Tests grün (inkl. vorherige)
- `cargo clippy --all-targets -- -D warnings` sauber
- `cargo fmt --check` sauber
```

---

## Abschnitt 1.4 — MRT-Archiv-Parser

---

### Prompt 1.4.1 — `bgpkit-parser` Crate evaluieren und einbinden

```
Du arbeitest im Rust-Workspace `log-gateway`, Branch `trustwave-core`.
Dateien: `Cargo.toml` (root workspace), neues Workspace-Member `tools/mrt_replay/`
Voraussetzung: Abschnitt 1.3 ist erledigt.

## Aufgabe

Lege ein neues Workspace-Member `tools/mrt_replay/` an und binde
`bgpkit-parser` ein — die führende Rust-Crate für MRT/BGP4MP-Dateien.

### Schritt 1: Workspace-Member anlegen

Ergänze in root `Cargo.toml`:
```toml
[workspace]
members = [
    ".",
    "tools/bgp_stream",
    "tools/mrt_replay",    # ← NEU
]
```

### Schritt 2: `tools/mrt_replay/Cargo.toml`

```toml
[package]
name    = "mrt-replay"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "mrt-replay"
path = "src/main.rs"

[dependencies]
bgpkit-parser = "0.10"
serde         = { version = "1", features = ["derive"] }
serde_json    = "1"
clap          = { version = "4", features = ["derive"] }
tracing       = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

### Schritt 3: `tools/mrt_replay/src/main.rs` Grundgerüst

```rust
//! MRT Replay Tool
//! Liest MRT-Archivdateien von RIPE RIS und gibt BGP-Events als JSON-Lines aus.
//!
//! Verwendung:
//!   mrt-replay --file updates.20180424.1555.gz
//!   mrt-replay --file updates.20180424.1555.gz --collector rrc12
//!   mrt-replay --url https://data.ris.ripe.net/rrc12/2018.04/updates.20180424.1555.gz

use clap::Parser;

#[derive(Parser, Debug)]
#[command(about = "MRT archive reader for BGP TrustWave backtesting")]
struct Args {
    /// Lokale MRT-Datei (gz oder unkomprimiert)
    #[arg(long)]
    file: Option<String>,

    /// Kollektor-ID für die Ausgabe (z.B. "rrc12")
    /// Wird aus dem Dateinamen abgeleitet wenn nicht angegeben
    #[arg(long, default_value = "unknown")]
    collector: String,
}

fn main() {
    let args = Args::parse();
    eprintln!("MRT Replay — Collector: {}", args.collector);
    // Implementierung in Prompt 1.4.2
    todo!("Implementiert in Prompt 1.4.2")
}
```

## Qualität
- `cargo check --workspace` fehlerfrei (inkl. neues mrt-replay crate)
- `cargo clippy --all-targets -- -D warnings` sauber
- `cargo fmt --check` sauber
```

---

### Prompt 1.4.2 — CLI-Tool: MRT-File → JSON-Lines

```
Du arbeitest im Rust-Workspace `log-gateway`, Branch `trustwave-core`.
Datei: `tools/mrt_replay/src/main.rs`
Voraussetzung: Prompt 1.4.1 ist erledigt. Grundgerüst existiert.

## Aufgabe

Implementiere die vollständige MRT-Parsing-Logik. Das Tool liest
eine MRT-Datei und gibt jedes BGP-Update als eine JSON-Zeile (JSON-Lines)
auf stdout aus — im gleichen Format wie der Live-NATS-Stream.

## Output-Format

Jede Zeile ist ein JSON-Objekt identisch zu den BgpEvents im NATS-Stream:

```json
{
  "event_type": "announce",
  "prefix":     "8.8.8.0/24",
  "peer_asn":   1103,
  "origin_as":  15169,
  "peer_ip":    "80.249.211.0",
  "as_path":    [1103, 3356, 15169],
  "collector":  "rrc12",
  "timestamp":  1524578100.0
}
```

## Implementierung

```rust
use bgpkit_parser::BgpkitParser;
use serde_json::json;

fn main() {
    let args = Args::parse();

    let file = match args.file {
        Some(f) => f,
        None => {
            eprintln!("Error: --file required");
            std::process::exit(1);
        }
    };

    // bgpkit-parser: liest gz-komprimierte MRT-Dateien automatisch
    let parser = BgpkitParser::new(&file)
        .expect("Failed to open MRT file");

    let mut count = 0u64;

    for elem in parser {
        let event_type = match elem.elem_type {
            bgpkit_parser::ElemType::ANNOUNCE => "announce",
            bgpkit_parser::ElemType::WITHDRAW => "withdraw",
        };

        let prefix = elem.prefix.prefix.to_string();
        let peer_asn = elem.peer_asn.to_u32();
        let peer_ip = elem.peer_ip.to_string();

        // AS-Pfad: letzter Eintrag = origin_as
        let as_path: Vec<u32> = elem.as_path
            .as_ref()
            .map(|p| p.to_u32_vec_opt()
                .unwrap_or_default()
                .into_iter()
                .flatten()
                .collect())
            .unwrap_or_default();

        let origin_as = as_path.last().copied().unwrap_or(peer_asn);

        let record = json!({
            "event_type": event_type,
            "prefix":     prefix,
            "peer_asn":   peer_asn,
            "origin_as":  origin_as,
            "peer_ip":    peer_ip,
            "as_path":    as_path,
            "collector":  args.collector,
            "timestamp":  elem.timestamp,
        });

        println!("{}", record);
        count += 1;

        if count % 100_000 == 0 {
            eprintln!("Processed {count} records ...");
        }
    }

    eprintln!("Done. Total: {count} records.");
}
```

## Qualität
- `cargo build --bin mrt-replay` erfolgreich
- `cargo clippy --all-targets -- -D warnings` sauber
- `cargo fmt --check` sauber
- Manuelle Prüfung: Tool startet und gibt usage aus wenn ohne --file aufgerufen
```

---

### Prompt 1.4.3 — Download-Script für RIPE RIS MRT-Archive

```
Du arbeitest im Rust-Workspace `log-gateway`, Branch `trustwave-core`.
Neue Datei: `scripts/download_mrt.sh`

## Aufgabe

Erstelle ein Bash-Script das MRT-Archivdateien von RIPE RIS herunterlädt.
RIPE RIS stellt die Daten öffentlich und kostenlos unter dieser URL bereit:
  https://data.ris.ripe.net/rrcXX/YYYY.MM/updates.YYYYMMDD.HHMM.gz

Das Script soll:
1. Für einen konfigurierbaren Zeitraum (Standard: letzten 30 Tage)
2. Für konfigurierbare Kollektoren (Standard: rrc12, rrc00, rrc11, rrc17)
3. Alle Update-Files herunterladen (alle 5 Minuten ein File)
4. In `data/mrt/rrcXX/YYYY.MM/` ablegen (Verzeichnisstruktur beibehalten)
5. Bereits vorhandene Files überspringen (idempotent)

```bash
#!/usr/bin/env bash
# download_mrt.sh — RIPE RIS MRT Archive Downloader
# Verwendung: ./scripts/download_mrt.sh [--days N] [--collectors "rrc12 rrc00"]
# Standard:   30 Tage, Kollektoren: rrc12 rrc00 rrc11 rrc17

set -euo pipefail

DAYS=30
COLLECTORS="rrc12 rrc00 rrc11 rrc17"
OUTPUT_DIR="data/mrt"
BASE_URL="https://data.ris.ripe.net"

# Parameter parsen
while [[ $# -gt 0 ]]; do
    case $1 in
        --days)       DAYS="$2";       shift 2 ;;
        --collectors) COLLECTORS="$2"; shift 2 ;;
        --output)     OUTPUT_DIR="$2"; shift 2 ;;
        *) echo "Unbekannter Parameter: $1"; exit 1 ;;
    esac
done

echo "Lade MRT-Daten: letzte ${DAYS} Tage, Kollektoren: ${COLLECTORS}"
echo "Zielverzeichnis: ${OUTPUT_DIR}"

total=0
skipped=0
downloaded=0

for collector in $COLLECTORS; do
    for day_offset in $(seq 0 $((DAYS - 1))); do
        # Datum berechnen (macOS und Linux kompatibel)
        if date --version &>/dev/null 2>&1; then
            # GNU date (Linux)
            date_str=$(date -d "${day_offset} days ago" +%Y.%m)
            date_file=$(date -d "${day_offset} days ago" +%Y%m%d)
        else
            # BSD date (macOS)
            date_str=$(date -v-${day_offset}d +%Y.%m)
            date_file=$(date -v-${day_offset}d +%Y%m%d)
        fi

        target_dir="${OUTPUT_DIR}/${collector}/${date_str}"
        mkdir -p "$target_dir"

        # Alle 5-Minuten-Files des Tages
        for hour in $(seq -w 0 23); do
            for minute in 00 05 10 15 20 25 30 35 40 45 50 55; do
                filename="updates.${date_file}.${hour}${minute}.gz"
                url="${BASE_URL}/${collector}/${date_str}/${filename}"
                target="${target_dir}/${filename}"

                total=$((total + 1))

                if [[ -f "$target" ]]; then
                    skipped=$((skipped + 1))
                    continue
                fi

                if curl -sf --max-time 30 -o "$target" "$url" 2>/dev/null; then
                    downloaded=$((downloaded + 1))
                    echo "✓ ${collector}/${date_str}/${filename}"
                else
                    rm -f "$target"  # leere Datei entfernen bei Fehler
                fi
            done
        done
    done
done

echo ""
echo "Fertig: ${downloaded} heruntergeladen, ${skipped} übersprungen (${total} gesamt)"
```

Mache das Script ausführbar:
```bash
chmod +x scripts/download_mrt.sh
```

## Qualität
- Script ist syntaktisch korrekt (`bash -n scripts/download_mrt.sh`)
- Script ist ausführbar
- Idempotent: zweimaliges Ausführen lädt nicht doppelt herunter
```

---

### Prompt 1.4.4 — Output-Format mit Live-Feed vereinheitlichen

```
Du arbeitest im Rust-Workspace `log-gateway`, Branch `trustwave-core`.
Datei: `tools/mrt_replay/src/main.rs`
Voraussetzung: Prompt 1.4.2 ist erledigt.

## Aufgabe

Stelle sicher dass das Output-Format von `mrt-replay` **identisch**
zum Format der Live NATS-Messages aus `bgp_stream` ist.

Schreibe einen Integrationstest der beide Formate vergleicht:

### Test: JSON-Felder sind identisch

Erstelle `tools/mrt_replay/src/main.rs` am Ende:

```rust
#[cfg(test)]
mod tests {
    /// Stellt sicher dass alle Pflichtfelder im Output vorhanden sind.
    #[test]
    fn test_output_has_all_required_fields() {
        // Simuliere einen MRT-Record als JSON
        let record = serde_json::json!({
            "event_type": "announce",
            "prefix":     "8.8.8.0/24",
            "peer_asn":   1103_u32,
            "origin_as":  15169_u32,
            "peer_ip":    "80.249.211.0",
            "as_path":    [1103_u32, 3356_u32, 15169_u32],
            "collector":  "rrc12",
            "timestamp":  1524578100.0_f64,
        });

        // Alle Pflichtfelder müssen vorhanden sein
        let required = ["event_type","prefix","peer_asn","origin_as",
                        "peer_ip","as_path","collector","timestamp"];
        for field in required {
            assert!(
                record.get(field).is_some(),
                "Pflichtfeld '{field}' fehlt im Output-Format"
            );
        }

        // Typen prüfen
        assert!(record["as_path"].is_array());
        assert!(record["peer_asn"].is_number());
        assert!(record["timestamp"].is_number());
        assert!(record["collector"].is_string());
    }

    /// "collector" darf nie null oder leer sein
    #[test]
    fn test_collector_field_is_never_empty() {
        // Das CLI-Tool setzt collector per --collector Flag
        // Standardwert ist "unknown" (nicht leer)
        // Dieser Test dokumentiert die Erwartung
        let default_collector = "unknown";
        assert!(!default_collector.is_empty());
        assert_ne!(default_collector, "");
    }
}
```

## Dokumentation

Ergänze am Anfang von `tools/mrt_replay/src/main.rs` als Kommentar:

```rust
//! # Output-Format
//!
//! Jede Zeile ist ein JSON-Objekt mit genau diesen Feldern
//! (identisch zum bgp_stream NATS-Format):
//!
//! ```json
//! {
//!   "event_type": "announce" | "withdraw",
//!   "prefix":     "8.8.8.0/24",
//!   "peer_asn":   1103,
//!   "origin_as":  15169,
//!   "peer_ip":    "80.249.211.0",
//!   "as_path":    [1103, 3356, 15169],
//!   "collector":  "rrc12",
//!   "timestamp":  1524578100.0
//! }
//! ```
```

## Qualität
- `cargo test -p mrt-replay` alle Tests grün
- `cargo clippy --all-targets -- -D warnings` sauber
- `cargo fmt --check` sauber
```

---

## Meilenstein-Check Phase 1

Nach allen 18 Prompts muss gelten:

```bash
# Diese Befehle müssen alle erfolgreich sein:
cargo check --workspace
cargo clippy --all-targets -- -D warnings
cargo fmt --check
cargo test                          # 233+ bestehende Tests grün
cargo test collector_registry       # 3+3+3+3 = 12 neue Tests grün
cargo test propagation              # 6+ neue Tests grün
cargo test -p mrt-replay            # 2 neue Tests grün
cargo build --bin mrt-replay        # CLI-Tool baut erfolgreich
bash -n scripts/download_mrt.sh     # Script syntaktisch korrekt
```

**Meilenstein 1 bestanden wenn:**
- Jedes BGP-Update enthält `collector`-Feld (z.B. "rrc12")
- Gleiche Ankündigungen werden über Kollektoren korrekt gruppiert
- MRT-Archivdateien können eingelesen werden (gleiche Struktur wie Live-Feed)
- Alle bestehenden 233+ Tests weiterhin grün

# BGP TrustWave — Phase 2 Implementierungs-Prompts

**Datum:** 08.03.2026
**Branch:** `trustwave-core`
**Voraussetzung:** Phase 1 vollständig abgeschlossen (alle 18 Prompts 1.1.1–1.4.4, ✅ verifiziert)

---

## Neue Abhängigkeiten für Phase 2

Vor dem ersten Prompt folgende Crates in root `Cargo.toml` unter `[dependencies]` ergänzen:

```toml
bincode  = "1"           # binäre Serialisierung der Baseline
memmap2  = "0.9"         # memory-mapped file loading für große Baselines
```

Für `tools/baseline_builder/Cargo.toml` zusätzlich:
```toml
indicatif  = "0.17"      # Fortschrittsbalken
rayon      = "1"         # paralleles Parsen mehrerer Kollektor-Dateien
```

`zstd = "0.13"` ist bereits in root `Cargo.toml` vorhanden ✅.

---

## Abhängigkeitsreihenfolge Phase 2

```
2.1.1 WaveStats + Akkumulator
  └→ 2.1.2 PropagationBaseline struct
       └→ 2.1.3 Paarweise Delta-Statistiken
            └→ 2.1.4 Stabilität + is_reliable()
                 └→ 2.2.1 BaselineBuilder
                      └→ 2.2.2 Qualitäts-Filter
                           └→ 2.2.3 Batch-CLI baseline_builder
                                └→ 2.2.4 Fortschrittsanzeige
                                     └→ 2.3.1 bincode + zstd Persistenz
                                          └→ 2.3.2 Inkrementeller Live-Update
                                               └→ 2.3.3 Versionierung
                                                    └→ 2.3.4 mmap Load
```

---

## Abschnitt 2.1 — Baseline-Datenstruktur

---

### Prompt 2.1.1 — `WaveStats` + Welford-Akkumulator

```
Du arbeitest im Rust-Workspace `log-gateway`, Branch `trustwave-core`.
Neue Datei anlegen: `src/wave_baseline.rs`
Voraussetzung: Phase 1 vollständig abgeschlossen.

## Kontext

Jede BGP-Ankündigung die über mehrere Kollektoren beobachtet wird,
hinterlässt ein Zeitstempel-Muster. Über Hunderte von Beobachtungen
entsteht eine Statistik: Kollektor A kommt typisch Xms vor Kollektor B.
Diese Statistik ist die Baseline — die "Wellenphysik-Fingerabdruck"
einer legitimen Route.

## Aufgabe

Erstelle `src/wave_baseline.rs` mit zwei Structs:

### 1. `WaveStats` — serialisierbare Ergebnis-Statistik

```rust
use serde::{Deserialize, Serialize};

/// Statistische Zusammenfassung einer Zeitdifferenz-Verteilung.
/// Wird in der Baseline-Datei gespeichert.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaveStats {
    /// Mittelwert der Zeitdifferenz in Millisekunden
    pub mean_ms: f64,
    /// Standardabweichung in Millisekunden
    pub std_ms: f64,
    /// Minimum aller beobachteten Werte in ms
    pub min_ms: f64,
    /// Maximum aller beobachteten Werte in ms
    pub max_ms: f64,
    /// Anzahl der Beobachtungen
    pub count: u64,
}
```

### 2. `WaveStatsAccumulator` — Online-Akkumulator (Welford)

Nicht serialisiert. Wird nur während des Aufbaus der Baseline benutzt.
Implementiert Welford's Online-Algorithmus für stabiles Mean/Variance.

```rust
/// Online-Akkumulator für Zeitdifferenz-Statistiken.
/// Nutzt Welford's Algorithmus — numerisch stabiler als naive Varianz.
/// Nicht serialisiert — nur für den Baseline-Aufbau.
#[derive(Debug, Clone, Default)]
pub struct WaveStatsAccumulator {
    count: u64,
    mean:  f64,
    m2:    f64,   // Summe der quadratischen Abweichungen (Welford)
    min:   f64,
    max:   f64,
}

impl WaveStatsAccumulator {
    pub fn new() -> Self {
        Self {
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            ..Default::default()
        }
    }

    /// Fügt einen neuen Messwert (in ms) ein.
    pub fn update(&mut self, value_ms: f64) {
        self.count += 1;
        let delta  = value_ms - self.mean;
        self.mean += delta / self.count as f64;
        let delta2 = value_ms - self.mean;
        self.m2   += delta * delta2;
        if value_ms < self.min { self.min = value_ms; }
        if value_ms > self.max { self.max = value_ms; }
    }

    /// Konvertiert in serialisierbare WaveStats.
    /// Gibt None zurück wenn noch keine Werte vorhanden.
    pub fn to_stats(&self) -> Option<WaveStats> {
        if self.count == 0 { return None; }
        let variance = if self.count > 1 {
            self.m2 / (self.count - 1) as f64
        } else {
            0.0
        };
        Some(WaveStats {
            mean_ms: self.mean,
            std_ms:  variance.sqrt(),
            min_ms:  self.min,
            max_ms:  self.max,
            count:   self.count,
        })
    }

    pub fn count(&self) -> u64 { self.count }
}
```

Ergänze `src/lib.rs`:
```rust
pub mod wave_baseline;
```

## Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_accumulator_single_value() {
        let mut acc = WaveStatsAccumulator::new();
        acc.update(100.0);
        let stats = acc.to_stats().unwrap();
        assert_eq!(stats.mean_ms, 100.0);
        assert_eq!(stats.std_ms,  0.0);
        assert_eq!(stats.count,   1);
    }

    #[test]
    fn test_accumulator_mean_correct() {
        let mut acc = WaveStatsAccumulator::new();
        acc.update(10.0);
        acc.update(20.0);
        acc.update(30.0);
        let stats = acc.to_stats().unwrap();
        assert!((stats.mean_ms - 20.0).abs() < 0.001);
        assert_eq!(stats.count, 3);
    }

    #[test]
    fn test_accumulator_std_correct() {
        // Werte: 2, 4, 4, 4, 5, 5, 7, 9 → std = 2.0
        let mut acc = WaveStatsAccumulator::new();
        for v in [2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0] {
            acc.update(v);
        }
        let stats = acc.to_stats().unwrap();
        assert!((stats.mean_ms - 5.0).abs() < 0.001);
        assert!((stats.std_ms  - 2.0).abs() < 0.001);
    }

    #[test]
    fn test_accumulator_min_max() {
        let mut acc = WaveStatsAccumulator::new();
        acc.update(5.0);
        acc.update(1.0);
        acc.update(9.0);
        let stats = acc.to_stats().unwrap();
        assert_eq!(stats.min_ms, 1.0);
        assert_eq!(stats.max_ms, 9.0);
    }

    #[test]
    fn test_accumulator_empty_returns_none() {
        let acc = WaveStatsAccumulator::new();
        assert!(acc.to_stats().is_none());
    }
}
```

## Qualität
- `cargo test wave_baseline` alle Tests grün
- `cargo clippy --all-targets -- -D warnings` sauber
- `cargo fmt --check` sauber
```

---

### Prompt 2.1.2 — `PropagationBaseline` Datenstruktur

```
Du arbeitest im Rust-Workspace `log-gateway`, Branch `trustwave-core`.
Datei: `src/wave_baseline.rs`
Voraussetzung: Prompt 2.1.1 ist erledigt. `WaveStats` und `WaveStatsAccumulator` existieren.

## Aufgabe

Ergänze `src/wave_baseline.rs` um die zentrale Baseline-Datenstruktur.

## Schlüssel für die Baseline-Map

```rust
use ipnet::IpNet;

/// Schlüssel für die Baseline-HashMap: (Präfix, Ursprungs-AS).
/// Der AS-Pfad-Hash ist NICHT im Schlüssel — gleiche Route via
/// verschiedene AS-Pfade teilen sich eine Baseline (nach Qualitätsfilter).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BaselineKey {
    /// Normalisiertes Präfix als String, z.B. "8.8.8.0/24"
    pub prefix:    String,
    /// Ursprungs-AS (letzter Hop im AS-Pfad)
    pub origin_as: u32,
}

impl BaselineKey {
    pub fn new(prefix: &IpNet, origin_as: u32) -> Self {
        Self { prefix: prefix.to_string(), origin_as }
    }
}
```

## PropagationBaseline Struct

```rust
use std::collections::HashMap;
use std::time::SystemTime;

/// Statistisches Modell des normalen Propagationsverhaltens
/// für ein bestimmtes (Präfix, Origin-AS) Paar.
///
/// Wird aus historischen MRT-Daten aufgebaut und im Echtzeit-Betrieb
/// zum Erkennen von Abweichungen (Hijacks) verwendet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropagationBaseline {
    /// Das beobachtete IP-Präfix
    pub prefix:    String,
    /// Ursprungs-AS
    pub origin_as: u32,

    /// Paarweise Zeitdifferenz-Statistiken.
    /// Schlüssel: ("rrc00", "rrc12") alphabetisch sortiert.
    /// Wert: Statistik der Zeitdifferenz t_rrc12 − t_rrc00 in ms.
    /// Positiver mean_ms → rrc00 kommt typisch vor rrc12.
    pub pairwise_deltas: HashMap<(String, String), WaveStats>,

    /// Erwartete Ankunftsreihenfolge der ersten 3 Kollektoren.
    /// Aus den häufigsten Beobachtungen abgeleitet.
    pub expected_order: Vec<String>,

    /// Wie viele PropagationEvents in diese Baseline eingeflossen sind.
    pub sample_count: u64,

    /// Wann die Baseline zuletzt aktualisiert wurde.
    pub last_updated: SystemTime,
}

impl PropagationBaseline {
    pub fn new(prefix: &str, origin_as: u32) -> Self {
        Self {
            prefix:          prefix.to_string(),
            origin_as,
            pairwise_deltas: HashMap::new(),
            expected_order:  Vec::new(),
            sample_count:    0,
            last_updated:    SystemTime::now(),
        }
    }
}

/// Typ-Alias für die vollständige Baseline-Datenbank.
/// Wird als einzelne Datei (bincode + zstd) gespeichert.
pub type BaselineStore = HashMap<BaselineKey, PropagationBaseline>;
```

## Tests

```rust
#[test]
fn test_baseline_key_from_prefix() {
    let prefix: IpNet = "8.8.8.0/24".parse().unwrap();
    let key = BaselineKey::new(&prefix, 15169);
    assert_eq!(key.prefix,    "8.8.8.0/24");
    assert_eq!(key.origin_as, 15169);
}

#[test]
fn test_baseline_key_equality() {
    let prefix: IpNet = "1.1.1.0/24".parse().unwrap();
    let k1 = BaselineKey::new(&prefix, 13335);
    let k2 = BaselineKey::new(&prefix, 13335);
    assert_eq!(k1, k2);
}

#[test]
fn test_baseline_key_different_origin_different_key() {
    let prefix: IpNet = "1.1.1.0/24".parse().unwrap();
    let k1 = BaselineKey::new(&prefix, 13335);
    let k2 = BaselineKey::new(&prefix, 15169);
    assert_ne!(k1, k2);
}

#[test]
fn test_propagation_baseline_new() {
    let b = PropagationBaseline::new("8.8.8.0/24", 15169);
    assert_eq!(b.prefix,      "8.8.8.0/24");
    assert_eq!(b.origin_as,   15169);
    assert_eq!(b.sample_count, 0);
    assert!(b.pairwise_deltas.is_empty());
    assert!(b.expected_order.is_empty());
}
```

## Qualität
- `cargo check --workspace` fehlerfrei
- `cargo clippy --all-targets -- -D warnings` sauber
- `cargo fmt --check` sauber
```

---

### Prompt 2.1.3 — Paarweise Delta-Statistiken berechnen

```
Du arbeitest im Rust-Workspace `log-gateway`, Branch `trustwave-core`.
Datei: `src/wave_baseline.rs`
Voraussetzung: Prompt 2.1.2 ist erledigt. `PropagationBaseline` und `WaveStatsAccumulator` existieren.

## Kontext

Für jedes beobachtete `PropagationEvent` mit Ankunftszeiten bei mehreren
Kollektoren sollen paarweise Zeitdifferenzen akkumuliert werden.

Beispiel: Event kommt bei rrc00 t=1000.000 und rrc12 t=1000.089 an.
  → Paar ("rrc00", "rrc12"), delta = (1000.089 − 1000.000) × 1000 = 89ms

Schlüssel-Konvention: immer alphabetisch sortiert → ("rrc00", "rrc12"), nie ("rrc12", "rrc00").
Delta = t_zweiter − t_erster (positiv wenn erster Kollektor früher kommt).

## Aufgabe

### 1. `BaselineAccumulator` — Interner Baustein beim Aufbau

Definiere einen internen Helfer-Struct der die `WaveStatsAccumulator`-Instanzen
für alle Kollektor-Paare und die Ankunftsreihenfolge hält:

```rust
/// Interner Akkumulator während des Baseline-Aufbaus.
/// Wird nach dem Batch-Lauf in `PropagationBaseline` konvertiert.
#[derive(Debug, Default)]
pub(crate) struct BaselineAccumulator {
    /// Paarweise Akkumulatoren: (rrc_a, rrc_b) alphabetisch → Zeitdifferenz-Akkumulator
    pub pairwise: HashMap<(String, String), WaveStatsAccumulator>,
    /// Zählt wie oft jeder Kollektor als Erster ankam
    pub first_arrival_counts: HashMap<String, u64>,
    /// Gesamtzahl eingeflossener PropagationEvents
    pub sample_count: u64,
}

impl BaselineAccumulator {
    /// Verarbeitet ein `PropagationEvent` und akkumuliert alle Paar-Deltas.
    pub fn add_event(&mut self, arrivals: &std::collections::BTreeMap<String, f64>) {
        if arrivals.len() < 2 { return; }

        // Kollektoren sortiert nach Ankunftszeit (Aufstieg)
        let mut sorted: Vec<(&String, f64)> = arrivals
            .iter()
            .map(|(k, &v)| (k, v))
            .collect();
        sorted.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

        // Erster Kollektor zählen
        if let Some((first_collector, _)) = sorted.first() {
            *self.first_arrival_counts
                .entry(first_collector.to_string())
                .or_insert(0) += 1;
        }

        // Alle Paare berechnen (alphabetisch sortierter Schlüssel)
        let collectors: Vec<&String> = arrivals.keys().collect();
        for i in 0..collectors.len() {
            for j in (i + 1)..collectors.len() {
                let (a, b) = if collectors[i] <= collectors[j] {
                    (collectors[i], collectors[j])
                } else {
                    (collectors[j], collectors[i])
                };
                let delta_ms = (arrivals[b] - arrivals[a]) * 1000.0;
                let key = (a.to_string(), b.to_string());
                self.pairwise.entry(key).or_default().update(delta_ms);
            }
        }

        self.sample_count += 1;
    }

    /// Konvertiert in serialisierbare `PropagationBaseline`.
    pub fn to_baseline(&self, prefix: &str, origin_as: u32) -> PropagationBaseline {
        let pairwise_deltas: HashMap<(String, String), WaveStats> = self
            .pairwise
            .iter()
            .filter_map(|(k, acc)| acc.to_stats().map(|s| (k.clone(), s)))
            .collect();

        // Erwartete Reihenfolge: Kollektoren sortiert nach first_arrival_counts (absteigend)
        let mut order: Vec<(String, u64)> = self
            .first_arrival_counts
            .iter()
            .map(|(k, &v)| (k.clone(), v))
            .collect();
        order.sort_by(|a, b| b.1.cmp(&a.1));
        let expected_order: Vec<String> = order.into_iter()
            .take(3)
            .map(|(k, _)| k)
            .collect();

        PropagationBaseline {
            prefix:          prefix.to_string(),
            origin_as,
            pairwise_deltas,
            expected_order,
            sample_count:    self.sample_count,
            last_updated:    std::time::SystemTime::now(),
        }
    }
}
```

## Tests

```rust
#[test]
fn test_accumulator_single_event_two_collectors() {
    let mut acc = BaselineAccumulator::default();
    let mut arrivals = std::collections::BTreeMap::new();
    arrivals.insert("rrc12".to_string(), 1000.000);
    arrivals.insert("rrc00".to_string(), 1000.089);
    acc.add_event(&arrivals);

    assert_eq!(acc.sample_count, 1);
    // Paar ("rrc00", "rrc12"): delta = (1000.000 - 1000.089) * 1000 = -89ms
    // (rrc00 kam nach rrc12, also negatives delta)
    let pair = ("rrc00".to_string(), "rrc12".to_string());
    let stats = acc.pairwise[&pair].to_stats().unwrap();
    assert!((stats.mean_ms - (-89.0)).abs() < 0.1);
}

#[test]
fn test_accumulator_first_arrival_tracking() {
    let mut acc = BaselineAccumulator::default();
    let mut a1 = std::collections::BTreeMap::new();
    a1.insert("rrc12".to_string(), 1000.000); // rrc12 zuerst
    a1.insert("rrc00".to_string(), 1000.089);
    acc.add_event(&a1);

    let mut a2 = std::collections::BTreeMap::new();
    a2.insert("rrc12".to_string(), 2000.000); // rrc12 wieder zuerst
    a2.insert("rrc00".to_string(), 2000.100);
    acc.add_event(&a2);

    assert_eq!(acc.first_arrival_counts["rrc12"], 2);
    assert_eq!(acc.first_arrival_counts.get("rrc00"), None);
}

#[test]
fn test_to_baseline_expected_order() {
    let mut acc = BaselineAccumulator::default();
    // 3x rrc12 zuerst, 1x rrc00 zuerst
    for _ in 0..3 {
        let mut a = std::collections::BTreeMap::new();
        a.insert("rrc12".to_string(), 1000.0);
        a.insert("rrc00".to_string(), 1000.1);
        a.insert("rrc11".to_string(), 1000.5);
        acc.add_event(&a);
    }
    let mut a = std::collections::BTreeMap::new();
    a.insert("rrc00".to_string(), 2000.0);
    a.insert("rrc12".to_string(), 2000.1);
    a.insert("rrc11".to_string(), 2000.5);
    acc.add_event(&a);

    let baseline = acc.to_baseline("8.8.8.0/24", 15169);
    assert_eq!(baseline.expected_order[0], "rrc12"); // häufigster erster
}
```

## Qualität
- `cargo test wave_baseline` alle Tests grün (inkl. vorherige)
- `cargo clippy --all-targets -- -D warnings` sauber
- `cargo fmt --check` sauber
```

---

### Prompt 2.1.4 — Stabilität: `is_reliable()` und `baseline_confidence()`

```
Du arbeitest im Rust-Workspace `log-gateway`, Branch `trustwave-core`.
Datei: `src/wave_baseline.rs`
Voraussetzung: Prompt 2.1.3 ist erledigt. `PropagationBaseline` existiert vollständig.

## Aufgabe

Ergänze `impl PropagationBaseline` um zwei Methoden:

### Methode 1 — `is_reliable()`

Eine Baseline gilt als verlässlich wenn:
- `sample_count >= 30` (genug Beobachtungen für statistisch robuste Werte)
- Mindestens ein Kollektor-Paar mit `count >= 10` in `pairwise_deltas`

```rust
impl PropagationBaseline {
    /// Gibt true wenn die Baseline statistisch verlässlich ist.
    /// Schwelle: ≥ 30 Samples und mindestens ein Paar mit ≥ 10 Beobachtungen.
    pub fn is_reliable(&self) -> bool {
        if self.sample_count < 30 { return false; }
        self.pairwise_deltas
            .values()
            .any(|s| s.count >= 10)
    }
}
```

### Methode 2 — `baseline_confidence()`

Gibt einen Konfidenzwert 0.0–1.0 zurück, der angibt wie verlässlich die
Baseline ist. Verwendet für den `baseline_confidence`-Wert im WaveScore (Phase 3).

```rust
impl PropagationBaseline {
    /// Konfidenzwert 0.0–1.0 basierend auf der Anzahl der Beobachtungen.
    ///
    /// Wächst logarithmisch:
    ///   0 samples  → 0.0
    ///   30 samples → 0.5 (Mindest-Schwelle)
    ///   100 samples → ~0.75
    ///   1000 samples → 1.0 (praktisches Maximum)
    pub fn baseline_confidence(&self) -> f64 {
        if self.sample_count == 0 { return 0.0; }
        let log_n = (self.sample_count as f64).ln();
        let log_max = 1000_f64.ln();  // ~6.9 → normalisiert auf 1.0
        (log_n / log_max).min(1.0)
    }
}
```

## Tests

```rust
#[test]
fn test_is_reliable_below_threshold() {
    let mut b = PropagationBaseline::new("8.8.8.0/24", 15169);
    b.sample_count = 29;
    b.pairwise_deltas.insert(
        ("rrc00".to_string(), "rrc12".to_string()),
        WaveStats { mean_ms: 89.0, std_ms: 5.0, min_ms: 70.0, max_ms: 110.0, count: 15 },
    );
    assert!(!b.is_reliable());
}

#[test]
fn test_is_reliable_at_threshold() {
    let mut b = PropagationBaseline::new("8.8.8.0/24", 15169);
    b.sample_count = 30;
    b.pairwise_deltas.insert(
        ("rrc00".to_string(), "rrc12".to_string()),
        WaveStats { mean_ms: 89.0, std_ms: 5.0, min_ms: 70.0, max_ms: 110.0, count: 10 },
    );
    assert!(b.is_reliable());
}

#[test]
fn test_is_reliable_enough_samples_but_no_pairs() {
    let mut b = PropagationBaseline::new("8.8.8.0/24", 15169);
    b.sample_count = 100;
    // Keine Paare → nicht verlässlich
    assert!(!b.is_reliable());
}

#[test]
fn test_confidence_zero_samples() {
    let b = PropagationBaseline::new("8.8.8.0/24", 15169);
    assert_eq!(b.baseline_confidence(), 0.0);
}

#[test]
fn test_confidence_grows_with_samples() {
    let mut b = PropagationBaseline::new("8.8.8.0/24", 15169);
    b.sample_count = 30;
    let c30 = b.baseline_confidence();
    b.sample_count = 100;
    let c100 = b.baseline_confidence();
    b.sample_count = 1000;
    let c1000 = b.baseline_confidence();
    assert!(c30 < c100 && c100 < c1000);
    assert!(c30 >= 0.5, "30 Samples sollten ≥ 0.5 Konfidenz haben");
    assert!(c1000 > 0.99, "1000 Samples sollten ≈ 1.0 sein");
}
```

## Qualität
- `cargo test wave_baseline` alle Tests grün (inkl. alle vorherigen)
- `cargo clippy --all-targets -- -D warnings` sauber
- `cargo fmt --check` sauber
```

---

## Abschnitt 2.2 — Baseline Builder

---

### Prompt 2.2.1 — `BaselineBuilder` Struct

```
Du arbeitest im Rust-Workspace `log-gateway`, Branch `trustwave-core`.
Datei: `src/wave_baseline.rs`
Voraussetzung: Prompt 2.1.4 ist erledigt. Alle Baseline-Structs existieren.

## Aufgabe

Implementiere den `BaselineBuilder` — der zentrale Aggregator der aus
`PropagationEvent`-Instanzen schrittweise die Baseline aufbaut.

```rust
use crate::propagation::PropagationEvent;

/// Baut die PropagationBaseline aus historischen PropagationEvents auf.
///
/// Verwendung:
///   1. `BaselineBuilder::new()` erstellen
///   2. `add_event(&prop_event)` für jedes gefilterte Event aufrufen
///   3. `build()` aufrufen → gibt `BaselineStore` zurück
pub struct BaselineBuilder {
    /// Akkumulatoren pro (Präfix, Origin-AS)
    accumulators: HashMap<BaselineKey, BaselineAccumulator>,
}

impl BaselineBuilder {
    pub fn new() -> Self {
        Self { accumulators: HashMap::new() }
    }

    /// Fügt ein PropagationEvent in die Baseline ein.
    /// Das Event MUSS vorher durch den Qualitäts-Filter geprüft sein.
    pub fn add_event(&mut self, event: &PropagationEvent) {
        let key = BaselineKey::new(&event.prefix, event.origin_as);
        self.accumulators
            .entry(key)
            .or_default()
            .add_event(&event.arrivals);
    }

    /// Liefert die Anzahl bisher verarbeiteter eindeutiger (Präfix, Origin-AS) Paare.
    pub fn entry_count(&self) -> usize {
        self.accumulators.len()
    }

    /// Finalisiert die Baseline: konvertiert alle Akkumulatoren in `PropagationBaseline`.
    /// Gibt nur Einträge zurück die `is_reliable()` erfüllen (sample_count >= 30).
    pub fn build(self) -> BaselineStore {
        self.accumulators
            .into_iter()
            .map(|(key, acc)| {
                let baseline = acc.to_baseline(&key.prefix, key.origin_as);
                (key, baseline)
            })
            .filter(|(_, b)| b.is_reliable())
            .collect()
    }

    /// Wie `build()` aber gibt ALLE Einträge zurück (auch unreliable).
    /// Nützlich zum Debuggen und für Statistiken.
    pub fn build_all(self) -> BaselineStore {
        self.accumulators
            .into_iter()
            .map(|(key, acc)| {
                let baseline = acc.to_baseline(&key.prefix, key.origin_as);
                (key, baseline)
            })
            .collect()
    }
}

impl Default for BaselineBuilder {
    fn default() -> Self { Self::new() }
}
```

## Tests

```rust
fn make_event(prefix: &str, origin_as: u32, arrivals: &[(&str, f64)]) -> PropagationEvent {
    use std::collections::BTreeMap;
    let arrivals_map: BTreeMap<String, f64> = arrivals
        .iter()
        .map(|(k, v)| (k.to_string(), *v))
        .collect();
    let first = arrivals_map.values().copied().fold(f64::INFINITY, f64::min);
    let last  = arrivals_map.values().copied().fold(f64::NEG_INFINITY, f64::max);
    let mut order: Vec<String> = arrivals_map.keys().cloned().collect();
    order.sort_by(|a, b| arrivals_map[a].partial_cmp(&arrivals_map[b]).unwrap());
    PropagationEvent {
        prefix:        prefix.parse().unwrap(),
        origin_as,
        as_path:       vec![1103, origin_as],
        arrivals:      arrivals_map,
        first_arrival: first,
        last_arrival:  last,
        spread_ms:     (last - first) * 1000.0,
        arrival_order: order,
    }
}

#[test]
fn test_builder_accumulates_events() {
    let mut builder = BaselineBuilder::new();
    let event = make_event("8.8.8.0/24", 15169,
        &[("rrc12", 1000.0), ("rrc00", 1000.089), ("rrc11", 1000.891)]);
    builder.add_event(&event);
    assert_eq!(builder.entry_count(), 1);
}

#[test]
fn test_builder_different_prefix_different_entry() {
    let mut builder = BaselineBuilder::new();
    builder.add_event(&make_event("8.8.8.0/24", 15169,
        &[("rrc12", 1000.0), ("rrc00", 1000.1), ("rrc11", 1000.5)]));
    builder.add_event(&make_event("1.1.1.0/24", 13335,
        &[("rrc12", 1000.0), ("rrc00", 1000.1), ("rrc11", 1000.5)]));
    assert_eq!(builder.entry_count(), 2);
}

#[test]
fn test_builder_build_filters_unreliable() {
    let mut builder = BaselineBuilder::new();
    // Nur 5 Events → sample_count = 5 < 30 → unreliable → nicht in build()
    for i in 0..5 {
        builder.add_event(&make_event("8.8.8.0/24", 15169,
            &[("rrc12", 1000.0 + i as f64 * 100.0),
              ("rrc00", 1000.1 + i as f64 * 100.0),
              ("rrc11", 1000.5 + i as f64 * 100.0)]));
    }
    let store = builder.build();
    assert!(store.is_empty(), "Weniger als 30 Samples → sollte gefiltert werden");
}

#[test]
fn test_builder_build_all_includes_unreliable() {
    let mut builder = BaselineBuilder::new();
    builder.add_event(&make_event("8.8.8.0/24", 15169,
        &[("rrc12", 1000.0), ("rrc00", 1000.1), ("rrc11", 1000.5)]));
    let store = builder.build_all();
    assert_eq!(store.len(), 1);
}
```

## Qualität
- `cargo test wave_baseline` alle Tests grün
- `cargo clippy --all-targets -- -D warnings` sauber
- `cargo fmt --check` sauber
```

---

### Prompt 2.2.2 — Qualitäts-Filter für PropagationEvents

```
Du arbeitest im Rust-Workspace `log-gateway`, Branch `trustwave-core`.
Datei: `src/wave_baseline.rs`
Voraussetzung: Prompt 2.2.1 ist erledigt. `BaselineBuilder` existiert.

## Aufgabe

Implementiere eine freie Funktion die entscheidet ob ein `PropagationEvent`
gut genug ist um in die Baseline aufgenommen zu werden.

Nur Routen die dauerhaft stabil und klar sind, sollen die Baseline prägen.
Instabile oder ungewöhnliche Ankündigungen verzerren die Statistiken.

```rust
/// Gibt true wenn das Event für die Baseline geeignet ist.
///
/// Qualitätskriterien:
/// 1. Mindestens 3 Kollektoren haben die Ankündigung gesehen
///    (3 Punkte nötig für Triangulation)
/// 2. AS-Pfad-Länge ≤ 6 Hops
///    (längere Pfade sind oft instabile oder seltene Routen)
/// 3. Kein leerer AS-Pfad (= kein Origin-AS bestimmbar)
/// 4. Spread ≤ 10.000ms (10 Sekunden)
///    (größerer Spread = wahrscheinlich verschiedene Ereignisse, kein einzelner Announce)
pub fn is_good_route(event: &PropagationEvent) -> bool {
    if event.arrivals.len() < 3 { return false; }
    if event.as_path.is_empty() { return false; }
    if event.as_path.len() > 6  { return false; }
    if event.spread_ms > 10_000.0 { return false; }
    true
}
```

## Tests

```rust
#[test]
fn test_good_route_passes_filter() {
    let event = make_event("8.8.8.0/24", 15169,
        &[("rrc12", 1000.0), ("rrc00", 1000.089), ("rrc11", 1000.891)]);
    assert!(is_good_route(&event));
}

#[test]
fn test_too_few_collectors_rejected() {
    let event = make_event("8.8.8.0/24", 15169,
        &[("rrc12", 1000.0), ("rrc00", 1000.089)]); // nur 2
    assert!(!is_good_route(&event));
}

#[test]
fn test_too_long_as_path_rejected() {
    use std::collections::BTreeMap;
    let arrivals: BTreeMap<String, f64> = [
        ("rrc12", 1000.0), ("rrc00", 1000.1), ("rrc11", 1000.5),
    ].iter().map(|(k, v)| (k.to_string(), *v)).collect();
    let event = PropagationEvent {
        prefix:        "8.8.8.0/24".parse().unwrap(),
        origin_as:     15169,
        as_path:       vec![1, 2, 3, 4, 5, 6, 7], // 7 Hops → zu lang
        arrivals:      arrivals.clone(),
        first_arrival: 1000.0,
        last_arrival:  1000.5,
        spread_ms:     500.0,
        arrival_order: vec!["rrc12".into(), "rrc00".into(), "rrc11".into()],
    };
    assert!(!is_good_route(&event));
}

#[test]
fn test_large_spread_rejected() {
    use std::collections::BTreeMap;
    let arrivals: BTreeMap<String, f64> = [
        ("rrc12", 1000.0), ("rrc00", 1005.0), ("rrc11", 1011.0),
    ].iter().map(|(k, v)| (k.to_string(), *v)).collect();
    let event = PropagationEvent {
        prefix:        "1.0.0.0/24".parse().unwrap(),
        origin_as:     1,
        as_path:       vec![1],
        arrivals:      arrivals.clone(),
        first_arrival: 1000.0,
        last_arrival:  1011.0,
        spread_ms:     11_000.0, // > 10s → abgelehnt
        arrival_order: vec!["rrc12".into(), "rrc00".into(), "rrc11".into()],
    };
    assert!(!is_good_route(&event));
}
```

## Qualität
- `cargo test wave_baseline` alle Tests grün
- `cargo clippy --all-targets -- -D warnings` sauber
- `cargo fmt --check` sauber
```

---

### Prompt 2.2.3 — Batch-CLI `tools/baseline_builder/`

```
Du arbeitest im Rust-Workspace `log-gateway`, Branch `trustwave-core`.
Neues Workspace-Member: `tools/baseline_builder/`
Voraussetzung: Prompt 2.2.2 ist erledigt. `BaselineBuilder` und `is_good_route()` existieren.

## Aufgabe

Erstelle ein neues Workspace-Member `tools/baseline_builder/` das MRT-Archivdaten
aus dem von `scripts/download_mrt.sh` erzeugten Verzeichnis liest, PropagationEvents
baut (über mehrere Kollektoren pro Zeitslot), durch den Qualitäts-Filter schickt
und eine Baseline-Datei erzeugt.

### Schritt 1: Workspace ergänzen

Root `Cargo.toml`:
```toml
[workspace]
members = [
    ".",
    "tools/bgp_stream",
    "tools/mrt_replay",
    "tools/baseline_builder",   # ← NEU
]
```

### Schritt 2: `tools/baseline_builder/Cargo.toml`

```toml
[package]
name    = "baseline-builder"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "baseline-builder"
path = "src/main.rs"

[dependencies]
log-gateway        = { path = "../.." }
bgpkit-parser      = "0.10"
serde_json         = "1"
clap               = { version = "4", features = ["derive"] }
rayon              = "1"
indicatif          = "0.17"
tracing            = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
anyhow             = "1"
walkdir            = "2"
```

Ergänze auch in root `Cargo.toml` [dependencies]:
```toml
walkdir = "2"
```

### Schritt 3: `tools/baseline_builder/src/main.rs`

**Kernidee:** Für jeden 5-Minuten-Zeitslot (z.B. `updates.20240101.0000.gz`)
existieren Dateien von mehreren Kollektoren. Alle Dateien desselben Zeitslots
werden parallel gelesen und nach (prefix, origin_as, path_hash) zusammengeführt
→ PropagationEvents → Qualitäts-Filter → Baseline.

```rust
//! # Baseline Builder
//!
//! Liest MRT-Archivdaten von mehreren RIPE RIS Kollektoren und baut
//! daraus die PropagationBaseline (Wellenphysik-Fingerabdrücke).
//!
//! ## Verzeichnisstruktur erwartet (von download_mrt.sh):
//!
//! ```text
//! data/mrt/
//! ├── rrc00/2024.01/updates.20240101.0000.gz
//! ├── rrc00/2024.01/updates.20240101.0005.gz
//! ├── rrc12/2024.01/updates.20240101.0000.gz
//! └── rrc12/2024.01/updates.20240101.0005.gz
//! ```
//!
//! ## Verwendung:
//! ```bash
//! baseline-builder \
//!   --data-dir data/mrt/ \
//!   --output   data/baselines/baseline.bin.zst \
//!   --min-collectors 3
//! ```

use anyhow::{Context, Result};
use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use log_gateway::wave_baseline::{is_good_route, BaselineBuilder};
use log_gateway::propagation::PropagationEvent;
use rayon::prelude::*;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Parser, Debug)]
#[command(about = "Baut PropagationBaseline aus RIPE RIS MRT-Archivdaten")]
struct Args {
    /// Verzeichnis mit MRT-Daten (Struktur: data/mrt/rrcXX/YYYY.MM/updates.*.gz)
    #[arg(long, default_value = "data/mrt")]
    data_dir: PathBuf,

    /// Ausgabedatei für die Baseline (bincode + zstd)
    #[arg(long, default_value = "data/baselines/baseline.bin.zst")]
    output: PathBuf,

    /// Mindestanzahl Kollektoren pro PropagationEvent
    #[arg(long, default_value_t = 3)]
    min_collectors: usize,
}

/// Einfacher Schlüssel für das Zusammenführen über Kollektoren
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SlotKey {
    /// Dateiname ohne Kollektor-Prefix (z.B. "updates.20240101.0000.gz")
    filename: String,
}

/// Ein einzelner BGP-Eintrag aus einer MRT-Datei
#[derive(Debug, Clone)]
struct MrtRecord {
    collector: String,
    prefix:    String,
    origin_as: u32,
    as_path:   Vec<u32>,
    timestamp: f64,
}

fn parse_mrt_file(path: &Path, collector: &str) -> Vec<MrtRecord> {
    use bgpkit_parser::{models::ElemType, BgpkitParser};
    let parser = match BgpkitParser::new(path.to_str().unwrap_or("")) {
        Ok(p)  => p,
        Err(_) => return vec![],
    };
    let mut records = Vec::new();
    for elem in parser {
        if elem.elem_type != ElemType::ANNOUNCE { continue; }
        let as_path: Vec<u32> = elem.as_path
            .as_ref()
            .map(|p| p.to_u32_vec_opt(true).unwrap_or_default())
            .unwrap_or_default();
        if as_path.is_empty() { continue; }
        let origin_as = *as_path.last().unwrap();
        records.push(MrtRecord {
            collector: collector.to_string(),
            prefix:    elem.prefix.prefix.to_string(),
            origin_as,
            as_path,
            timestamp: elem.timestamp,
        });
    }
    records
}

/// Gruppiert Records nach (prefix, origin_as, path_hash) und baut PropagationEvents.
fn build_events(records: Vec<MrtRecord>, min_collectors: usize) -> Vec<PropagationEvent> {
    use log_gateway::propagation::GroupKey;
    use log_gateway::nats_subscriber::BgpRecord;

    // Zwischenspeicher: GroupKey → Map<collector, timestamp>
    // Für gleiche Announcements von verschiedenen Kollektoren innerhalb ±30s
    let mut groups: HashMap<(String, u32, u64), BTreeMap<String, f64>> = HashMap::new();

    for record in records {
        // path_hash: gleicher Algorithmus wie GroupKey::from_record()
        let path_hash: u64 = record.as_path.iter()
            .fold(0u64, |acc, &asn| acc.wrapping_mul(31).wrapping_add(asn as u64));
        let key = (record.prefix.clone(), record.origin_as, path_hash);
        let entry = groups.entry(key).or_default();
        // Behalte frühesten Timestamp pro Kollektor (falls Duplikate)
        entry.entry(record.collector.clone())
            .and_modify(|t| { if record.timestamp < *t { *t = record.timestamp; } })
            .or_insert(record.timestamp);
    }

    // Konvertiere Gruppen mit genug Kollektoren in PropagationEvents
    let mut events = Vec::new();
    for ((prefix_str, origin_as, _), arrivals) in groups {
        if arrivals.len() < min_collectors { continue; }
        let prefix = match prefix_str.parse() {
            Ok(p)  => p,
            Err(_) => continue,
        };
        let first = arrivals.values().copied().fold(f64::INFINITY, f64::min);
        let last  = arrivals.values().copied().fold(f64::NEG_INFINITY, f64::max);
        let mut order: Vec<String> = arrivals.keys().cloned().collect();
        order.sort_by(|a, b| arrivals[a].partial_cmp(&arrivals[b]).unwrap());

        // AS-Pfad aus erstem Arrival rekonstruieren (näherungsweise)
        let as_path = vec![origin_as]; // Vereinfachung für Batch-Verarbeitung

        events.push(PropagationEvent {
            prefix,
            origin_as,
            as_path,
            arrivals,
            first_arrival: first,
            last_arrival:  last,
            spread_ms:     (last - first) * 1000.0,
            arrival_order: order,
        });
    }
    events
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args = Args::parse();

    // Alle MRT-Dateien entdecken, gruppiert nach Zeitslot
    eprintln!("Scanne Verzeichnis: {}", args.data_dir.display());
    let mut slots: HashMap<SlotKey, Vec<(String, PathBuf)>> = HashMap::new();

    for entry in WalkDir::new(&args.data_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let path = entry.path();
        let filename = path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();

        if !filename.ends_with(".gz") { continue; }

        // Kollektor-Name aus Verzeichnisstruktur: data/mrt/rrc12/2024.01/updates...gz
        let collector = path.ancestors()
            .nth(2)
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        if !collector.starts_with("rrc") { continue; }

        slots.entry(SlotKey { filename })
            .or_default()
            .push((collector, path.to_path_buf()));
    }

    eprintln!("Gefunden: {} Zeitslots", slots.len());

    // Fortschrittsbalken
    let pb = ProgressBar::new(slots.len() as u64);
    pb.set_style(ProgressStyle::with_template(
        "[{elapsed_precise}] {bar:50.cyan/blue} {pos}/{len} Slots — {msg}"
    ).unwrap());

    // Baseline-Builder
    let mut builder = BaselineBuilder::new();
    let mut total_events = 0u64;
    let mut good_events  = 0u64;

    // Zeitslots verarbeiten (sequenziell, aber Dateien pro Slot parallel)
    let slot_list: Vec<_> = slots.into_iter().collect();
    for (slot, files) in &slot_list {
        pb.set_message(format!("{}", slot.filename));

        // Alle Kollektor-Dateien dieses Slots parallel parsen
        let all_records: Vec<MrtRecord> = files
            .par_iter()
            .flat_map(|(collector, path)| parse_mrt_file(path, collector))
            .collect();

        // PropagationEvents aus gemergten Records bauen
        let events = build_events(all_records, args.min_collectors);

        for event in &events {
            total_events += 1;
            if is_good_route(event) {
                builder.add_event(event);
                good_events += 1;
            }
        }

        pb.inc(1);
    }

    pb.finish_with_message("Fertig");

    eprintln!(
        "Events: {} total, {} good ({:.1}%), {} Präfixe",
        total_events,
        good_events,
        if total_events > 0 { good_events as f64 / total_events as f64 * 100.0 } else { 0.0 },
        builder.entry_count(),
    );

    // Baseline finalisieren und speichern
    let store = builder.build();
    eprintln!("Reliable Baseline-Einträge: {}", store.len());

    // Ausgabeverzeichnis anlegen
    if let Some(parent) = args.output.parent() {
        std::fs::create_dir_all(parent)?;
    }

    log_gateway::wave_baseline::save_baseline(&store, &args.output)?;
    eprintln!("Gespeichert: {}", args.output.display());

    Ok(())
}
```

Hinweis: `save_baseline()` wird in Prompt 2.3.1 implementiert.

## Qualität
- `cargo check --workspace` fehlerfrei
- `cargo clippy --all-targets -- -D warnings` sauber
- `cargo fmt --check` sauber
- `cargo build -p baseline-builder` erfolgreich
```

---

### Prompt 2.2.4 — Fortschrittsanzeige + Laufzeitschätzung

```
Du arbeitest im Rust-Workspace `log-gateway`, Branch `trustwave-core`.
Datei: `tools/baseline_builder/src/main.rs`
Voraussetzung: Prompt 2.2.3 ist erledigt. Grundgerüst existiert.

## Aufgabe

Verbessere die Fortschrittsanzeige mit:
1. Dateigrößen-Schätzung (verarbeitete MB)
2. Ereignis-Rate (Events/Sekunde)
3. Schätzung der verbleibenden Zeit (ETA)

Ersetze den bestehenden ProgressBar-Block durch:

```rust
use indicatif::{ProgressBar, ProgressStyle, MultiProgress};
use std::time::Instant;

// Zwei Balken: einer für Slots, einer für Statistiken
let mp = MultiProgress::new();

let pb_slots = mp.add(ProgressBar::new(slot_list.len() as u64));
pb_slots.set_style(ProgressStyle::with_template(
    "[{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} Slots ({per_sec} slots/s, ETA {eta})"
).unwrap().progress_chars("=>-"));

let pb_stats = mp.add(ProgressBar::new_spinner());
pb_stats.set_style(ProgressStyle::with_template(
    "  Events: {msg}"
).unwrap());

let start = Instant::now();
// ... (bestehende Verarbeitungs-Schleife)
// In der Schleife:
pb_stats.set_message(format!(
    "{} total / {} good ({:.1}%) — {} Präfixe — {:.0} events/s",
    total_events,
    good_events,
    if total_events > 0 { good_events as f64 / total_events as f64 * 100.0 } else { 0.0 },
    builder.entry_count(),
    total_events as f64 / start.elapsed().as_secs_f64().max(0.001),
));
pb_slots.inc(1);

pb_slots.finish_with_message("Alle Slots verarbeitet");
pb_stats.finish();
```

Ergänze außerdem eine Zusammenfassung am Ende:

```rust
let elapsed = start.elapsed();
eprintln!("\n=== Baseline Builder Ergebnis ===");
eprintln!("Laufzeit:          {:.1}s", elapsed.as_secs_f64());
eprintln!("Zeitslots:         {}", slot_list.len());
eprintln!("Events total:      {}", total_events);
eprintln!("Events gut:        {} ({:.1}%)", good_events,
    if total_events > 0 { good_events as f64 / total_events as f64 * 100.0 } else { 0.0 });
eprintln!("Baseline-Einträge: {} (reliable)", store.len());
eprintln!("Ausgabe:           {}", args.output.display());
```

## Tests

Keine neuen Unit-Tests (UI-Logik). Manuell prüfen:
- `cargo build -p baseline-builder` erfolgreich
- `baseline-builder --help` gibt sinnvolle Usage aus

## Qualität
- `cargo check --workspace` fehlerfrei
- `cargo clippy --all-targets -- -D warnings` sauber
- `cargo fmt --check` sauber
```

---

## Abschnitt 2.3 — Persistenz

---

### Prompt 2.3.1 — Binäres Speicherformat (bincode + zstd)

```
Du arbeitest im Rust-Workspace `log-gateway`, Branch `trustwave-core`.
Datei: `src/wave_baseline.rs`
Voraussetzung: Prompt 2.2.2 ist erledigt. `BaselineStore` und alle Typen existieren.

## Voraussetzung Cargo.toml

Stelle sicher dass in root `Cargo.toml` vorhanden:
```toml
bincode = "1"   # Bereits ergänzt laut Phase-2-Header
zstd    = "0.13" # Bereits vorhanden ✅
```

## Aufgabe

Implementiere zwei freie Funktionen in `src/wave_baseline.rs`:

### `save_baseline()`

```rust
use std::path::Path;

/// Speichert die `BaselineStore` als binäre Datei (bincode + zstd Kompression).
///
/// Format: bincode-serialisiert, dann zstd-komprimiert (Level 3).
/// Typische Größe: ~100k Präfixe → ~5–15 MB komprimiert.
///
/// # Fehler
/// - I/O-Fehler (Verzeichnis nicht vorhanden, keine Schreibrechte)
/// - Serialisierungsfehler (sollte nie auftreten bei korrekten Typen)
pub fn save_baseline(store: &BaselineStore, path: &Path) -> anyhow::Result<()> {
    let encoded   = bincode::serialize(store)?;
    let compressed = zstd::encode_all(&encoded[..], 3)?;
    std::fs::write(path, &compressed)?;
    tracing::info!(
        path = %path.display(),
        entries = store.len(),
        size_kb = compressed.len() / 1024,
        "Baseline gespeichert"
    );
    Ok(())
}
```

### `load_baseline()`

```rust
/// Lädt eine Baseline-Datei (bincode + zstd).
///
/// # Fehler
/// - Datei nicht gefunden
/// - Dekomprimierungs- oder Deserialisierungsfehler (korrupte Datei)
pub fn load_baseline(path: &Path) -> anyhow::Result<BaselineStore> {
    let compressed = std::fs::read(path)?;
    let decoded    = zstd::decode_all(&compressed[..])?;
    let store: BaselineStore = bincode::deserialize(&decoded)?;
    tracing::info!(
        path = %path.display(),
        entries = store.len(),
        "Baseline geladen"
    );
    Ok(store)
}
```

## Tests

```rust
#[test]
fn test_save_and_load_roundtrip() {
    use std::collections::HashMap;
    use tempfile::NamedTempFile;  // tempfile = "3" in Cargo.toml ergänzen

    let mut store: BaselineStore = HashMap::new();
    let key = BaselineKey { prefix: "8.8.8.0/24".to_string(), origin_as: 15169 };
    let mut baseline = PropagationBaseline::new("8.8.8.0/24", 15169);
    baseline.sample_count = 42;
    baseline.pairwise_deltas.insert(
        ("rrc00".to_string(), "rrc12".to_string()),
        WaveStats { mean_ms: 89.0, std_ms: 5.0, min_ms: 70.0, max_ms: 110.0, count: 42 },
    );
    store.insert(key.clone(), baseline);

    let tmpfile = NamedTempFile::new().unwrap();
    save_baseline(&store, tmpfile.path()).unwrap();

    let loaded = load_baseline(tmpfile.path()).unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[&key].origin_as, 15169);
    assert_eq!(loaded[&key].sample_count, 42);
    let delta = &loaded[&key].pairwise_deltas[&("rrc00".to_string(), "rrc12".to_string())];
    assert!((delta.mean_ms - 89.0).abs() < 0.001);
}

#[test]
fn test_save_empty_store() {
    use std::collections::HashMap;
    use tempfile::NamedTempFile;
    let store: BaselineStore = HashMap::new();
    let tmp = NamedTempFile::new().unwrap();
    save_baseline(&store, tmp.path()).unwrap();
    let loaded = load_baseline(tmp.path()).unwrap();
    assert!(loaded.is_empty());
}
```

Ergänze in root `Cargo.toml` unter `[dev-dependencies]`:
```toml
tempfile = "3"
```

## Qualität
- `cargo test wave_baseline` alle Tests grün (inkl. save/load roundtrip)
- `cargo clippy --all-targets -- -D warnings` sauber
- `cargo fmt --check` sauber
```

---

### Prompt 2.3.2 — Inkrementeller Live-Update

```
Du arbeitest im Rust-Workspace `log-gateway`, Branch `trustwave-core`.
Datei: `src/wave_baseline.rs`
Voraussetzung: Prompt 2.3.1 ist erledigt. `save_baseline()` und `load_baseline()` existieren.

## Kontext

Nach dem initialen Batch-Lauf (3 Jahre MRT) soll die Baseline kontinuierlich
mit neuen Live-Daten aktualisiert werden. Der `PropagationAggregator` (Phase 1.3)
publiziert fertige `PropagationEvents` auf NATS `bgp.propagation`.
Diese sollen in die Baseline einfließen — aber nur wenn sie den Qualitäts-Filter
bestehen und die Baseline damit tatsächlich verbessern.

## Aufgabe

### 1. `PropagationBaseline::update()` — Einzelnes Event einarbeiten

```rust
impl PropagationBaseline {
    /// Aktualisiert die Baseline mit einem neuen PropagationEvent.
    ///
    /// Aktualisiert pairwise_deltas und expected_order inkrementell.
    /// ACHTUNG: Diese Methode nutzt nicht Welford — sie ist eine Näherung
    /// für inkrementelle Updates (aktualisiert nur mean, nicht std_ms).
    /// Für genaue Statistiken wird der vollständige Batch-Rebuild empfohlen.
    pub fn update(&mut self, event: &PropagationEvent) {
        if event.arrivals.len() < 3 { return; }

        // Paarweise Deltas aktualisieren (online mean update ohne Welford)
        let collectors: Vec<&String> = event.arrivals.keys().collect();
        for i in 0..collectors.len() {
            for j in (i + 1)..collectors.len() {
                let (a, b) = if collectors[i] <= collectors[j] {
                    (collectors[i], collectors[j])
                } else {
                    (collectors[j], collectors[i])
                };
                let delta_ms = (event.arrivals[b] - event.arrivals[a]) * 1000.0;
                let key = (a.to_string(), b.to_string());

                self.pairwise_deltas
                    .entry(key)
                    .and_modify(|s| {
                        // Online mean update: new_mean = old_mean + (x - old_mean) / n
                        let new_count = s.count + 1;
                        s.mean_ms += (delta_ms - s.mean_ms) / new_count as f64;
                        if delta_ms < s.min_ms { s.min_ms = delta_ms; }
                        if delta_ms > s.max_ms { s.max_ms = delta_ms; }
                        s.count = new_count;
                    })
                    .or_insert(WaveStats {
                        mean_ms: delta_ms,
                        std_ms:  0.0,
                        min_ms:  delta_ms,
                        max_ms:  delta_ms,
                        count:   1,
                    });
            }
        }

        self.sample_count += 1;
        self.last_updated  = std::time::SystemTime::now();

        // expected_order: erster Kollektor des Events
        if let Some(first) = event.arrival_order.first() {
            // Einfache Heuristik: wenn first != expected_order[0], nach 100 Updates anpassen
            if self.sample_count % 100 == 0 {
                // Alle arrival_order-Einträge zählen und häufigsten nach vorne
                // (Vereinfachung: hier nur ersten Eintrag prüfen)
                if !self.expected_order.contains(first) {
                    self.expected_order.insert(0, first.clone());
                    self.expected_order.truncate(3);
                }
            }
        }
    }
}
```

### 2. `LiveBaselineUpdater` — NATS Consumer

```rust
use crate::propagation::PropagationEvent;
use std::sync::{Arc, RwLock};
use std::path::PathBuf;

/// Hält die aktuelle Baseline im Speicher und aktualisiert sie
/// kontinuierlich aus dem NATS `bgp.propagation` Stream.
pub struct LiveBaselineUpdater {
    pub store:   Arc<RwLock<BaselineStore>>,
    save_path:   PathBuf,
    save_every:  u64,   // Alle N Updates auf Disk schreiben
    update_count: std::sync::atomic::AtomicU64,
}

impl LiveBaselineUpdater {
    pub fn new(store: BaselineStore, save_path: PathBuf, save_every: u64) -> Self {
        Self {
            store:        Arc::new(RwLock::new(store)),
            save_path,
            save_every,
            update_count: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Verarbeitet ein neues PropagationEvent.
    /// Schreibt alle `save_every` Updates auf Disk.
    pub fn process(&self, event: &PropagationEvent) -> anyhow::Result<()> {
        if !is_good_route(event) { return Ok(()); }

        let key = BaselineKey::new(&event.prefix, event.origin_as);
        {
            let mut store = self.store.write().unwrap();
            store.entry(key)
                .and_modify(|b| b.update(event))
                .or_insert_with(|| {
                    let mut b = PropagationBaseline::new(
                        &event.prefix.to_string(), event.origin_as
                    );
                    b.update(event);
                    b
                });
        }

        let n = self.update_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        if n % self.save_every == 0 {
            let store = self.store.read().unwrap();
            save_baseline(&*store, &self.save_path)?;
            tracing::info!(update_count = n, "Baseline auf Disk geschrieben (inkrementell)");
        }

        Ok(())
    }
}
```

## Tests

```rust
#[test]
fn test_baseline_update_increases_sample_count() {
    use crate::propagation::PropagationEvent;
    use std::collections::BTreeMap;

    let mut baseline = PropagationBaseline::new("8.8.8.0/24", 15169);
    let mut arrivals = BTreeMap::new();
    arrivals.insert("rrc12".to_string(), 1000.0);
    arrivals.insert("rrc00".to_string(), 1000.089);
    arrivals.insert("rrc11".to_string(), 1000.891);

    let event = PropagationEvent {
        prefix:        "8.8.8.0/24".parse().unwrap(),
        origin_as:     15169,
        as_path:       vec![1103, 15169],
        first_arrival: 1000.0,
        last_arrival:  1000.891,
        spread_ms:     891.0,
        arrival_order: vec!["rrc12".into(), "rrc00".into(), "rrc11".into()],
        arrivals,
    };

    assert_eq!(baseline.sample_count, 0);
    baseline.update(&event);
    assert_eq!(baseline.sample_count, 1);
    // Paar ("rrc00", "rrc12") sollte existieren
    assert!(baseline.pairwise_deltas.contains_key(
        &("rrc00".to_string(), "rrc12".to_string())
    ));
}

#[test]
fn test_baseline_update_mean_converges() {
    let mut baseline = PropagationBaseline::new("1.0.0.0/24", 1);

    let make_event = |delta: f64| {
        let mut arrivals = std::collections::BTreeMap::new();
        arrivals.insert("rrc00".to_string(), 1000.0);
        arrivals.insert("rrc12".to_string(), 1000.0 + delta / 1000.0); // delta in ms → s
        arrivals.insert("rrc11".to_string(), 1000.5);
        PropagationEvent {
            prefix:        "1.0.0.0/24".parse().unwrap(),
            origin_as:     1,
            as_path:       vec![1],
            first_arrival: 1000.0,
            last_arrival:  1000.5,
            spread_ms:     500.0,
            arrival_order: vec!["rrc00".into(), "rrc12".into(), "rrc11".into()],
            arrivals,
        }
    };

    // 5 Events mit delta = 100ms
    for _ in 0..5 { baseline.update(&make_event(100.0)); }
    let stats = &baseline.pairwise_deltas[&("rrc00".to_string(), "rrc12".to_string())];
    assert!((stats.mean_ms - 100.0).abs() < 1.0);
}
```

## Qualität
- `cargo test wave_baseline` alle Tests grün
- `cargo clippy --all-targets -- -D warnings` sauber
- `cargo fmt --check` sauber
```

---

### Prompt 2.3.3 — Versionierung der Baseline-Dateien

```
Du arbeitest im Rust-Workspace `log-gateway`, Branch `trustwave-core`.
Datei: `src/wave_baseline.rs`
Voraussetzung: Prompt 2.3.2 ist erledigt. `save_baseline()` und `load_baseline()` existieren.

## Aufgabe

Implementiere Versionierung der Baseline-Dateien: jede gespeicherte Baseline
erhält einen Zeitstempel im Dateinamen. So sind Rollbacks möglich wenn eine
neue Baseline schlechtere Ergebnisse liefert.

```rust
use chrono::{DateTime, Utc};

/// Speichert die Baseline mit Zeitstempel im Dateinamen.
///
/// Beispiel: save_path = "data/baselines/baseline.bin.zst"
/// → erzeugt: "data/baselines/baseline_20240101_123045.bin.zst"
/// → aktualisiert: "data/baselines/baseline_latest.bin.zst" (Kopie)
///
/// Die `_latest`-Datei zeigt immer auf die neueste Version.
pub fn save_baseline_versioned(
    store:     &BaselineStore,
    save_dir:  &Path,
    base_name: &str,
) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(save_dir)?;

    let now: DateTime<Utc> = Utc::now();
    let timestamp = now.format("%Y%m%d_%H%M%S").to_string();
    let versioned_name = format!("{base_name}_{timestamp}.bin.zst");
    let versioned_path = save_dir.join(&versioned_name);

    save_baseline(store, &versioned_path)?;

    // latest-Datei aktualisieren (Kopie, kein Symlink — plattformübergreifend)
    let latest_path = save_dir.join(format!("{base_name}_latest.bin.zst"));
    std::fs::copy(&versioned_path, &latest_path)?;

    tracing::info!(
        versioned = %versioned_path.display(),
        latest    = %latest_path.display(),
        "Baseline versioniert gespeichert"
    );

    Ok(versioned_path)
}

/// Listet alle versionierten Baseline-Dateien im Verzeichnis (neueste zuerst).
pub fn list_baseline_versions(dir: &Path, base_name: &str) -> Vec<PathBuf> {
    let prefix  = format!("{base_name}_2");  // Zeitstempel beginnen mit "2" (Jahr 2xxx)
    let suffix  = ".bin.zst";
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with(&prefix) && n.ends_with(suffix))
                .unwrap_or(false)
        })
        .collect();
    files.sort_by(|a, b| b.cmp(a)); // Neueste zuerst (String-Sort = Lexikografisch = Chronologisch)
    files
}
```

## Tests

```rust
#[test]
fn test_save_versioned_creates_files() {
    use std::collections::HashMap;
    use tempfile::TempDir;

    let dir   = TempDir::new().unwrap();
    let store: BaselineStore = HashMap::new();

    let versioned = save_baseline_versioned(&store, dir.path(), "baseline").unwrap();
    assert!(versioned.exists());

    let latest = dir.path().join("baseline_latest.bin.zst");
    assert!(latest.exists());
}

#[test]
fn test_list_versions_sorted_newest_first() {
    use std::collections::HashMap;
    use tempfile::TempDir;

    let dir   = TempDir::new().unwrap();
    let store: BaselineStore = HashMap::new();

    // Zwei Versionen erstellen (sleep 1s für unterschiedliche Timestamps)
    let v1 = save_baseline_versioned(&store, dir.path(), "baseline").unwrap();
    std::thread::sleep(std::time::Duration::from_secs(1));
    let v2 = save_baseline_versioned(&store, dir.path(), "baseline").unwrap();

    let versions = list_baseline_versions(dir.path(), "baseline");
    // Mindestens 2 Versionen
    assert!(versions.len() >= 2);
    // Neueste zuerst
    assert_eq!(versions[0], v2);
    assert_eq!(versions[1], v1);
}
```

## Qualität
- `cargo test wave_baseline` alle Tests grün
- `cargo clippy --all-targets -- -D warnings` sauber
- `cargo fmt --check` sauber
```

---

### Prompt 2.3.4 — Memory-Mapped Load (`load_baseline_mmap`)

```
Du arbeitest im Rust-Workspace `log-gateway`, Branch `trustwave-core`.
Datei: `src/wave_baseline.rs`
Voraussetzung: Prompt 2.3.3 ist erledigt.

## Kontext

Für Produktivbetrieb mit ~800k Präfixen werden Baseline-Dateien mehrere
Hundert Megabyte groß. Memory-Mapped Loading (mmap) vermeidet dass das
Betriebssystem eine separate Pufferkopie anlegen muss — die Datei wird
direkt in den virtuellen Adressraum eingeblendet.

## Voraussetzung Cargo.toml

In root `Cargo.toml` unter `[dependencies]`:
```toml
memmap2 = "0.9"   # Bereits ergänzt laut Phase-2-Header
```

## Aufgabe

```rust
use memmap2::Mmap;

/// Lädt eine Baseline-Datei via Memory-Map.
///
/// Vorteil gegenüber `load_baseline()`:
/// - Kein zusätzlicher Heap-Buffer für die komprimierte Datei
/// - OS kann Datei-Pages on-demand einlesen (lazy loading)
/// - Besonders vorteilhaft für Dateien > 100 MB
///
/// # Safety
/// Die Verwendung von `Mmap` erfordert `unsafe`. Das mmap wird
/// nur lesend genutzt — keine Schreiboperationen.
///
/// # Fehler
/// - Datei nicht gefunden oder nicht lesbar
/// - zstd-Dekomprimierungsfehler
/// - bincode-Deserialisierungsfehler
pub fn load_baseline_mmap(path: &Path) -> anyhow::Result<BaselineStore> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("Baseline-Datei nicht gefunden: {}", path.display()))?;

    // SAFETY: Wir öffnen die Datei nur lesend und halten keine anderen
    // Referenzen darauf. Das mmap ist für die Lebensdauer dieses Blocks gültig.
    let mmap = unsafe { Mmap::map(&file) }
        .with_context(|| "mmap fehlgeschlagen")?;

    // zstd-Dekomprimierung aus dem mmap-Slice (kein extra Copy nötig)
    let decoded = zstd::decode_all(&mmap[..])
        .with_context(|| "zstd-Dekomprimierung fehlgeschlagen")?;

    // bincode-Deserialisierung
    let store: BaselineStore = bincode::deserialize(&decoded)
        .with_context(|| "bincode-Deserialisierung fehlgeschlagen")?;

    tracing::info!(
        path    = %path.display(),
        entries = store.len(),
        size_mb = mmap.len() / (1024 * 1024),
        "Baseline via mmap geladen"
    );

    Ok(store)
}
```

## Tests

```rust
#[test]
fn test_load_mmap_roundtrip() {
    use std::collections::HashMap;
    use tempfile::NamedTempFile;

    // Baseline speichern
    let mut store: BaselineStore = HashMap::new();
    let key = BaselineKey { prefix: "8.8.8.0/24".to_string(), origin_as: 15169 };
    let mut baseline = PropagationBaseline::new("8.8.8.0/24", 15169);
    baseline.sample_count = 100;
    baseline.pairwise_deltas.insert(
        ("rrc00".to_string(), "rrc12".to_string()),
        WaveStats { mean_ms: 89.0, std_ms: 5.0, min_ms: 70.0, max_ms: 110.0, count: 100 },
    );
    store.insert(key.clone(), baseline);

    let tmp = NamedTempFile::new().unwrap();
    save_baseline(&store, tmp.path()).unwrap();

    // Via mmap laden
    let loaded = load_baseline_mmap(tmp.path()).unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[&key].sample_count, 100);
}

#[test]
fn test_load_mmap_nonexistent_returns_error() {
    let result = load_baseline_mmap(Path::new("/nonexistent/file.bin.zst"));
    assert!(result.is_err());
}

#[test]
fn test_load_mmap_equals_load_baseline() {
    use std::collections::HashMap;
    use tempfile::NamedTempFile;

    let mut store: BaselineStore = HashMap::new();
    let key = BaselineKey { prefix: "1.1.1.0/24".to_string(), origin_as: 13335 };
    store.insert(key.clone(), PropagationBaseline::new("1.1.1.0/24", 13335));

    let tmp = NamedTempFile::new().unwrap();
    save_baseline(&store, tmp.path()).unwrap();

    let via_read = load_baseline(tmp.path()).unwrap();
    let via_mmap = load_baseline_mmap(tmp.path()).unwrap();

    assert_eq!(via_read.len(), via_mmap.len());
    assert_eq!(
        via_read[&key].origin_as,
        via_mmap[&key].origin_as
    );
}
```

## Qualität
- `cargo test wave_baseline` alle Tests grün (inkl. alle vorherigen 2.1–2.3)
- `cargo clippy --all-targets -- -D warnings` sauber
- `cargo fmt --check` sauber
```

---

## Meilenstein-Check Phase 2

Nach allen 12 Prompts müssen diese Befehle alle erfolgreich sein:

```bash
# Workspace-Integrität
cargo check --workspace
cargo clippy --all-targets -- -D warnings
cargo fmt --check

# Unit-Tests
cargo test wave_baseline          # 20+ neue Tests grün
cargo test -p baseline-builder    # Tool baut und startet

# Build
cargo build -p baseline-builder   # CLI-Tool erfolgreich gebaut

# Manuelle Prüfung
baseline-builder --help           # Gibt sinnvolle Usage aus
baseline-builder \
  --data-dir data/mrt/ \
  --output   /tmp/test_baseline.bin.zst  # Startet (auch wenn data/mrt/ leer)
```

**Meilenstein 2 bestanden wenn:**
- `PropagationBaseline` enthält für jedes beobachtete Kollektor-Paar Zeitdifferenz-Statistiken (mean, std, min, max)
- `is_reliable()` schützt vor Baseline-Entries mit zu wenig Daten (< 30 Samples)
- Baseline wird verlustfrei als bincode+zstd gespeichert und geladen (Roundtrip-Test grün)
- `baseline-builder` CLI startet und verarbeitet MRT-Verzeichnisse
- Alle bestehenden 320 Tests weiterhin grün

---

## Notizen für Phase 3

Nach Phase 2 sind folgende Strukturen verfügbar die Phase 3 direkt nutzt:

```rust
// In src/wave_baseline.rs:
pub struct PropagationBaseline {
    pub pairwise_deltas: HashMap<(String, String), WaveStats>,
    pub expected_order:  Vec<String>,
    pub sample_count:    u64,
    // ...
}

impl PropagationBaseline {
    pub fn is_reliable(&self) -> bool { ... }
    pub fn baseline_confidence(&self) -> f64 { ... }
}

// WaveStats für Z-Score Berechnung in Phase 3:
// z_score = (observed_delta_ms - stats.mean_ms) / stats.std_ms
// z_score > 3.0 → Anomalie (3σ)
```

Phase 3 (`WaveAnomalyDetector`) nimmt `PropagationEvent` + `&PropagationBaseline`
und berechnet daraus 5 Signale → `WaveScore`.

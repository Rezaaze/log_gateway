use crate::propagation::PropagationEvent;
use anyhow::Context;
use ipnet::IpNet;
use memmap2::Mmap;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

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
    if event.arrivals.len() < 3 {
        return false;
    }
    if event.as_path.is_empty() {
        return false;
    }
    if event.as_path.len() > 6 {
        return false;
    }
    if event.spread_ms > 10_000.0 {
        return false;
    }
    true
}

/// Schlüssel für die Baseline-HashMap: (Präfix, Ursprungs-AS).
/// Der AS-Pfad-Hash ist NICHT im Schlüssel — gleiche Route via
/// verschiedene AS-Pfade teilen sich eine Baseline (nach Qualitätsfilter).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BaselineKey {
    /// Normalisiertes Präfix als String, z.B. "8.8.8.0/24"
    pub prefix: String,
    /// Ursprungs-AS (letzter Hop im AS-Pfad)
    pub origin_as: u32,
}

impl BaselineKey {
    pub fn new(prefix: &IpNet, origin_as: u32) -> Self {
        Self {
            prefix: prefix.to_string(),
            origin_as,
        }
    }
}

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

/// Online-Akkumulator für Zeitdifferenz-Statistiken.
/// Nutzt Welford's Algorithmus — numerisch stabiler als naive Varianz.
/// Nicht serialisiert — nur für den Baseline-Aufbau.
#[derive(Debug, Clone, Default)]
pub struct WaveStatsAccumulator {
    count: u64,
    mean: f64,
    m2: f64, // Summe der quadratischen Abweichungen (Welford)
    min: f64,
    max: f64,
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
        let delta = value_ms - self.mean;
        self.mean += delta / self.count as f64;
        let delta2 = value_ms - self.mean;
        self.m2 += delta * delta2;
        if value_ms < self.min {
            self.min = value_ms;
        }
        if value_ms > self.max {
            self.max = value_ms;
        }
    }

    /// Konvertiert in serialisierbare WaveStats.
    /// Gibt None zurück wenn noch keine Werte vorhanden.
    pub fn to_stats(&self) -> Option<WaveStats> {
        if self.count == 0 {
            return None;
        }
        let variance = if self.count > 0 {
            self.m2 / self.count as f64
        } else {
            0.0
        };
        Some(WaveStats {
            mean_ms: self.mean,
            std_ms: variance.sqrt(),
            min_ms: self.min,
            max_ms: self.max,
            count: self.count,
        })
    }

    pub fn count(&self) -> u64 {
        self.count
    }
}

/// Interner Akkumulator während des Baseline-Aufbaus.
/// Wird nach dem Batch-Lauf in `PropagationBaseline` konvertiert.
#[derive(Debug, Default)]
#[allow(dead_code)]
pub(crate) struct BaselineAccumulator {
    /// Paarweise Akkumulatoren: (rrc_a, rrc_b) alphabetisch → Zeitdifferenz-Akkumulator
    pub pairwise: HashMap<(String, String), WaveStatsAccumulator>,
    /// Zählt wie oft jeder Kollektor als Erster ankam
    pub first_arrival_counts: HashMap<String, u64>,
    /// Gesamtzahl eingeflossener PropagationEvents
    pub sample_count: u64,
}

#[allow(dead_code)]
impl BaselineAccumulator {
    /// Verarbeitet ein `PropagationEvent` und akkumuliert alle Paar-Deltas.
    pub fn add_event(&mut self, arrivals: &BTreeMap<String, f64>) {
        if arrivals.len() < 2 {
            return;
        }

        // Kollektoren sortiert nach Ankunftszeit (Aufstieg)
        let mut sorted: Vec<(&String, f64)> = arrivals.iter().map(|(k, &v)| (k, v)).collect();
        sorted.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

        // Erster Kollektor zählen
        if let Some((first_collector, _)) = sorted.first() {
            *self
                .first_arrival_counts
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
        let expected_order: Vec<String> = order.into_iter().take(3).map(|(k, _)| k).collect();

        PropagationBaseline {
            prefix: prefix.to_string(),
            origin_as,
            pairwise_deltas,
            expected_order,
            sample_count: self.sample_count,
            last_updated: std::time::SystemTime::now(),
        }
    }
}

/// Statistisches Modell des normalen Propagationsverhaltens
/// für ein bestimmtes (Präfix, Origin-AS) Paar.
///
/// Wird aus historischen MRT-Daten aufgebaut und im Echtzeit-Betrieb
/// zum Erkennen von Abweichungen (Hijacks) verwendet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropagationBaseline {
    /// Das beobachtete IP-Präfix
    pub prefix: String,
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
            prefix: prefix.to_string(),
            origin_as,
            pairwise_deltas: HashMap::new(),
            expected_order: Vec::new(),
            sample_count: 0,
            last_updated: SystemTime::now(),
        }
    }

    /// Aktualisiert die Baseline mit einem neuen PropagationEvent.
    ///
    /// Aktualisiert pairwise_deltas und expected_order inkrementell.
    /// ACHTUNG: Diese Methode nutzt nicht Welford — sie ist eine Näherung
    /// für inkrementelle Updates (aktualisiert nur mean, nicht std_ms).
    /// Für genaue Statistiken wird der vollständige Batch-Rebuild empfohlen.
    pub fn update(&mut self, event: &PropagationEvent) {
        if event.arrivals.len() < 3 {
            return;
        }

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
                        if delta_ms < s.min_ms {
                            s.min_ms = delta_ms;
                        }
                        if delta_ms > s.max_ms {
                            s.max_ms = delta_ms;
                        }
                        s.count = new_count;
                    })
                    .or_insert(WaveStats {
                        mean_ms: delta_ms,
                        std_ms: 0.0,
                        min_ms: delta_ms,
                        max_ms: delta_ms,
                        count: 1,
                    });
            }
        }

        self.sample_count += 1;
        self.last_updated = std::time::SystemTime::now();

        // expected_order: erster Kollektor des Events
        if let Some(first) = event.arrival_order.first() {
            // Einfache Heuristik: wenn first != expected_order[0], nach 100 Updates anpassen
            if self.sample_count.is_multiple_of(100) {
                // Alle arrival_order-Einträge zählen und häufigsten nach vorne
                // (Vereinfachung: hier nur ersten Eintrag prüfen)
                if !self.expected_order.contains(first) {
                    self.expected_order.insert(0, first.clone());
                    self.expected_order.truncate(3);
                }
            }
        }
    }

    /// Gibt true wenn die Baseline statistisch verlässlich ist.
    /// Schwelle: ≥ 30 Samples und mindestens ein Paar mit ≥ 10 Beobachtungen.
    pub fn is_reliable(&self) -> bool {
        if self.sample_count < 30 {
            return false;
        }
        self.pairwise_deltas.values().any(|s| s.count >= 10)
    }

    /// Konfidenzwert 0.0–1.0 basierend auf der Anzahl der Beobachtungen.
    ///
    /// Wächst logarithmisch:
    ///   0 samples  → 0.0
    ///   30 samples → 0.5 (Mindest-Schwelle)
    ///   100 samples → ~0.75
    ///   1000 samples → 1.0 (praktisches Maximum)
    pub fn baseline_confidence(&self) -> f64 {
        if self.sample_count == 0 {
            return 0.0;
        }
        let log_n = (self.sample_count as f64).ln();
        let log_max = 1000_f64.ln(); // ~6.9 → normalisiert auf 1.0
                                     // Skalierungsfaktor damit 30 Samples genau 0.5 ergeben
                                     // ln(30)/ln(1000) ≈ 0.4924, wir wollen 0.5 → Faktor ≈ 1.0155
        let scale = 0.5 / (30_f64.ln() / 1000_f64.ln());
        ((log_n / log_max) * scale).min(1.0)
    }
}

/// Typ-Alias für die vollständige Baseline-Datenbank.
/// Wird als einzelne Datei (bincode + zstd) gespeichert.
pub type BaselineStore = HashMap<BaselineKey, PropagationBaseline>;

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
        Self {
            accumulators: HashMap::new(),
        }
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

/// Speichert die `BaselineStore` als binäre Datei (bincode + zstd Kompression).
///
/// Format: bincode-serialisiert, dann zstd-komprimiert (Level 3).
/// Typische Größe: ~100k Präfixe → ~5–15 MB komprimiert.
///
/// # Fehler
/// - I/O-Fehler (Verzeichnis nicht vorhanden, keine Schreibrechte)
/// - Serialisierungsfehler (sollte nie auftreten bei korrekten Typen)
pub fn save_baseline(store: &BaselineStore, path: &std::path::Path) -> anyhow::Result<()> {
    let encoded = bincode::serialize(store)?;
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

/// Lädt eine Baseline-Datei (bincode + zstd).
///
/// # Fehler
/// - Datei nicht gefunden
/// - Dekomprimierungs- oder Deserialisierungsfehler (korrupte Datei)
pub fn load_baseline(path: &std::path::Path) -> anyhow::Result<BaselineStore> {
    let compressed = std::fs::read(path)?;
    let decoded = zstd::decode_all(&compressed[..])?;
    let store: BaselineStore = bincode::deserialize(&decoded)?;
    tracing::info!(
        path = %path.display(),
        entries = store.len(),
        "Baseline geladen"
    );
    Ok(store)
}

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
    let mmap = unsafe { Mmap::map(&file) }.with_context(|| "mmap fehlgeschlagen")?;

    // zstd-Dekomprimierung aus dem mmap-Slice (kein extra Copy nötig)
    let decoded =
        zstd::decode_all(&mmap[..]).with_context(|| "zstd-Dekomprimierung fehlgeschlagen")?;

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

/// Speichert die Baseline mit Zeitstempel im Dateinamen.
///
/// Beispiel: save_path = "data/baselines/baseline.bin.zst"
/// → erzeugt: "data/baselines/baseline_20240101_123045.bin.zst"
/// → aktualisiert: "data/baselines/baseline_latest.bin.zst" (Kopie)
///
/// Die `_latest`-Datei zeigt immer auf die neueste Version.
pub fn save_baseline_versioned(
    store: &BaselineStore,
    save_dir: &std::path::Path,
    base_name: &str,
) -> anyhow::Result<std::path::PathBuf> {
    use chrono::{DateTime, Utc};

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
pub fn list_baseline_versions(dir: &std::path::Path, base_name: &str) -> Vec<std::path::PathBuf> {
    let prefix = format!("{base_name}_2"); // Zeitstempel beginnen mit "2" (Jahr 2xxx)
    let suffix = ".bin.zst";
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
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

impl Default for BaselineBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Hält die aktuelle Baseline im Speicher und aktualisiert sie
/// kontinuierlich aus dem NATS `bgp.propagation` Stream.
pub struct LiveBaselineUpdater {
    pub store: Arc<RwLock<BaselineStore>>,
    save_path: PathBuf,
    save_every: u64, // Alle N Updates auf Disk schreiben
    update_count: std::sync::atomic::AtomicU64,
}

impl LiveBaselineUpdater {
    pub fn new(store: BaselineStore, save_path: PathBuf, save_every: u64) -> Self {
        Self {
            store: Arc::new(RwLock::new(store)),
            save_path,
            save_every,
            update_count: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Verarbeitet ein neues PropagationEvent.
    /// Schreibt alle `save_every` Updates auf Disk.
    pub fn process(&self, event: &PropagationEvent) -> anyhow::Result<()> {
        if !is_good_route(event) {
            return Ok(());
        }

        let key = BaselineKey::new(&event.prefix, event.origin_as);
        {
            let mut store = self.store.write().unwrap();
            store
                .entry(key)
                .and_modify(|b| b.update(event))
                .or_insert_with(|| {
                    let mut b =
                        PropagationBaseline::new(&event.prefix.to_string(), event.origin_as);
                    b.update(event);
                    b
                });
        }

        let n = self
            .update_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        if n.is_multiple_of(self.save_every) {
            let store = self.store.read().unwrap();
            save_baseline(&store, &self.save_path)?;
            tracing::info!(
                update_count = n,
                "Baseline auf Disk geschrieben (inkrementell)"
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_accumulator_single_value() {
        let mut acc = WaveStatsAccumulator::new();
        acc.update(100.0);
        let stats = acc.to_stats().unwrap();
        assert_eq!(stats.mean_ms, 100.0);
        assert_eq!(stats.std_ms, 0.0);
        assert_eq!(stats.count, 1);
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
        assert!((stats.std_ms - 2.0).abs() < 0.001);
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

    #[test]
    fn test_baseline_key_from_prefix() {
        let prefix: IpNet = "8.8.8.0/24".parse().unwrap();
        let key = BaselineKey::new(&prefix, 15169);
        assert_eq!(key.prefix, "8.8.8.0/24");
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
        assert_eq!(b.prefix, "8.8.8.0/24");
        assert_eq!(b.origin_as, 15169);
        assert_eq!(b.sample_count, 0);
        assert!(b.pairwise_deltas.is_empty());
        assert!(b.expected_order.is_empty());
    }

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

    #[test]
    fn test_is_reliable_below_threshold() {
        let mut b = PropagationBaseline::new("8.8.8.0/24", 15169);
        b.sample_count = 29;
        b.pairwise_deltas.insert(
            ("rrc00".to_string(), "rrc12".to_string()),
            WaveStats {
                mean_ms: 89.0,
                std_ms: 5.0,
                min_ms: 70.0,
                max_ms: 110.0,
                count: 15,
            },
        );
        assert!(!b.is_reliable());
    }

    #[test]
    fn test_is_reliable_at_threshold() {
        let mut b = PropagationBaseline::new("8.8.8.0/24", 15169);
        b.sample_count = 30;
        b.pairwise_deltas.insert(
            ("rrc00".to_string(), "rrc12".to_string()),
            WaveStats {
                mean_ms: 89.0,
                std_ms: 5.0,
                min_ms: 70.0,
                max_ms: 110.0,
                count: 10,
            },
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

    fn make_event(prefix: &str, origin_as: u32, arrivals: &[(&str, f64)]) -> PropagationEvent {
        use std::collections::BTreeMap;
        let arrivals_map: BTreeMap<String, f64> =
            arrivals.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        let first = arrivals_map.values().copied().fold(f64::INFINITY, f64::min);
        let last = arrivals_map
            .values()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max);
        let mut order: Vec<String> = arrivals_map.keys().cloned().collect();
        order.sort_by(|a, b| arrivals_map[a].partial_cmp(&arrivals_map[b]).unwrap());
        PropagationEvent {
            prefix: prefix.parse().unwrap(),
            origin_as,
            as_path: vec![1103, origin_as],
            arrivals: arrivals_map,
            first_arrival: first,
            last_arrival: last,
            spread_ms: (last - first) * 1000.0,
            arrival_order: order,
        }
    }

    #[test]
    fn test_builder_accumulates_events() {
        let mut builder = BaselineBuilder::new();
        let event = make_event(
            "8.8.8.0/24",
            15169,
            &[("rrc12", 1000.0), ("rrc00", 1000.089), ("rrc11", 1000.891)],
        );
        builder.add_event(&event);
        assert_eq!(builder.entry_count(), 1);
    }

    #[test]
    fn test_builder_different_prefix_different_entry() {
        let mut builder = BaselineBuilder::new();
        builder.add_event(&make_event(
            "8.8.8.0/24",
            15169,
            &[("rrc12", 1000.0), ("rrc00", 1000.1), ("rrc11", 1000.5)],
        ));
        builder.add_event(&make_event(
            "1.1.1.0/24",
            13335,
            &[("rrc12", 1000.0), ("rrc00", 1000.1), ("rrc11", 1000.5)],
        ));
        assert_eq!(builder.entry_count(), 2);
    }

    #[test]
    fn test_builder_build_filters_unreliable() {
        let mut builder = BaselineBuilder::new();
        // Nur 5 Events → sample_count = 5 < 30 → unreliable → nicht in build()
        for i in 0..5 {
            builder.add_event(&make_event(
                "8.8.8.0/24",
                15169,
                &[
                    ("rrc12", 1000.0 + i as f64 * 100.0),
                    ("rrc00", 1000.1 + i as f64 * 100.0),
                    ("rrc11", 1000.5 + i as f64 * 100.0),
                ],
            ));
        }
        let store = builder.build();
        assert!(
            store.is_empty(),
            "Weniger als 30 Samples → sollte gefiltert werden"
        );
    }

    #[test]
    fn test_builder_build_all_includes_unreliable() {
        let mut builder = BaselineBuilder::new();
        builder.add_event(&make_event(
            "8.8.8.0/24",
            15169,
            &[("rrc12", 1000.0), ("rrc00", 1000.1), ("rrc11", 1000.5)],
        ));
        let store = builder.build_all();
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn test_good_route_passes_filter() {
        let event = make_event(
            "8.8.8.0/24",
            15169,
            &[("rrc12", 1000.0), ("rrc00", 1000.089), ("rrc11", 1000.891)],
        );
        assert!(is_good_route(&event));
    }

    #[test]
    fn test_too_few_collectors_rejected() {
        let event = make_event(
            "8.8.8.0/24",
            15169,
            &[("rrc12", 1000.0), ("rrc00", 1000.089)],
        ); // nur 2
        assert!(!is_good_route(&event));
    }

    #[test]
    fn test_too_long_as_path_rejected() {
        use std::collections::BTreeMap;
        let arrivals: BTreeMap<String, f64> =
            [("rrc12", 1000.0), ("rrc00", 1000.1), ("rrc11", 1000.5)]
                .iter()
                .map(|(k, v)| (k.to_string(), *v))
                .collect();
        let event = PropagationEvent {
            prefix: "8.8.8.0/24".parse().unwrap(),
            origin_as: 15169,
            as_path: vec![1, 2, 3, 4, 5, 6, 7], // 7 Hops → zu lang
            arrivals: arrivals.clone(),
            first_arrival: 1000.0,
            last_arrival: 1000.5,
            spread_ms: 500.0,
            arrival_order: vec!["rrc12".into(), "rrc00".into(), "rrc11".into()],
        };
        assert!(!is_good_route(&event));
    }

    #[test]
    fn test_large_spread_rejected() {
        use std::collections::BTreeMap;
        let arrivals: BTreeMap<String, f64> =
            [("rrc12", 1000.0), ("rrc00", 1005.0), ("rrc11", 1011.0)]
                .iter()
                .map(|(k, v)| (k.to_string(), *v))
                .collect();
        let event = PropagationEvent {
            prefix: "1.0.0.0/24".parse().unwrap(),
            origin_as: 1,
            as_path: vec![1],
            arrivals: arrivals.clone(),
            first_arrival: 1000.0,
            last_arrival: 1011.0,
            spread_ms: 11_000.0, // > 10s → abgelehnt
            arrival_order: vec!["rrc12".into(), "rrc00".into(), "rrc11".into()],
        };
        assert!(!is_good_route(&event));
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        use std::collections::HashMap;
        use tempfile::NamedTempFile;

        let mut store: BaselineStore = HashMap::new();
        let key = BaselineKey {
            prefix: "8.8.8.0/24".to_string(),
            origin_as: 15169,
        };
        let mut baseline = PropagationBaseline::new("8.8.8.0/24", 15169);
        baseline.sample_count = 42;
        baseline.pairwise_deltas.insert(
            ("rrc00".to_string(), "rrc12".to_string()),
            WaveStats {
                mean_ms: 89.0,
                std_ms: 5.0,
                min_ms: 70.0,
                max_ms: 110.0,
                count: 42,
            },
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
            prefix: "8.8.8.0/24".parse().unwrap(),
            origin_as: 15169,
            as_path: vec![1103, 15169],
            first_arrival: 1000.0,
            last_arrival: 1000.891,
            spread_ms: 891.0,
            arrival_order: vec!["rrc12".into(), "rrc00".into(), "rrc11".into()],
            arrivals,
        };

        assert_eq!(baseline.sample_count, 0);
        baseline.update(&event);
        assert_eq!(baseline.sample_count, 1);
        // Paar ("rrc00", "rrc12") sollte existieren
        assert!(baseline
            .pairwise_deltas
            .contains_key(&("rrc00".to_string(), "rrc12".to_string())));
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
                prefix: "1.0.0.0/24".parse().unwrap(),
                origin_as: 1,
                as_path: vec![1],
                first_arrival: 1000.0,
                last_arrival: 1000.5,
                spread_ms: 500.0,
                arrival_order: vec!["rrc00".into(), "rrc12".into(), "rrc11".into()],
                arrivals,
            }
        };

        // 5 Events mit delta = 100ms
        for _ in 0..5 {
            baseline.update(&make_event(100.0));
        }
        let stats = &baseline.pairwise_deltas[&("rrc00".to_string(), "rrc12".to_string())];
        assert!((stats.mean_ms - 100.0).abs() < 1.0);
    }

    #[test]
    fn test_save_versioned_creates_files() {
        use std::collections::HashMap;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
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

        let dir = TempDir::new().unwrap();
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

    #[test]
    fn test_load_mmap_roundtrip() {
        use std::collections::HashMap;
        use tempfile::NamedTempFile;

        // Baseline speichern
        let mut store: BaselineStore = HashMap::new();
        let key = BaselineKey {
            prefix: "8.8.8.0/24".to_string(),
            origin_as: 15169,
        };
        let mut baseline = PropagationBaseline::new("8.8.8.0/24", 15169);
        baseline.sample_count = 100;
        baseline.pairwise_deltas.insert(
            ("rrc00".to_string(), "rrc12".to_string()),
            WaveStats {
                mean_ms: 89.0,
                std_ms: 5.0,
                min_ms: 70.0,
                max_ms: 110.0,
                count: 100,
            },
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
        let key = BaselineKey {
            prefix: "1.1.1.0/24".to_string(),
            origin_as: 13335,
        };
        store.insert(key.clone(), PropagationBaseline::new("1.1.1.0/24", 13335));

        let tmp = NamedTempFile::new().unwrap();
        save_baseline(&store, tmp.path()).unwrap();

        let via_read = load_baseline(tmp.path()).unwrap();
        let via_mmap = load_baseline_mmap(tmp.path()).unwrap();

        assert_eq!(via_read.len(), via_mmap.len());
        assert_eq!(via_read[&key].origin_as, via_mmap[&key].origin_as);
    }
}

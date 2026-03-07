use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::time::SystemTime;

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
}

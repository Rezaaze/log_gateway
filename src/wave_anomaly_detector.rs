//! Wave Anomaly Detector

use crate::propagation::PropagationEvent;
use crate::wave_baseline::{z_score, WaveBaseline, WaveBaselineEntry};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnomalyScore {
    pub total_score: f64,
    pub signals: Signals,
    pub classification: AnomalyClassification,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnomalyClassification {
    Normal,
    Suspicious,
    Anomalous,
}

impl AnomalyClassification {
    pub fn from_score(score: f64) -> Self {
        if score < 0.3 {
            AnomalyClassification::Normal
        } else if score < 0.6 {
            AnomalyClassification::Suspicious
        } else {
            AnomalyClassification::Anomalous
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signals {
    /// |z-score| of this event's spread vs. the baseline's p50/std_dev,
    /// clamped to [0,1] over a 3-sigma range. Symmetric: an unusually
    /// *small* spread (arrived suspiciously simultaneously — the classic
    /// hijack signature) scores just as high as an unusually large one.
    pub spread_z_score: f64,
    pub outlier_factor: f64,
    pub collector_gap_ratio: f64,
    /// How much this event's earliest-arriving collectors differ from the
    /// baseline's historically expected order (0 = matches, 1 = completely
    /// different). Requires `WaveBaselineEntry::expected_order` — 0.0 if
    /// the baseline entry has none (e.g. built before this field existed).
    pub order_deviation: f64,
    /// Spread relative to the physical speed-of-light minimum between the
    /// two most distant reporting collectors — NOT baseline-relative. See
    /// `calculate_propagation_speed`'s doc comment for why this signal
    /// alone cannot distinguish a hijack from legitimate anycast.
    pub propagation_speed: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnomalyDetectorConfig {
    pub spread_weight: f64,
    pub outlier_weight: f64,
    pub gap_weight: f64,
    pub order_weight: f64,
    pub speed_weight: f64,
}

impl Default for AnomalyDetectorConfig {
    fn default() -> Self {
        Self {
            spread_weight: 0.4,
            outlier_weight: 0.3,
            gap_weight: 0.15,
            order_weight: 0.1,
            speed_weight: 0.05,
        }
    }
}

pub struct WaveAnomalyDetector {
    baseline: Option<WaveBaseline>,
    config: AnomalyDetectorConfig,
}

impl WaveAnomalyDetector {
    pub fn new(baseline_path: Option<&std::path::Path>) -> Result<Self, String> {
        let baseline = if let Some(path) = baseline_path {
            if path.exists() {
                Some(
                    WaveBaseline::load(path)
                        .map_err(|e| format!("Failed to load baseline: {}", e))?,
                )
            } else {
                tracing::warn!("Baseline file not found: {:?}", path);
                None
            }
        } else {
            None
        };
        Ok(Self {
            baseline,
            config: AnomalyDetectorConfig::default(),
        })
    }

    pub fn with_config(mut self, config: AnomalyDetectorConfig) -> Self {
        self.config = config;
        self
    }

    pub fn score_event(&self, event: &PropagationEvent) -> AnomalyScore {
        if let Some(ref baseline) = self.baseline {
            let key = format!("{}", event.prefix);
            let origin_as = event.origin_as;
            let as_path_hash = calculate_path_hash(&event.as_path);
            if let Some(entry) = baseline.find_entry(&key, origin_as, as_path_hash) {
                return self.calculate_score_for_entry(event, entry);
            }
        }
        AnomalyScore {
            total_score: 0.0,
            signals: Signals::default(),
            classification: AnomalyClassification::Normal,
        }
    }

    fn calculate_score_for_entry(
        &self,
        event: &PropagationEvent,
        entry: &WaveBaselineEntry,
    ) -> AnomalyScore {
        let spread_z = z_score(event.spread_ms, entry.p50_spread_ms, entry.std_dev);
        // abs(): a spread far SMALLER than baseline (arrived suspiciously
        // simultaneously — the actual hijack signature this whole system is
        // named after) must score just as high as one far larger. The
        // previous one-sided clamp(0.0, 1.0) on the raw (unsigned-not-
        // abs'd) z-score silently discarded every "too fast" case, which
        // was the primary signal this project's own incident writeup
        // describes ("rrc11 New York t=+12ms — viel zu früh").
        let spread_z_score = (spread_z.abs() / 3.0).clamp(0.0, 1.0);
        let outlier_factor = if event.spread_ms > entry.p99_spread_ms {
            1.0
        } else {
            0.0
        };
        let gap_ratio = calculate_gap_ratio(event);
        let order_deviation = calculate_order_deviation(event, entry);
        let speed = calculate_propagation_speed(event);
        let total_score = self.config.spread_weight * spread_z_score
            + self.config.outlier_weight * outlier_factor
            + self.config.gap_weight * gap_ratio
            + self.config.order_weight * order_deviation
            + self.config.speed_weight * speed;
        AnomalyScore {
            total_score: total_score.min(1.0),
            signals: Signals {
                spread_z_score,
                outlier_factor,
                collector_gap_ratio: gap_ratio,
                order_deviation,
                propagation_speed: speed,
            },
            classification: AnomalyClassification::from_score(total_score),
        }
    }

    pub fn is_anomalous(&self, event: &PropagationEvent, threshold: f64) -> bool {
        self.score_event(event).total_score >= threshold
    }

    pub fn warning_threshold(&self) -> f64 {
        0.3
    }

    pub fn critical_threshold(&self) -> f64 {
        0.6
    }
}

fn calculate_path_hash(as_path: &[u32]) -> u64 {
    as_path.iter().fold(0u64, |acc, &asn| {
        acc.wrapping_mul(31).wrapping_add(asn as u64)
    })
}

fn calculate_gap_ratio(event: &PropagationEvent) -> f64 {
    if event.arrivals.len() < 2 {
        return 0.0;
    }
    let mut arrivals: Vec<(&String, &f64)> = event.arrivals.iter().collect();
    arrivals.sort_by(|a, b| a.1.partial_cmp(b.1).unwrap());
    let mut gap_count = 0;
    for i in 0..arrivals.len() - 1 {
        let gap_ms = (arrivals[i + 1].1 - arrivals[i].1) * 1000.0;
        if gap_ms > 4.0 {
            gap_count += 1;
        }
    }
    gap_count as f64 / (arrivals.len() - 1) as f64
}

/// How much this event's earliest-arriving collectors differ from the
/// baseline's historically expected order — TRUSTWAVE_ROADMAP.md Abschnitt
/// 3.1's "Reihenfolge-Anomalie" signal (erste 3 Kollektoren weichen von
/// Baseline-Erwartung ab).
///
/// Compares the top-N earliest collectors in `event.arrival_order` against
/// `entry.expected_order` (collectors ranked by mean arrival position
/// across the baseline's historical samples) as a set overlap: 0.0 = same
/// set, 1.0 = completely different. Returns 0.0 when the baseline entry
/// has no `expected_order` (e.g. built before that field existed) — no
/// expectation to deviate from, not "always matches".
fn calculate_order_deviation(event: &PropagationEvent, entry: &WaveBaselineEntry) -> f64 {
    const TOP_N: usize = 3;
    if entry.expected_order.is_empty() || event.arrival_order.len() < 2 {
        return 0.0;
    }
    let actual_top: std::collections::HashSet<&String> =
        event.arrival_order.iter().take(TOP_N).collect();
    let expected_top: std::collections::HashSet<&String> =
        entry.expected_order.iter().take(TOP_N).collect();
    let denom = actual_top.len().min(expected_top.len());
    if denom == 0 {
        return 0.0;
    }
    let overlap = actual_top.intersection(&expected_top).count();
    1.0 - (overlap as f64 / denom as f64)
}

/// Spread relative to the speed-of-light-in-fiber minimum between the two
/// most distant reporting collectors: a spread much smaller than that
/// minimum means the announcement arrived implausibly fast to be a single
/// physical origin propagating outward, which is the wave-physics
/// hijack signature.
///
/// **Known limitation:** this is an absolute physics floor, not relative to
/// this prefix's own baseline — unlike `spread_z_score`, it can't tell a
/// hijack from legitimate anycast (a prefix intentionally announced from
/// many sites at once has no single "origin" for light-speed to bound at
/// all, and will trip this signal every time regardless of history). Kept
/// as a coarse, low-weighted contributor pending a real anycast-aware
/// design (e.g. an operator-supplied allowlist of known-anycast prefixes,
/// or recognizing a baseline whose own historical spread is consistently
/// near-zero as "normally simultaneous" and damping this signal for it).
fn calculate_propagation_speed(event: &PropagationEvent) -> f64 {
    if event.arrivals.len() < 2 {
        return 0.0;
    }
    let max_dist_km: f64 = find_max_collector_distance(event);
    if max_dist_km == 0.0 {
        return 0.0;
    }
    let max_light_time_ms = (max_dist_km / 200_000.0) * 1_000.0;
    let speed = event.spread_ms / max_light_time_ms.max(0.001);
    speed.clamp(0.0, 1.0)
}

fn find_max_collector_distance(event: &PropagationEvent) -> f64 {
    use crate::collector_registry as geo;
    let mut max_dist: f64 = 0.0;
    let collectors: Vec<&String> = event.arrivals.keys().collect();
    for i in 0..collectors.len() {
        for j in i + 1..collectors.len() {
            if let Some(dist) = geo::distance_between_collectors(collectors[i], collectors[j]) {
                max_dist = max_dist.max(dist);
            }
        }
    }
    max_dist
}

impl Default for Signals {
    fn default() -> Self {
        Self {
            spread_z_score: 0.0,
            outlier_factor: 0.0,
            collector_gap_ratio: 0.0,
            order_deviation: 0.0,
            propagation_speed: 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn make_test_event(spread_ms: f64, num_collectors: usize) -> PropagationEvent {
        let mut arrivals = BTreeMap::new();
        let base_time = 1000.0;
        for i in 0..num_collectors {
            arrivals.insert(
                format!("rrc{}", i),
                base_time + (i as f64 * 10.0 / num_collectors as f64),
            );
        }
        PropagationEvent {
            prefix: "8.8.8.0/24".parse().unwrap(),
            origin_as: 15169,
            as_path: vec![1103, 15169],
            arrivals: arrivals.clone(),
            first_arrival: 1000.0,
            last_arrival: 1000.0 + (spread_ms / 1000.0),
            spread_ms,
            arrival_order: arrivals.keys().cloned().collect(),
        }
    }

    #[test]
    fn test_z_score_simple() {
        let z = z_score(35.0, 25.0, 15.0);
        assert!((z - 0.666).abs() < 0.001);
    }

    #[test]
    fn test_anomaly_detector_no_baseline() {
        let detector = WaveAnomalyDetector::new(None).unwrap();
        let event = make_test_event(25.0, 3);
        let score = detector.score_event(&event);
        assert_eq!(score.total_score, 0.0);
        assert_eq!(score.classification, AnomalyClassification::Normal);
    }

    #[test]
    fn test_anomaly_thresholds() {
        let config = AnomalyDetectorConfig {
            spread_weight: 0.9,
            outlier_weight: 0.05,
            gap_weight: 0.02,
            order_weight: 0.02,
            speed_weight: 0.01,
        };
        let detector = WaveAnomalyDetector::new(None).unwrap().with_config(config);
        let event_normal = make_test_event(25.0, 3);
        assert!(!detector.is_anomalous(&event_normal, detector.warning_threshold()));
        let event_extreme = make_test_event(1000.0, 3);
        assert!(!detector.is_anomalous(&event_extreme, detector.warning_threshold()));
    }

    #[test]
    fn test_gap_ratio() {
        let _detector = WaveAnomalyDetector::new(None).unwrap();
        let event = make_test_event(25.0, 3);
        let gap_ratio = calculate_gap_ratio(&event);
        assert!((0.0..=1.0).contains(&gap_ratio));
    }

    #[test]
    fn test_path_hash_consistency() {
        assert_eq!(calculate_path_hash(&[]), 0);
        assert_eq!(
            calculate_path_hash(&[1, 2, 3]),
            calculate_path_hash(&[1, 2, 3])
        );
        assert_ne!(
            calculate_path_hash(&[1, 2, 3]),
            calculate_path_hash(&[3, 2, 1])
        );
    }

    #[test]
    fn test_spread_z_score_symmetric_catches_too_fast_arrival() {
        // Baseline: this prefix normally has ~100ms spread, std_dev 20ms.
        let as_path = vec![1103u32, 15169u32];
        let path_hash = calculate_path_hash(&as_path);
        let mut baseline = WaveBaseline::new("test");
        baseline.extend(vec![WaveBaselineEntry {
            prefix: "8.8.8.0/24".to_string(),
            origin_as: 15169,
            as_path_hash: path_hash,
            sample_count: 100,
            min_spread_ms: 50.0,
            p50_spread_ms: 100.0,
            p95_spread_ms: 140.0,
            p99_spread_ms: 160.0,
            max_spread_ms: 180.0,
            std_dev: 20.0,
            expected_order: vec![],
        }]);
        let tmp = tempfile::NamedTempFile::new().unwrap();
        baseline.save(tmp.path()).unwrap();
        let detector = WaveAnomalyDetector::new(Some(tmp.path())).unwrap();

        // Spread of 10ms vs. baseline mean 100ms/std 20ms -> z = -4.5.
        // Before the abs() fix, the one-sided clamp(0.0, 1.0) on the raw
        // (signed) z-score discarded this entirely, scoring 0.0 for the
        // exact "arrived suspiciously simultaneously" pattern this project
        // is built to catch.
        let mut arrivals = BTreeMap::new();
        arrivals.insert("rrc00".to_string(), 1000.0);
        arrivals.insert("rrc01".to_string(), 1000.01);
        let event = PropagationEvent::new("8.8.8.0/24".parse().unwrap(), 15169, as_path, arrivals);

        let score = detector.score_event(&event);
        assert!(
            score.signals.spread_z_score > 0.9,
            "a spread far SMALLER than baseline (suspiciously simultaneous arrival) must score \
             high on spread_z_score, not be clamped to 0 — got {}",
            score.signals.spread_z_score
        );
    }

    #[test]
    fn test_order_deviation_detects_reordering() {
        let as_path = vec![1103u32, 15169u32];
        let path_hash = calculate_path_hash(&as_path);
        let mut baseline = WaveBaseline::new("test");
        baseline.extend(vec![WaveBaselineEntry {
            prefix: "8.8.8.0/24".to_string(),
            origin_as: 15169,
            as_path_hash: path_hash,
            sample_count: 100,
            min_spread_ms: 10.0,
            p50_spread_ms: 25.0,
            p95_spread_ms: 50.0,
            p99_spread_ms: 75.0,
            max_spread_ms: 100.0,
            std_dev: 15.0,
            expected_order: vec![
                "rrc00".to_string(),
                "rrc01".to_string(),
                "rrc02".to_string(),
            ],
        }]);
        let tmp = tempfile::NamedTempFile::new().unwrap();
        baseline.save(tmp.path()).unwrap();
        let detector = WaveAnomalyDetector::new(Some(tmp.path())).unwrap();

        // Matches the baseline's expected order exactly.
        let mut matching_arrivals = BTreeMap::new();
        matching_arrivals.insert("rrc00".to_string(), 1000.0);
        matching_arrivals.insert("rrc01".to_string(), 1000.01);
        matching_arrivals.insert("rrc02".to_string(), 1000.02);
        let matching_event = PropagationEvent::new(
            "8.8.8.0/24".parse().unwrap(),
            15169,
            as_path.clone(),
            matching_arrivals,
        );
        let matching_score = detector.score_event(&matching_event);
        assert_eq!(
            matching_score.signals.order_deviation, 0.0,
            "arrival order matching the baseline's expected order must score 0 deviation"
        );

        // Zero overlap with the expected top-3 collectors.
        let mut different_arrivals = BTreeMap::new();
        different_arrivals.insert("rrcXX".to_string(), 1000.0);
        different_arrivals.insert("rrcYY".to_string(), 1000.01);
        different_arrivals.insert("rrcZZ".to_string(), 1000.02);
        let different_event = PropagationEvent::new(
            "8.8.8.0/24".parse().unwrap(),
            15169,
            as_path,
            different_arrivals,
        );
        let different_score = detector.score_event(&different_event);
        assert_eq!(
            different_score.signals.order_deviation, 1.0,
            "arrival order with zero overlap vs. the expected order must score maximum deviation"
        );
    }
}

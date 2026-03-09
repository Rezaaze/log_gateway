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
    pub spread_z_score: f64,
    pub outlier_factor: f64,
    pub collector_gap_ratio: f64,
    pub arrival_order_entropy: f64,
    pub propagation_speed: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnomalyDetectorConfig {
    pub spread_weight: f64,
    pub outlier_weight: f64,
    pub gap_weight: f64,
    pub entropy_weight: f64,
    pub speed_weight: f64,
}

impl Default for AnomalyDetectorConfig {
    fn default() -> Self {
        Self {
            spread_weight: 0.4,
            outlier_weight: 0.3,
            gap_weight: 0.15,
            entropy_weight: 0.1,
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
        let spread_z_score = (spread_z / 3.0).clamp(0.0, 1.0);
        let outlier_factor = if event.spread_ms > entry.p99_spread_ms {
            1.0
        } else {
            0.0
        };
        let gap_ratio = calculate_gap_ratio(event);
        let order_entropy = calculate_order_entropy(event);
        let speed = calculate_propagation_speed(event);
        let total_score = self.config.spread_weight * spread_z_score
            + self.config.outlier_weight * outlier_factor
            + self.config.gap_weight * gap_ratio
            + self.config.entropy_weight * order_entropy
            + self.config.speed_weight * speed;
        AnomalyScore {
            total_score: total_score.min(1.0),
            signals: Signals {
                spread_z_score,
                outlier_factor,
                collector_gap_ratio: gap_ratio,
                arrival_order_entropy: order_entropy,
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

fn calculate_order_entropy(event: &PropagationEvent) -> f64 {
    let arrival_order = &event.arrival_order;
    if arrival_order.len() < 2 {
        return 0.0;
    }
    let unique_count = arrival_order.len();
    let max_possible = arrival_order.len();
    if max_possible == 0 {
        return 0.0;
    }
    (unique_count as f64 / max_possible as f64).min(1.0)
}

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
            arrival_order_entropy: 0.0,
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
            entropy_weight: 0.02,
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
}

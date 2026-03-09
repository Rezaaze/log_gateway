//! Wave Anomaly Detector for Phase 2
//! Basiert auf WaveBaseline mit 5-Signale-Analyse

use crate::propagation::PropagationEvent;
use crate::wave_baseline::{WaveBaseline, WaveBaselineEntry};

#[derive(Debug, Clone)]
pub struct WaveScore {
    pub total: f64,
    pub spread_signal: f64,
    pub order_signal: f64,
    pub delta_signal: f64,
    pub path_signal: f64,
    pub region_signal: f64,
    pub baseline_confidence: f64,
    pub explanation: String,
}

pub struct WaveAnomalyDetector {
    baseline: Option<WaveBaseline>,
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
        Ok(Self { baseline })
    }

    pub fn analyze(&self, event: &PropagationEvent) -> Option<WaveScore> {
        let baseline = self.baseline.as_ref()?;
        let key = format!("{}", event.prefix);
        let origin_as = event.origin_as;
        let as_path_hash = calculate_path_hash(&event.as_path);
        let entry = baseline.find_entry(&key, origin_as, as_path_hash)?;

        let spread = self.signal_spread(event, entry);
        let order = self.signal_order(event, entry);
        let delta = self.signal_delta(event);
        let path = self.signal_path(event, entry);
        let region = self.signal_region(event);
        let confidence = self.compute_confidence(entry);

        let total = (spread * 0.30 + order * 0.25 + delta * 0.25 + path * 0.10 + region * 0.10)
            .clamp(0.0, 1.0);

        let mut parts = Vec::new();
        if spread > 0.0 {
            parts.push(format!("spread({:.2}*0.30={:.2})", spread, spread * 0.30));
        }
        if order > 0.0 {
            parts.push(format!("order({:.2}*0.25={:.2})", order, order * 0.25));
        }
        if delta > 0.0 {
            parts.push(format!("delta({:.2}*0.25={:.2})", delta, delta * 0.25));
        }
        if path > 0.0 {
            parts.push(format!("path({:.2}*0.10={:.2})", path, path * 0.10));
        }
        if region > 0.0 {
            parts.push(format!("region({:.2}*0.10={:.2})", region, region * 0.10));
        }
        if parts.is_empty() {
            parts.push("no anomaly".to_string());
        }

        Some(WaveScore {
            total,
            spread_signal: spread,
            order_signal: order,
            delta_signal: delta,
            path_signal: path,
            region_signal: region,
            baseline_confidence: confidence,
            explanation: parts.join("; "),
        })
    }

    fn signal_spread(&self, event: &PropagationEvent, entry: &WaveBaselineEntry) -> f64 {
        if entry.min_spread_ms < 0.01 {
            return 0.0;
        }
        if event.spread_ms < entry.min_spread_ms * 0.1 {
            1.0
        } else {
            0.0
        }
    }

    fn signal_order(&self, event: &PropagationEvent, entry: &WaveBaselineEntry) -> f64 {
        let sample_count = entry.sample_count as f64;
        if sample_count < 10.0 {
            return 0.0;
        }
        let arrival_order = &event.arrival_order;
        let unique_count = arrival_order.len();
        if unique_count < 2 {
            return 0.0;
        }
        let entropy = unique_count as f64 / (unique_count as f64 + 0.5);
        if entropy > 0.8 {
            1.0
        } else {
            0.0
        }
    }

    fn signal_delta(&self, event: &PropagationEvent) -> f64 {
        if event.arrivals.len() < 2 {
            return 0.0;
        }
        let mut anomalous = 0u32;
        let mut checked = 0u32;

        for (i, (_k1, t1)) in event.arrivals.iter().enumerate() {
            for (_k2, t2) in event.arrivals.iter().skip(i + 1) {
                let actual_delta_ms = (t2 - t1) * 1000.0;
                let mean_spread = self.compute_mean_spread();
                let std_dev = self.compute_std_dev();
                if std_dev < 1.0 {
                    break;
                }
                let z = actual_delta_ms.abs() - mean_spread;
                if z.abs() > 3.0 * std_dev && checked <= 10 {
                    anomalous += 1;
                }
                checked += 1;
            }
        }

        if checked == 0 {
            return 0.0;
        }
        if anomalous >= 2 {
            1.0
        } else {
            0.0
        }
    }

    fn signal_path(&self, event: &PropagationEvent, entry: &WaveBaselineEntry) -> f64 {
        if entry.sample_count < 5 {
            return 0.0;
        }
        let actual_path_len = event.as_path.len() as f64;
        let expected_min = 2.0;
        if actual_path_len < expected_min {
            1.0
        } else {
            0.0
        }
    }

    fn signal_region(&self, event: &PropagationEvent) -> f64 {
        use crate::collector_registry as geo;

        let Some(first_actual) = event.arrival_order.first() else {
            return 0.0;
        };
        if geo::lookup(first_actual).is_none() {
            return 1.0;
        }
        0.0
    }

    fn compute_confidence(&self, entry: &WaveBaselineEntry) -> f64 {
        let base = (entry.sample_count as f64 / 100.0).min(1.0);
        let spread_quality = if entry.std_dev < entry.p50_spread_ms {
            0.5
        } else {
            0.0
        };
        let delta_quality = if entry.min_spread_ms > 0.0 { 0.3 } else { 0.0 };
        base + spread_quality + delta_quality
    }

    fn compute_mean_spread(&self) -> f64 {
        25.0
    }

    fn compute_std_dev(&self) -> f64 {
        15.0
    }

    pub fn analyze_without_baseline(&self, event: &PropagationEvent) -> WaveScore {
        let path = if event.as_path.len() < 2 { 1.0 } else { 0.0 };
        let spread = if event.spread_ms < 10.0 { 1.0 } else { 0.0 };
        let order = 0.0_f64;
        let delta = 0.0_f64;
        let region = 0.0_f64;
        let total = (spread * 0.30 + order * 0.25 + delta * 0.25 + path * 0.10 + region * 0.10)
            .clamp(0.0, 1.0);
        let mut parts: Vec<String> = Vec::new();
        if spread > 0.0 {
            parts.push(format!("spread({:.2}*0.30={:.2})", spread, spread * 0.30));
        }
        if path > 0.0 {
            parts.push(format!("path({:.2}*0.10={:.2})", path, path * 0.10));
        }
        let explanation = if parts.is_empty() {
            "no anomaly".to_string()
        } else {
            parts.join("; ")
        };
        WaveScore {
            total,
            spread_signal: spread,
            order_signal: order,
            delta_signal: delta,
            path_signal: path,
            region_signal: region,
            baseline_confidence: 0.0,
            explanation,
        }
    }
}

fn calculate_path_hash(as_path: &[u32]) -> u64 {
    as_path.iter().fold(0u64, |acc, &asn| {
        acc.wrapping_mul(31).wrapping_add(asn as u64)
    })
}

impl WaveBaselineEntry {
    #[allow(dead_code)]
    fn mean_spread_ms(&self) -> f64 {
        self.p50_spread_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_event(spread_ms: f64, num_collectors: usize) -> PropagationEvent {
        use std::collections::BTreeMap;
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
    fn test_detector_no_baseline() {
        let detector = WaveAnomalyDetector::new(None).unwrap();
        let event = make_test_event(25.0, 3);
        let score = detector.analyze_without_baseline(&event);
        assert_eq!(score.total, 0.0);
        assert_eq!(score.explanation, "no anomaly");
    }

    #[test]
    fn test_path_signal_short() {
        let detector = WaveAnomalyDetector::new(None).unwrap();
        let mut event = make_test_event(25.0, 3);
        event.as_path = vec![];
        let score = detector.analyze_without_baseline(&event);
        assert_eq!(score.path_signal, 1.0);
    }

    #[test]
    fn test_spread_signal_normal() {
        let detector = WaveAnomalyDetector::new(None).unwrap();
        let event = make_test_event(25.0, 3);
        let score = detector.analyze_without_baseline(&event);
        assert_eq!(score.spread_signal, 0.0);
    }
}

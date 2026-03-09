//! Wave Baseline – Historische Propagationsverteilungen

use anyhow::{Context, Result};
use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::SystemTime;

pub fn is_good_route(event: &crate::propagation::PropagationEvent) -> bool {
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

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BaselineKey {
    pub prefix: String,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaveStats {
    pub mean_ms: f64,
    pub std_ms: f64,
    pub min_ms: f64,
    pub max_ms: f64,
    pub count: u64,
}

#[derive(Debug, Clone, Default)]
pub struct WaveStatsAccumulator {
    count: u64,
    mean: f64,
    m2: f64,
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

    pub fn to_stats(&self) -> Option<WaveStats> {
        if self.count == 0 {
            return None;
        }
        let variance = self.m2 / self.count as f64;
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaveBaselineEntry {
    pub prefix: String,
    pub origin_as: u32,
    pub as_path_hash: u64,
    pub sample_count: u64,
    pub min_spread_ms: f64,
    pub p50_spread_ms: f64,
    pub p95_spread_ms: f64,
    pub p99_spread_ms: f64,
    pub max_spread_ms: f64,
    pub std_dev: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaveBaseline {
    pub version: String,
    pub created_at: f64,
    pub description: String,
    pub entries: Vec<WaveBaselineEntry>,
}

impl WaveBaseline {
    pub fn new(description: &str) -> Self {
        Self {
            version: "1.0".to_string(),
            created_at: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs_f64(),
            description: description.to_string(),
            entries: Vec::new(),
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read baseline file: {:?}", path))?;
        let baseline: WaveBaseline =
            serde_json::from_str(&content).with_context(|| "Failed to parse baseline JSON")?;
        Ok(baseline)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let content =
            serde_json::to_string_pretty(self).with_context(|| "Failed to serialize baseline")?;
        std::fs::write(path, content)
            .with_context(|| format!("Failed to write baseline to {:?}", path))?;
        Ok(())
    }

    pub fn find_entry(
        &self,
        prefix: &str,
        origin_as: u32,
        as_path_hash: u64,
    ) -> Option<&WaveBaselineEntry> {
        self.entries.iter().find(|e| {
            e.prefix == prefix && e.origin_as == origin_as && e.as_path_hash == as_path_hash
        })
    }

    pub fn extend(&mut self, entries: Vec<WaveBaselineEntry>) {
        self.entries.extend(entries);
    }
}

#[allow(dead_code)]
fn percentile(sorted_values: &[f64], p: f64) -> f64 {
    if sorted_values.is_empty() {
        return 0.0;
    }
    if sorted_values.len() == 1 {
        return sorted_values[0];
    }

    let n = sorted_values.len() as f64;
    let rank = (p / 100.0) * (n - 1.0);
    let lower = rank.floor() as usize;
    let upper = rank.ceil() as usize;

    if lower == upper {
        sorted_values[lower.min(sorted_values.len() - 1)]
    } else {
        let fraction = rank - lower as f64;
        sorted_values[lower] * (1.0 - fraction) + sorted_values[upper] * fraction
    }
}

pub fn compute_spread_stats(spreads: &[f64]) -> WaveStats {
    if spreads.is_empty() {
        return WaveStats {
            mean_ms: 0.0,
            std_ms: 0.0,
            min_ms: 0.0,
            max_ms: 0.0,
            count: 0,
        };
    }

    let mut sorted = spreads.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let mean: f64 = spreads.iter().sum::<f64>() / spreads.len() as f64;
    let variance: f64 =
        spreads.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / spreads.len() as f64;

    WaveStats {
        mean_ms: mean,
        std_ms: variance.sqrt(),
        min_ms: sorted[0],
        max_ms: sorted[sorted.len() - 1],
        count: spreads.len() as u64,
    }
}

pub fn z_score(value: f64, mean: f64, std: f64) -> f64 {
    if std == 0.0 {
        return 0.0;
    }
    (value - mean) / std
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_percentile_simple() {
        let values = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        assert!((percentile(&values, 50.0) - 3.0).abs() < 0.1);
    }

    #[test]
    fn test_z_score_known_values() {
        assert_eq!(z_score(3.0, 3.0, 1.0), 0.0);
        assert_eq!(z_score(4.0, 3.0, 1.0), 1.0);
    }

    #[test]
    fn test_baseline_save_load() {
        let baseline = WaveBaseline::new("Test baseline");
        let entry = WaveBaselineEntry {
            prefix: "8.8.8.0/24".to_string(),
            origin_as: 15169,
            as_path_hash: 12345,
            sample_count: 100,
            min_spread_ms: 10.0,
            p50_spread_ms: 25.0,
            p95_spread_ms: 50.0,
            p99_spread_ms: 75.0,
            max_spread_ms: 100.0,
            std_dev: 15.0,
        };
        let mut baseline = baseline.clone();
        baseline.entries.push(entry);
        let temp_path = "/tmp/test_baseline4.json";
        baseline.save(Path::new(temp_path)).unwrap();
        let loaded = WaveBaseline::load(Path::new(temp_path)).unwrap();
        assert_eq!(loaded.entries.len(), 1);
        let _ = std::fs::remove_file(temp_path);
    }

    #[test]
    fn test_is_good_route() {
        use std::collections::BTreeMap;
        let arrivals: BTreeMap<String, f64> = [("a".to_string(), 1.0), ("b".to_string(), 2.0)]
            .iter()
            .cloned()
            .collect();
        assert!(!is_good_route(&crate::propagation::PropagationEvent::new(
            "1.0.0.0/24".parse().unwrap(),
            1,
            vec![1],
            arrivals.clone()
        )));
        let arrivals: BTreeMap<String, f64> = [
            ("a".to_string(), 1.0),
            ("b".to_string(), 2.0),
            ("c".to_string(), 3.0),
        ]
        .iter()
        .cloned()
        .collect();
        assert!(is_good_route(&crate::propagation::PropagationEvent::new(
            "1.0.0.0/24".parse().unwrap(),
            1,
            vec![1, 2, 3],
            arrivals
        )));
    }
}

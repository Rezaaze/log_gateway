//! Wave Baseline – Historische Propagationsverteilungen

// The single shared AS-path hash: baseline entries must be keyed exactly the
// way `WaveAnomalyDetector` looks them up, or offline-built baselines are
// invisible to the live detector.
use crate::propagation::path_hash;
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
    /// Collectors ranked by mean arrival position across historical
    /// samples (index 0 = arrives earliest on average). Empty for entries
    /// built before this field existed, or with too few samples to be
    /// meaningful — callers must treat empty as "no expectation available"
    /// rather than "always first place".
    #[serde(default)]
    pub expected_order: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaveBaseline {
    pub version: String,
    /// Wall-clock time this baseline was built/saved — NOT the age of its
    /// underlying data. A baseline built today from a 2018 MRT archive has
    /// `created_at` = today. Use `data_cutoff_ts` for anything that needs
    /// to know how recent the underlying observations are (e.g. backtest
    /// leakage checks).
    pub created_at: f64,
    /// Latest observed sample timestamp (unix seconds) across all source
    /// `PropagationEvent`s this baseline was built from. `0.0` means
    /// unknown — either the baseline predates this field, or it was
    /// constructed by hand rather than via `BaselineBuilder`.
    #[serde(default)]
    pub data_cutoff_ts: f64,
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
            data_cutoff_ts: 0.0,
            description: description.to_string(),
            entries: Vec::new(),
        }
    }

    /// Loads a baseline written by `save()` — bincode-encoded, zstd-compressed
    /// (see Abschnitt 2.3.1 in TRUSTWAVE_ROADMAP.md: JSON was rejected as too
    /// large for the ~800k prefixes a full baseline covers).
    pub fn load(path: &Path) -> Result<Self> {
        let compressed = std::fs::read(path)
            .with_context(|| format!("Failed to read baseline file: {:?}", path))?;
        let bytes = zstd::decode_all(compressed.as_slice())
            .with_context(|| "Failed to decompress baseline (zstd)")?;
        let baseline: WaveBaseline = bincode::deserialize(&bytes)
            .with_context(|| "Failed to deserialize baseline (bincode)")?;
        Ok(baseline)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let bytes =
            bincode::serialize(self).with_context(|| "Failed to serialize baseline (bincode)")?;
        let compressed = zstd::encode_all(bytes.as_slice(), 3)
            .with_context(|| "Failed to compress baseline (zstd)")?;
        std::fs::write(path, compressed)
            .with_context(|| format!("Failed to write baseline to {:?}", path))?;
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
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

/// Persists a baseline the same way as `WaveBaseline::save()` — a thin
/// free-function wrapper kept for callers (e.g. `tools/baseline_builder`)
/// that build a baseline incrementally via `BaselineBuilder` rather than
/// holding a `WaveBaseline` directly.
pub fn save_baseline(baseline: &WaveBaseline, path: &Path) -> Result<()> {
    baseline.save(path)
}

/// (prefix, origin_as, as_path_hash) — the grouping key `BaselineBuilder`
/// accumulates samples under.
type GroupKey = (String, u32, u64);

/// Per group: collector -> (sum of arrival-rank positions, count) — rank 0
/// means "arrived first". Used to compute `WaveBaselineEntry::expected_order`.
type RankSums = std::collections::HashMap<String, (f64, u64)>;

/// Accumulates `PropagationEvent`s (typically parsed from historical MRT
/// archive data) into a `WaveBaseline`, grouped by (prefix, origin_as,
/// as_path_hash). Only groups that reach `min_samples` observations produce
/// a baseline entry — see Abschnitt 2.1.4/2.2.2 in TRUSTWAVE_ROADMAP.md
/// ("Stabilität-Counter: ab n ≥ 30 Samples gilt Baseline als verlässlich").
pub struct BaselineBuilder {
    samples: std::collections::HashMap<GroupKey, Vec<f64>>,
    rank_sums: std::collections::HashMap<GroupKey, RankSums>,
    min_samples: usize,
    /// Latest `last_arrival` seen across all fed events — becomes the
    /// resulting baseline's `data_cutoff_ts`.
    max_observed_ts: f64,
}

impl Default for BaselineBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl BaselineBuilder {
    pub fn new() -> Self {
        Self {
            samples: std::collections::HashMap::new(),
            rank_sums: std::collections::HashMap::new(),
            min_samples: 30,
            max_observed_ts: 0.0,
        }
    }

    /// Overrides the minimum sample count required for a group to produce a
    /// baseline entry (default: 30).
    pub fn with_min_samples(mut self, min_samples: usize) -> Self {
        self.min_samples = min_samples;
        self
    }

    /// Feeds one `PropagationEvent`'s spread and arrival order into its
    /// (prefix, origin_as, as_path) group.
    pub fn add_event(&mut self, event: &crate::propagation::PropagationEvent) {
        let key = (
            event.prefix.to_string(),
            event.origin_as,
            path_hash(&event.as_path),
        );
        self.samples
            .entry(key.clone())
            .or_default()
            .push(event.spread_ms);

        let ranks = self.rank_sums.entry(key).or_default();
        for (rank, collector) in event.arrival_order.iter().enumerate() {
            let entry = ranks.entry(collector.clone()).or_insert((0.0, 0));
            entry.0 += rank as f64;
            entry.1 += 1;
        }

        if event.last_arrival > self.max_observed_ts {
            self.max_observed_ts = event.last_arrival;
        }
    }

    /// Number of distinct (prefix, origin_as, as_path) groups observed so
    /// far, regardless of whether they meet `min_samples` yet.
    pub fn entry_count(&self) -> usize {
        self.samples.len()
    }

    /// Finalizes accumulated samples into a `WaveBaseline`. Groups with
    /// fewer than `min_samples` observations are dropped — not enough data
    /// to be a reliable baseline for wave-score comparison.
    pub fn build(&self) -> WaveBaseline {
        let mut baseline = WaveBaseline::new("Built via tools/baseline_builder from MRT archive");
        baseline.data_cutoff_ts = self.max_observed_ts;
        let entries: Vec<WaveBaselineEntry> = self
            .samples
            .iter()
            .filter(|(_, values)| values.len() >= self.min_samples)
            .map(|((prefix, origin_as, as_path_hash), values)| {
                let mut sorted = values.clone();
                sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let stats = compute_spread_stats(&sorted);

                let mut expected_order: Vec<(String, f64)> = self
                    .rank_sums
                    .get(&(prefix.clone(), *origin_as, *as_path_hash))
                    .map(|ranks| {
                        ranks
                            .iter()
                            .map(|(collector, (sum, count))| {
                                (collector.clone(), sum / *count as f64)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                expected_order.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());

                WaveBaselineEntry {
                    prefix: prefix.clone(),
                    origin_as: *origin_as,
                    as_path_hash: *as_path_hash,
                    sample_count: sorted.len() as u64,
                    min_spread_ms: stats.min_ms,
                    p50_spread_ms: percentile(&sorted, 50.0),
                    p95_spread_ms: percentile(&sorted, 95.0),
                    p99_spread_ms: percentile(&sorted, 99.0),
                    max_spread_ms: stats.max_ms,
                    std_dev: stats.std_ms,
                    expected_order: expected_order.into_iter().map(|(c, _)| c).collect(),
                }
            })
            .collect();
        baseline.extend(entries);
        baseline
    }
}

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
            expected_order: vec!["rrc00".to_string(), "rrc12".to_string()],
        };
        let mut baseline = baseline.clone();
        baseline.entries.push(entry);
        let temp_path = "/tmp/test_baseline4.bin.zst";
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

    fn make_event(
        prefix: &str,
        origin_as: u32,
        as_path: Vec<u32>,
        spread_ms: f64,
    ) -> crate::propagation::PropagationEvent {
        use std::collections::BTreeMap;
        let arrivals: BTreeMap<String, f64> = [
            ("rrc00".to_string(), 1000.0),
            ("rrc01".to_string(), 1000.0 + spread_ms / 1000.0),
        ]
        .into_iter()
        .collect();
        crate::propagation::PropagationEvent::new(
            prefix.parse().unwrap(),
            origin_as,
            as_path,
            arrivals,
        )
    }

    #[test]
    fn test_baseline_builder_drops_groups_below_min_samples() {
        let mut builder = BaselineBuilder::new().with_min_samples(30);
        for i in 0..29 {
            builder.add_event(&make_event(
                "8.8.8.0/24",
                15169,
                vec![1103, 15169],
                20.0 + i as f64,
            ));
        }
        assert_eq!(builder.entry_count(), 1, "one distinct group observed");
        let baseline = builder.build();
        assert!(
            baseline.is_empty(),
            "29 samples is below the 30-sample reliability threshold"
        );
    }

    #[test]
    fn test_baseline_builder_produces_entry_at_min_samples() {
        let mut builder = BaselineBuilder::new().with_min_samples(30);
        for i in 0..30 {
            builder.add_event(&make_event(
                "8.8.8.0/24",
                15169,
                vec![1103, 15169],
                20.0 + i as f64,
            ));
        }
        let baseline = builder.build();
        assert_eq!(baseline.len(), 1);
        let entry = &baseline.entries[0];
        assert_eq!(entry.prefix, "8.8.8.0/24");
        assert_eq!(entry.origin_as, 15169);
        assert_eq!(entry.sample_count, 30);
        // Samples are 20.0..=49.0ms, uniformly spaced — p50 should land near
        // the middle of that range.
        assert!(entry.p50_spread_ms > 30.0 && entry.p50_spread_ms < 40.0);
        assert!(entry.min_spread_ms <= entry.p50_spread_ms);
        assert!(entry.p50_spread_ms <= entry.p95_spread_ms);
        assert!(entry.p95_spread_ms <= entry.p99_spread_ms);
        assert!(entry.p99_spread_ms <= entry.max_spread_ms);
    }

    #[test]
    fn test_baseline_builder_tracks_expected_order_by_mean_rank() {
        use crate::propagation::PropagationEvent;
        use std::collections::BTreeMap;

        fn event_with_order(order: &[&str]) -> PropagationEvent {
            let mut arrivals = BTreeMap::new();
            for (i, collector) in order.iter().enumerate() {
                arrivals.insert(collector.to_string(), 1000.0 + i as f64 * 0.001);
            }
            PropagationEvent::new(
                "8.8.8.0/24".parse().unwrap(),
                15169,
                vec![1103, 15169],
                arrivals,
            )
        }

        let mut builder = BaselineBuilder::new().with_min_samples(1);
        // 20 samples: rrcA, rrcB, rrcC (ranks 0,1,2). 10 samples: rrcB,
        // rrcA, rrcC (ranks 0,1,2 for B,A,C) — a minority reordering that
        // should NOT change the overall majority-order winner.
        for _ in 0..20 {
            builder.add_event(&event_with_order(&["rrcA", "rrcB", "rrcC"]));
        }
        for _ in 0..10 {
            builder.add_event(&event_with_order(&["rrcB", "rrcA", "rrcC"]));
        }

        let baseline = builder.build();
        assert_eq!(baseline.len(), 1);
        // Mean rank: A = (20*0 + 10*1)/30 = 0.33, B = (20*1 + 10*0)/30 = 0.67,
        // C = 2.0 always -> expected order A, B, C.
        assert_eq!(
            baseline.entries[0].expected_order,
            vec!["rrcA".to_string(), "rrcB".to_string(), "rrcC".to_string()]
        );
    }

    #[test]
    fn test_baseline_builder_separates_different_groups() {
        let mut builder = BaselineBuilder::new().with_min_samples(1);
        builder.add_event(&make_event("8.8.8.0/24", 15169, vec![1103, 15169], 20.0));
        builder.add_event(&make_event("1.1.1.0/24", 13335, vec![1103, 13335], 30.0));
        assert_eq!(builder.entry_count(), 2);
        let baseline = builder.build();
        assert_eq!(baseline.len(), 2);
    }

    #[test]
    fn test_baseline_builder_save_load_round_trip_via_free_function() {
        let mut builder = BaselineBuilder::new().with_min_samples(1);
        builder.add_event(&make_event("8.8.8.0/24", 15169, vec![1103, 15169], 42.0));
        let baseline = builder.build();

        let temp_path = "/tmp/test_baseline_builder_roundtrip.bin.zst";
        save_baseline(&baseline, Path::new(temp_path)).unwrap();
        let loaded = WaveBaseline::load(Path::new(temp_path)).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded.entries[0].prefix, "8.8.8.0/24");
        let _ = std::fs::remove_file(temp_path);
    }

    #[test]
    fn test_baseline_builder_tracks_data_cutoff_ts() {
        // make_event() anchors arrivals at base_time=1000.0 and offsets the
        // second collector by spread_ms/1000 seconds, so last_arrival grows
        // with spread_ms — data_cutoff_ts must track the maximum across all
        // fed events, not just the last one added.
        let mut builder = BaselineBuilder::new().with_min_samples(1);
        builder.add_event(&make_event("8.8.8.0/24", 15169, vec![1103, 15169], 10.0));
        builder.add_event(&make_event("8.8.8.0/24", 15169, vec![1103, 15169], 500.0));
        builder.add_event(&make_event("8.8.8.0/24", 15169, vec![1103, 15169], 100.0));
        let baseline = builder.build();
        assert!(
            (baseline.data_cutoff_ts - 1000.5).abs() < 0.001,
            "expected cutoff to track the 500ms-spread event's last_arrival (1000.5), got {}",
            baseline.data_cutoff_ts
        );
    }

    #[test]
    fn test_new_baseline_has_zero_data_cutoff() {
        // A hand-built baseline (not via BaselineBuilder) has no way to know
        // its data's real time range — callers must treat 0.0 as "unknown",
        // not "epoch 1970".
        let baseline = WaveBaseline::new("hand-built");
        assert_eq!(baseline.data_cutoff_ts, 0.0);
    }
}

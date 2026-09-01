//! Wave Anomaly Detector

use crate::propagation::PropagationEvent;
use crate::wave_baseline::{z_score, WaveBaseline, WaveBaselineEntry};
use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A baseline entry chosen as the scoring reference for an event, plus
/// which fallback tier it came from — see `WaveAnomalyDetector::find_reference_entry`.
struct ReferenceMatch<'a> {
    entry: &'a WaveBaselineEntry,
    origin_novelty: bool,
    deaggregation_mismatch: bool,
}

/// IPv4/IPv6 floor prefix length for the covering-aggregate walk in
/// `find_covering_prefix` — broader than this and a "covering aggregate"
/// stops being meaningful (e.g. an IPv4 /0-/7 covers a large fraction of
/// the entire routable address space; almost no real BGP announcement is
/// that broad, so a baseline "hit" there would be noise, not signal).
fn covering_floor_prefix_len(prefix: &IpNet) -> u8 {
    match prefix {
        IpNet::V4(_) => 8,
        IpNet::V6(_) => 19,
    }
}

/// Walks from `prefix` up to progressively broader covering aggregates
/// (via repeated `IpNet::supernet()`) looking for the first one with
/// baseline history, stopping at a family-appropriate floor to avoid
/// matching absurdly broad, meaningless aggregates. Returns the matching
/// aggregate's canonical string key (as used in `prefix_index`), not the
/// original `prefix` itself.
fn find_covering_prefix(
    prefix_index: &HashMap<String, Vec<usize>>,
    prefix: IpNet,
) -> Option<String> {
    let floor = covering_floor_prefix_len(&prefix);
    let mut current = prefix.supernet()?;
    while current.prefix_len() >= floor {
        let key = current.to_string();
        if prefix_index.contains_key(&key) {
            return Some(key);
        }
        current = current.supernet()?;
    }
    None
}

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
    /// 1.0 if this prefix has baseline history but never under this exact
    /// origin_as (the event was scored against a fallback entry from a
    /// *different* origin — see `score_event`'s fallback lookup); 0.0 if
    /// the origin_as matches some baseline entry for this prefix (whether
    /// or not the AS-path itself matches exactly). This is the signal that
    /// makes a classic origin hijack visible at all: the hijacker's
    /// origin_as by construction never has an exact-match baseline entry,
    /// since the baseline is built from the legitimate origin's history.
    pub origin_novelty: f64,
    /// 1.0 if this EXACT prefix has no baseline history at all, but a
    /// broader covering aggregate does, under a DIFFERENT origin_as than
    /// this event — i.e. someone who is not the aggregate's known origin
    /// just carved out a more-specific route within it. This is the
    /// textbook hijack-by-deaggregation signature (longest-prefix-match
    /// wins, so a more-specific route from an unrelated AS overrides the
    /// legitimate aggregate everywhere it propagates) — confirmed against
    /// the real Pakistan Telecom/YouTube 2008 incident, where the
    /// hijacked /24 had zero pre-incident history of its own but sits
    /// inside YouTube's normally-routed /22. 0.0 if the exact prefix has
    /// its own history (see `origin_novelty` instead), or if a covering
    /// aggregate exists under the SAME origin_as (a plausible legitimate
    /// traffic-engineering deaggregation by the prefix's own holder).
    pub deaggregation_mismatch: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnomalyDetectorConfig {
    pub spread_weight: f64,
    pub outlier_weight: f64,
    pub gap_weight: f64,
    pub order_weight: f64,
    pub speed_weight: f64,
    /// Weight of `Signals::origin_novelty` — deliberately not large enough
    /// on its own to cross `warning_threshold()`: a clean origin migration
    /// (legitimate provider change) with unremarkable propagation timing
    /// should not alone trigger an alert. Only origin novelty COMBINED
    /// with a real propagation-timing anomaly should cross into Suspicious
    /// / Anomalous — see TRUSTWAVE_ROADMAP.md Abschnitt 3.2 for the real
    /// backtest data this weight was calibrated against.
    pub origin_novelty_weight: f64,
    /// Weight of `Signals::deaggregation_mismatch`. Set higher than
    /// `origin_novelty_weight`: an unrelated AS deaggregating someone
    /// else's already-routed aggregate has essentially no legitimate use
    /// case (unlike a plain origin change, which can be an ordinary
    /// provider migration), so this alone should already land in the
    /// Suspicious band.
    pub deaggregation_weight: f64,
}

impl Default for AnomalyDetectorConfig {
    fn default() -> Self {
        Self {
            spread_weight: 0.25,
            outlier_weight: 0.15,
            gap_weight: 0.05,
            order_weight: 0.05,
            speed_weight: 0.05,
            origin_novelty_weight: 0.15,
            deaggregation_weight: 0.3,
        }
    }
}

pub struct WaveAnomalyDetector {
    baseline: Option<WaveBaseline>,
    config: AnomalyDetectorConfig,
    /// prefix → indices into `baseline.entries`, built once from the
    /// loaded baseline. Lets `score_event` find "any history for this
    /// prefix" in O(1) instead of a linear scan, and is what makes the
    /// origin-novelty fallback lookup below affordable at real baseline
    /// sizes (tens of thousands of entries, scored per live event).
    prefix_index: HashMap<String, Vec<usize>>,
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
        let prefix_index = build_prefix_index(baseline.as_ref());
        Ok(Self {
            baseline,
            config: AnomalyDetectorConfig::default(),
            prefix_index,
        })
    }

    pub fn with_config(mut self, config: AnomalyDetectorConfig) -> Self {
        self.config = config;
        self
    }

    /// Looks up the best baseline entry to score `event` against, in
    /// tiers:
    ///
    /// 1. Exact match: same prefix, same origin_as, same AS-path hash —
    ///    genuinely repeat traffic, score normally, no novelty signals.
    /// 2. Same origin, different path: the prefix has history under this
    ///    origin_as, just via a different AS-path (multihoming, a peering
    ///    change) — score against the origin's best-sampled entry, still
    ///    no novelty signals (path diversity alone isn't a hijack signal).
    /// 3. Different origin, same exact prefix: the prefix has baseline
    ///    history, but never under this origin_as. A hijack that reuses an
    ///    already-independently-routed prefix ALWAYS falls into this tier
    ///    (the hijacker's origin_as by construction never appears in a
    ///    baseline built from the legitimate origin's history) — score
    ///    against the prefix's best-sampled entry and set
    ///    `origin_novelty = 1.0`.
    /// 4. No history for the exact prefix, but a covering aggregate has
    ///    baseline history: a hijack via deaggregation (announcing a
    ///    more-specific, never-independently-routed sub-prefix of an
    ///    already-routed aggregate) falls here — confirmed against the
    ///    real Pakistan Telecom/YouTube 2008 incident, which tier 3 alone
    ///    cannot see (there is no baseline entry for the exact hijacked
    ///    /24 under ANY origin, legitimate or not). Score against the
    ///    covering aggregate's best-sampled entry; set
    ///    `deaggregation_mismatch = 1.0` if that entry's origin_as differs
    ///    from the event's (else this looks like a legitimate
    ///    traffic-engineering deaggregation by the aggregate's own
    ///    holder — origin matches, no flag).
    ///
    /// A prefix with NEITHER exact NOR covering-aggregate history (tier 0)
    /// stays unscored — there is nothing at all to compare against, and
    /// that narrower ambiguity (a brand new, top-level allocation that
    /// happens to be a hijack of an address block nobody here has ever
    /// observed announced) is a real, separate, still-open problem.
    fn find_reference_entry<'a>(
        &self,
        baseline: &'a WaveBaseline,
        event: &PropagationEvent,
    ) -> Option<ReferenceMatch<'a>> {
        let key = event.prefix.to_string();
        let as_path_hash = calculate_path_hash(&event.as_path);

        if let Some(indices) = self.prefix_index.get(&key) {
            if let Some(entry) = indices
                .iter()
                .map(|&i| &baseline.entries[i])
                .find(|e| e.origin_as == event.origin_as && e.as_path_hash == as_path_hash)
            {
                return Some(ReferenceMatch {
                    entry,
                    origin_novelty: false,
                    deaggregation_mismatch: false,
                });
            }

            let same_origin_best = indices
                .iter()
                .map(|&i| &baseline.entries[i])
                .filter(|e| e.origin_as == event.origin_as)
                .max_by_key(|e| e.sample_count);
            if let Some(entry) = same_origin_best {
                return Some(ReferenceMatch {
                    entry,
                    origin_novelty: false,
                    deaggregation_mismatch: false,
                });
            }

            let any_origin_best = indices
                .iter()
                .map(|&i| &baseline.entries[i])
                .max_by_key(|e| e.sample_count);
            if let Some(entry) = any_origin_best {
                return Some(ReferenceMatch {
                    entry,
                    origin_novelty: true,
                    deaggregation_mismatch: false,
                });
            }
        }

        let covering_key = find_covering_prefix(&self.prefix_index, event.prefix)?;
        let indices = self.prefix_index.get(&covering_key)?;
        let entry = indices
            .iter()
            .map(|&i| &baseline.entries[i])
            .max_by_key(|e| e.sample_count)?;
        Some(ReferenceMatch {
            entry,
            origin_novelty: false,
            deaggregation_mismatch: entry.origin_as != event.origin_as,
        })
    }

    pub fn score_event(&self, event: &PropagationEvent) -> AnomalyScore {
        if let Some(ref baseline) = self.baseline {
            if let Some(m) = self.find_reference_entry(baseline, event) {
                return self.calculate_score_for_entry(
                    event,
                    m.entry,
                    m.origin_novelty,
                    m.deaggregation_mismatch,
                );
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
        origin_novelty: bool,
        deaggregation_mismatch: bool,
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
        let origin_novelty_score = if origin_novelty { 1.0 } else { 0.0 };
        let deaggregation_mismatch_score = if deaggregation_mismatch { 1.0 } else { 0.0 };
        let total_score = self.config.spread_weight * spread_z_score
            + self.config.outlier_weight * outlier_factor
            + self.config.gap_weight * gap_ratio
            + self.config.order_weight * order_deviation
            + self.config.speed_weight * speed
            + self.config.origin_novelty_weight * origin_novelty_score
            + self.config.deaggregation_weight * deaggregation_mismatch_score;
        AnomalyScore {
            total_score: total_score.min(1.0),
            signals: Signals {
                spread_z_score,
                outlier_factor,
                collector_gap_ratio: gap_ratio,
                order_deviation,
                propagation_speed: speed,
                origin_novelty: origin_novelty_score,
                deaggregation_mismatch: deaggregation_mismatch_score,
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

fn build_prefix_index(baseline: Option<&WaveBaseline>) -> HashMap<String, Vec<usize>> {
    let mut idx: HashMap<String, Vec<usize>> = HashMap::new();
    if let Some(baseline) = baseline {
        for (i, entry) in baseline.entries.iter().enumerate() {
            idx.entry(entry.prefix.clone()).or_default().push(i);
        }
    }
    idx
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
            origin_novelty: 0.0,
            deaggregation_mismatch: 0.0,
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
            origin_novelty_weight: 0.0,
            deaggregation_weight: 0.0,
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

    fn make_baseline_entry(origin_as: u32, as_path: &[u32]) -> WaveBaselineEntry {
        make_baseline_entry_for("8.8.8.0/24", origin_as, as_path)
    }

    fn make_baseline_entry_for(prefix: &str, origin_as: u32, as_path: &[u32]) -> WaveBaselineEntry {
        WaveBaselineEntry {
            prefix: prefix.to_string(),
            origin_as,
            as_path_hash: calculate_path_hash(as_path),
            sample_count: 100,
            min_spread_ms: 50.0,
            p50_spread_ms: 100.0,
            p95_spread_ms: 140.0,
            p99_spread_ms: 160.0,
            max_spread_ms: 180.0,
            std_dev: 20.0,
            expected_order: vec![],
        }
    }

    /// A hijack, by definition, announces from an origin_as that never
    /// appears in a baseline built from the legitimate origin's history —
    /// before the fallback lookup this existed, `score_event` returned a
    /// hard 0.0/Normal on every such event (a guaranteed baseline-lookup
    /// miss), which is what a real backtest against the 2008 Pakistan
    /// Telecom/YouTube hijack surfaced (TRUSTWAVE_ROADMAP.md Abschnitt
    /// 3.2). The prefix-level fallback must now score it against the best
    /// available reference entry AND flag the origin mismatch.
    #[test]
    fn test_origin_novelty_fires_on_new_origin_for_known_prefix() {
        let legit_path = vec![1103u32, 15169u32];
        let mut baseline = WaveBaseline::new("test");
        baseline.extend(vec![make_baseline_entry(15169, &legit_path)]);
        let tmp = tempfile::NamedTempFile::new().unwrap();
        baseline.save(tmp.path()).unwrap();
        let detector = WaveAnomalyDetector::new(Some(tmp.path())).unwrap();

        // Same prefix, but a hijacker's origin_as that never appears in
        // the baseline at all — this used to be a guaranteed lookup miss.
        let mut arrivals = BTreeMap::new();
        arrivals.insert("rrc00".to_string(), 1000.0);
        arrivals.insert("rrc01".to_string(), 1000.01);
        arrivals.insert("rrc02".to_string(), 1000.02);
        let hijack_event = PropagationEvent::new(
            "8.8.8.0/24".parse().unwrap(),
            666, // hijacker AS, never seen in the baseline
            vec![174, 666],
            arrivals,
        );

        let score = detector.score_event(&hijack_event);
        assert_eq!(
            score.signals.origin_novelty, 1.0,
            "an origin_as never seen for this prefix in the baseline must set origin_novelty=1.0, \
             not silently fall back to a zero score"
        );
        assert!(
            score.total_score > 0.0,
            "origin novelty must contribute to the total score"
        );
    }

    /// The reverse case: a path change under the SAME origin_as (e.g. a
    /// multihoming/peering change) is common and benign — it must be
    /// scored against the origin's own history, not flagged as novel.
    #[test]
    fn test_same_origin_different_path_is_not_origin_novelty() {
        let legit_path = vec![1103u32, 15169u32];
        let mut baseline = WaveBaseline::new("test");
        baseline.extend(vec![make_baseline_entry(15169, &legit_path)]);
        let tmp = tempfile::NamedTempFile::new().unwrap();
        baseline.save(tmp.path()).unwrap();
        let detector = WaveAnomalyDetector::new(Some(tmp.path())).unwrap();

        let mut arrivals = BTreeMap::new();
        arrivals.insert("rrc00".to_string(), 1000.0);
        arrivals.insert("rrc01".to_string(), 1000.01);
        arrivals.insert("rrc02".to_string(), 1000.02);
        let event = PropagationEvent::new(
            "8.8.8.0/24".parse().unwrap(),
            15169,             // same, known-legitimate origin
            vec![3320, 15169], // different AS-path than the baseline entry
            arrivals,
        );

        let score = detector.score_event(&event);
        assert_eq!(
            score.signals.origin_novelty, 0.0,
            "a known origin announcing via a different path must not be flagged as origin-novel"
        );
    }

    /// A prefix with zero baseline entries at all must stay unscored —
    /// there is nothing to compare against. This ambiguity (new legitimate
    /// allocation vs. hijack-by-deaggregation) is real and intentionally
    /// NOT resolved by the origin-novelty fallback, which only helps once
    /// SOME baseline history exists for the prefix under a different
    /// origin. See hijack_detector_realcheck.rs's doc comment.
    #[test]
    fn test_unknown_prefix_stays_unscored_not_flagged_novel() {
        let mut baseline = WaveBaseline::new("test");
        baseline.extend(vec![make_baseline_entry(15169, &[1103, 15169])]);
        let tmp = tempfile::NamedTempFile::new().unwrap();
        baseline.save(tmp.path()).unwrap();
        let detector = WaveAnomalyDetector::new(Some(tmp.path())).unwrap();

        let mut arrivals = BTreeMap::new();
        arrivals.insert("rrc00".to_string(), 1000.0);
        arrivals.insert("rrc01".to_string(), 1000.01);
        let event = PropagationEvent::new(
            "203.0.113.0/24".parse().unwrap(), // never in the baseline at all
            666,
            vec![174, 666],
            arrivals,
        );

        let score = detector.score_event(&event);
        assert_eq!(score.total_score, 0.0);
        assert_eq!(score.classification, AnomalyClassification::Normal);
    }

    /// The real Pakistan Telecom/YouTube 2008 scenario: the hijacked /24
    /// has zero baseline history of its own (it was never independently
    /// routed before the incident), but it sits inside a /22 that IS in
    /// the baseline under the legitimate origin. A hijacker announcing the
    /// more-specific /24 under a DIFFERENT origin must be flagged via the
    /// covering-aggregate fallback, since tier 3 (same-prefix fallback)
    /// cannot see this case at all — there is no baseline entry for the
    /// exact /24 under any origin.
    #[test]
    fn test_deaggregation_mismatch_fires_on_covering_aggregate_with_different_origin() {
        let legit_path = vec![3356u32, 36561u32];
        let mut baseline = WaveBaseline::new("test");
        baseline.extend(vec![make_baseline_entry_for(
            "208.65.152.0/22",
            36561, // YouTube's real legitimate origin
            &legit_path,
        )]);
        let tmp = tempfile::NamedTempFile::new().unwrap();
        baseline.save(tmp.path()).unwrap();
        let detector = WaveAnomalyDetector::new(Some(tmp.path())).unwrap();

        let mut arrivals = BTreeMap::new();
        arrivals.insert("rrc00".to_string(), 1000.0);
        arrivals.insert("rrc01".to_string(), 1000.01);
        arrivals.insert("rrc02".to_string(), 1000.02);
        // A more-specific /24 within the baselined /22, never itself seen
        // before, announced by an unrelated origin (the real hijacker's AS).
        let hijack_event = PropagationEvent::new(
            "208.65.153.0/24".parse().unwrap(),
            17557, // Pakistan Telecom's real hijacker AS
            vec![9999, 17557],
            arrivals,
        );

        let score = detector.score_event(&hijack_event);
        assert_eq!(
            score.signals.deaggregation_mismatch, 1.0,
            "a more-specific prefix within a baselined aggregate, announced by an origin that \
             differs from the aggregate's known origin, must set deaggregation_mismatch=1.0"
        );
        assert_eq!(
            score.signals.origin_novelty, 0.0,
            "deaggregation_mismatch and origin_novelty are distinct tiers — this case is not \
             also tier 3 (there's no exact-prefix baseline entry to be 'novel' against)"
        );
        assert!(score.total_score > 0.0);
    }

    /// The same deaggregation, but by the aggregate's OWN legitimate
    /// origin (e.g. traffic engineering) — must NOT be flagged as a
    /// mismatch, since the origin matches the covering aggregate's known
    /// origin.
    #[test]
    fn test_deaggregation_by_legitimate_owner_is_not_flagged() {
        let legit_path = vec![3356u32, 36561u32];
        let mut baseline = WaveBaseline::new("test");
        baseline.extend(vec![make_baseline_entry_for(
            "208.65.152.0/22",
            36561,
            &legit_path,
        )]);
        let tmp = tempfile::NamedTempFile::new().unwrap();
        baseline.save(tmp.path()).unwrap();
        let detector = WaveAnomalyDetector::new(Some(tmp.path())).unwrap();

        let mut arrivals = BTreeMap::new();
        arrivals.insert("rrc00".to_string(), 1000.0);
        arrivals.insert("rrc01".to_string(), 1000.01);
        arrivals.insert("rrc02".to_string(), 1000.02);
        let te_event = PropagationEvent::new(
            "208.65.153.0/24".parse().unwrap(),
            36561, // the aggregate's OWN legitimate origin
            legit_path,
            arrivals,
        );

        let score = detector.score_event(&te_event);
        assert_eq!(
            score.signals.deaggregation_mismatch, 0.0,
            "a more-specific prefix announced by the covering aggregate's OWN legitimate origin \
             (e.g. traffic engineering) must not be flagged as a mismatch"
        );
    }

    /// A prefix with no history under ITS OWN exact match nor any covering
    /// aggregate broader than the family floor must still stay unscored —
    /// the covering-prefix walk must not wander into meaninglessly broad
    /// matches (e.g. treating a /7 as "covering" and alerting on
    /// essentially unrelated traffic).
    #[test]
    fn test_covering_prefix_walk_respects_floor() {
        let mut baseline = WaveBaseline::new("test");
        // Only a very broad, floor-violating aggregate exists — a real
        // baseline would essentially never contain something this broad,
        // but the walk must not use it even if it did.
        baseline.extend(vec![make_baseline_entry_for("1.0.0.0/7", 64512, &[64512])]);
        let tmp = tempfile::NamedTempFile::new().unwrap();
        baseline.save(tmp.path()).unwrap();
        let detector = WaveAnomalyDetector::new(Some(tmp.path())).unwrap();

        let mut arrivals = BTreeMap::new();
        arrivals.insert("rrc00".to_string(), 1000.0);
        arrivals.insert("rrc01".to_string(), 1000.01);
        let event =
            PropagationEvent::new("1.2.3.0/24".parse().unwrap(), 666, vec![174, 666], arrivals);

        let score = detector.score_event(&event);
        assert_eq!(score.total_score, 0.0);
        assert_eq!(score.classification, AnomalyClassification::Normal);
    }
}

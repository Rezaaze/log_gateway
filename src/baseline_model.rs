use dashmap::DashMap;
use std::sync::Arc;

/// Baseline statistics for a single prefix.
#[derive(Debug, Clone)]
pub struct PrefixBaseline {
    /// Exponential moving average of AS-path length.
    pub ema: f64,
    /// Exponential moving average of variance (squared deviations).
    pub variance_ema: f64,
    /// Number of samples seen so far.
    pub sample_count: u32,
}

impl PrefixBaseline {
    /// Creates a new baseline with the given initial AS-path length.
    fn new(initial_value: f64) -> Self {
        Self {
            ema: initial_value,
            variance_ema: 0.0,
            sample_count: 1,
        }
    }

    /// Updates the baseline with a new AS-path length.
    ///
    /// Uses the formula for exponential moving average:
    ///   ema = alpha * value + (1 - alpha) * ema
    ///
    /// And for exponential moving variance:
    ///   delta = value - ema_old
    ///   incr = alpha * delta
    ///   ema_new = ema_old + incr
    ///   variance_ema = (1 - alpha) * (variance_ema + alpha * delta²)
    fn update(&mut self, value: f64, alpha: f64) {
        let delta = value - self.ema;
        let incr = alpha * delta;
        self.ema += incr;

        // Update variance EMA using Welford's online algorithm adapted for EMA
        self.variance_ema = (1.0 - alpha) * (self.variance_ema + alpha * delta * delta);

        self.sample_count += 1;
    }

    /// Computes the Z‑score of a value relative to this baseline.
    ///
    /// Returns `None` if we have fewer than 5 samples (too little data).
    /// Z‑score = (value - ema) / sqrt(variance_ema + epsilon)
    /// where epsilon = 1e‑6 prevents division by zero.
    fn z_score(&self, value: f64) -> Option<f64> {
        if self.sample_count < 5 {
            return None;
        }

        let epsilon = 1e-6;
        let denominator = (self.variance_ema + epsilon).sqrt();
        if denominator == 0.0 {
            return None;
        }

        Some((value - self.ema) / denominator)
    }
}

/// Statistical baseline model for BGP anomaly detection.
///
/// Tracks multiple features for anomaly detection:
/// - AS-path length (EMA and variance)
/// - AS knowledge (how many days an AS has been seen)
///
/// Thread‑safe via `DashMap`.
#[derive(Debug, Clone)]
pub struct BaselineModel {
    /// EMA smoothing factor (0 < alpha ≤ 1).
    alpha: f64,
    /// Per‑prefix baselines for AS-path length.
    baselines: Arc<DashMap<String, PrefixBaseline>>,
    /// AS knowledge tracker.
    as_knowledge: Arc<AsKnowledgeTracker>,
}

impl BaselineModel {
    /// Creates a new baseline model with the given smoothing factor.
    ///
    /// # Panics
    ///
    /// Panics if `alpha` is not in (0, 1].
    pub fn new(alpha: f64) -> Self {
        assert!(alpha > 0.0 && alpha <= 1.0, "alpha must be in (0, 1]");
        Self {
            alpha,
            baselines: Arc::new(DashMap::new()),
            as_knowledge: Arc::new(AsKnowledgeTracker::new()),
        }
    }

    /// Updates the baseline for a prefix with a new AS‑path length.
    pub fn update(&self, prefix: &str, as_path_len: f64) {
        let entry = self.baselines.entry(prefix.to_string());
        match entry {
            dashmap::mapref::entry::Entry::Occupied(mut occ) => {
                occ.get_mut().update(as_path_len, self.alpha);
            }
            dashmap::mapref::entry::Entry::Vacant(vac) => {
                vac.insert(PrefixBaseline::new(as_path_len));
            }
        }
    }

    /// Computes the Z‑score of an AS‑path length for a prefix.
    ///
    /// Returns `None` if the prefix has fewer than 5 samples.
    pub fn z_score(&self, prefix: &str, as_path_len: f64) -> Option<f64> {
        self.baselines
            .get(prefix)
            .and_then(|baseline| baseline.z_score(as_path_len))
    }

    /// Checks whether an AS‑path length is anomalous for a prefix.
    ///
    /// Returns `true` if the absolute Z‑score exceeds `threshold`.
    /// Returns `false` if there are too few samples or the Z‑score is within bounds.
    pub fn is_anomaly(&self, prefix: &str, as_path_len: f64, threshold: f64) -> bool {
        self.z_score(prefix, as_path_len)
            .map(|z| z.abs() > threshold)
            .unwrap_or(false)
    }

    /// Records that an AS was seen on a specific date.
    ///
    /// This updates the AS knowledge tracker with the date when the AS was observed.
    ///
    /// # Arguments
    ///
    /// * `asn` - The AS number
    /// * `date` - Date in YYYY-MM-DD format
    pub fn record_as_seen(&self, asn: u32, date: &str) {
        self.as_knowledge.record_as_seen(asn, date);
    }

    /// Gets the AS knowledge tracker.
    pub fn as_knowledge(&self) -> &AsKnowledgeTracker {
        &self.as_knowledge
    }

    /// Gets a reference to the baselines DashMap.
    ///
    /// This is used by the ModelTrainer to save snapshots.
    pub fn baselines(&self) -> &Arc<DashMap<String, PrefixBaseline>> {
        &self.baselines
    }

    /// Computes a confidence boost based on multiple features.
    ///
    /// The boost is calculated as:
    /// 1. +0.1 if AS-path length is anomalous (z-score > 3.0)
    /// 2. +0.05 if prefix length is unusual (too specific or too broad)
    /// 3. -0.1 if AS is well-known (seen on ≥7 days) - reduces false positives
    ///
    /// The total boost is capped between -0.15 and +0.15.
    ///
    /// # Arguments
    ///
    /// * `prefix` - The CIDR prefix (e.g., "10.0.0.0/8")
    /// * `as_path_len` - AS-path length
    /// * `origin_as` - Origin AS number
    ///
    /// # Returns
    ///
    /// Confidence boost value between -0.15 and +0.15.
    pub fn compute_confidence_boost(&self, prefix: &str, as_path_len: f64, origin_as: u32) -> f64 {
        let mut boost: f64 = 0.0;

        // 1. AS-path length anomaly
        if self.is_anomaly(prefix, as_path_len, 3.0) {
            boost += 0.1;
        }

        // 2. Unusual prefix length
        if prefix_features::is_unusual_prefix_length(prefix) {
            boost += 0.05;
        }

        // 3. AS knowledge (well-known AS reduces suspicion)
        if self.as_knowledge.is_well_known(origin_as) {
            boost -= 0.1;
        }

        // Cap the boost
        boost.clamp(-0.15, 0.15)
    }

    /// Computes an enhanced confidence score by applying feature-based boost.
    ///
    /// This takes a base confidence and applies the computed boost.
    /// The result is capped between 0.0 and 1.0.
    ///
    /// # Arguments
    ///
    /// * `base_confidence` - Base confidence score (0.0 to 1.0)
    /// * `prefix` - The CIDR prefix
    /// * `as_path_len` - AS-path length
    /// * `origin_as` - Origin AS number
    ///
    /// # Returns
    ///
    /// Enhanced confidence score between 0.0 and 1.0.
    pub fn enhance_confidence(
        &self,
        base_confidence: f64,
        prefix: &str,
        as_path_len: f64,
        origin_as: u32,
    ) -> f64 {
        let boost = self.compute_confidence_boost(prefix, as_path_len, origin_as);
        (base_confidence + boost).clamp(0.0, 1.0)
    }
}

impl Default for BaselineModel {
    fn default() -> Self {
        Self::new(0.1)
    }
}

/// Utility functions for prefix length feature extraction.
pub mod prefix_features {
    /// Extracts the prefix length from a CIDR notation string.
    ///
    /// # Examples
    ///
    /// ```
    /// use log_gateway::baseline_model::prefix_features::extract_prefix_length;
    ///
    /// assert_eq!(extract_prefix_length("10.0.0.0/8"), Some(8));
    /// assert_eq!(extract_prefix_length("2001:db8::/32"), Some(32));
    /// assert_eq!(extract_prefix_length("invalid"), None);
    /// ```
    pub fn extract_prefix_length(cidr: &str) -> Option<u8> {
        cidr.split('/').nth(1)?.parse().ok()
    }

    /// Checks if a prefix length is unusual for an IP version.
    ///
    /// Returns `true` if the prefix length is outside typical ranges:
    /// - IPv4: < 8 or > 24 is unusual
    /// - IPv6: < 32 or > 48 is unusual
    ///
    /// # Examples
    ///
    /// ```
    /// use log_gateway::baseline_model::prefix_features::is_unusual_prefix_length;
    ///
    /// assert!(is_unusual_prefix_length("10.0.0.0/30")); // IPv4 too specific
    /// assert!(!is_unusual_prefix_length("10.0.0.0/16")); // IPv4 normal
    /// assert!(is_unusual_prefix_length("2001:db8::/64")); // IPv6 too specific
    /// assert!(!is_unusual_prefix_length("2001:db8::/40")); // IPv6 normal
    /// ```
    pub fn is_unusual_prefix_length(cidr: &str) -> bool {
        let Some(prefix_len) = extract_prefix_length(cidr) else {
            return false;
        };

        if cidr.contains(':') {
            // IPv6
            !(32..=48).contains(&prefix_len)
        } else {
            // IPv4
            !(8..=24).contains(&prefix_len)
        }
    }
}

/// Tracks AS knowledge (how many days an AS has been seen).
#[derive(Debug, Clone)]
pub struct AsKnowledgeTracker {
    /// Maps AS number → set of days (as YYYY-MM-DD strings) when the AS was seen.
    as_to_days: Arc<DashMap<u32, std::collections::HashSet<String>>>,
}

impl AsKnowledgeTracker {
    /// Creates a new AS knowledge tracker.
    pub fn new() -> Self {
        Self {
            as_to_days: Arc::new(DashMap::new()),
        }
    }

    /// Records that an AS was seen on a specific date.
    ///
    /// # Arguments
    ///
    /// * `asn` - The AS number
    /// * `date` - Date in YYYY-MM-DD format
    pub fn record_as_seen(&self, asn: u32, date: &str) {
        let entry = self.as_to_days.entry(asn);
        let mut day_set = entry.or_insert_with(std::collections::HashSet::new);
        day_set.insert(date.to_string());
    }

    /// Gets the number of distinct days an AS has been seen.
    ///
    /// Returns 0 if the AS has never been recorded.
    pub fn days_seen(&self, asn: u32) -> usize {
        self.as_to_days.get(&asn).map(|set| set.len()).unwrap_or(0)
    }

    /// Checks if an AS is "well-known" (seen on at least 7 different days).
    pub fn is_well_known(&self, asn: u32) -> bool {
        self.days_seen(asn) >= 7
    }

    /// Gets the knowledge score for an AS (0.0 to 1.0).
    ///
    /// The score is min(days_seen / 30, 1.0), where 30 days represents
    /// "very well established".
    pub fn knowledge_score(&self, asn: u32) -> f64 {
        let days = self.days_seen(asn) as f64;
        (days / 30.0).min(1.0)
    }

    /// Gets a reference to the as_to_days DashMap.
    ///
    /// This is used by the ModelTrainer to save snapshots.
    pub fn as_to_days(&self) -> &Arc<DashMap<u32, std::collections::HashSet<String>>> {
        &self.as_to_days
    }
}

impl Default for AsKnowledgeTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ema_update_single() {
        let model = BaselineModel::new(0.1);
        model.update("10.0.0.0/8", 5.0);

        let baseline = model.baselines.get("10.0.0.0/8").unwrap();
        assert_eq!(baseline.ema, 5.0);
        assert_eq!(baseline.sample_count, 1);
    }

    #[test]
    fn test_z_score_none_below_5_samples() {
        let model = BaselineModel::new(0.1);
        model.update("10.0.0.0/8", 5.0);
        model.update("10.0.0.0/8", 6.0);
        model.update("10.0.0.0/8", 5.5);
        model.update("10.0.0.0/8", 5.8); // 4 samples

        assert!(model.z_score("10.0.0.0/8", 10.0).is_none());
    }

    #[test]
    fn test_z_score_some_after_5_samples() {
        let model = BaselineModel::new(0.1);
        model.update("10.0.0.0/8", 5.0);
        model.update("10.0.0.0/8", 6.0);
        model.update("10.0.0.0/8", 5.5);
        model.update("10.0.0.0/8", 5.8);
        model.update("10.0.0.0/8", 5.2); // 5 samples

        let z = model.z_score("10.0.0.0/8", 5.3);
        assert!(z.is_some());
        // Z‑score should be small because value is close to EMA
        let z = z.unwrap();
        assert!(z.abs() < 1.0);
    }

    #[test]
    fn test_is_anomaly_false_for_normal() {
        let model = BaselineModel::new(0.1);
        // Add samples with some variance
        for i in 0..10 {
            model.update("10.0.0.0/8", 5.0 + (i % 3) as f64 * 0.1); // Values: 5.0, 5.1, 5.2, 5.0, 5.1, ...
        }

        // Value within normal range → not anomalous
        assert!(!model.is_anomaly("10.0.0.0/8", 5.15, 3.0));
    }

    #[test]
    fn test_is_anomaly_true_for_outlier() {
        let model = BaselineModel::new(0.1);
        // Add consistent samples
        for _ in 0..10 {
            model.update("10.0.0.0/8", 5.0);
        }

        // Very different value → should be anomalous
        assert!(model.is_anomaly("10.0.0.0/8", 20.0, 3.0));
    }

    #[test]
    fn test_default_alpha() {
        let model = BaselineModel::default();
        // Default alpha should be 0.1
        model.update("10.0.0.0/8", 5.0);
        let baseline = model.baselines.get("10.0.0.0/8").unwrap();
        assert_eq!(baseline.ema, 5.0);
    }

    #[test]
    #[should_panic(expected = "alpha must be in (0, 1]")]
    fn test_invalid_alpha_zero() {
        BaselineModel::new(0.0);
    }

    #[test]
    #[should_panic(expected = "alpha must be in (0, 1]")]
    fn test_invalid_alpha_negative() {
        BaselineModel::new(-0.5);
    }

    #[test]
    #[should_panic(expected = "alpha must be in (0, 1]")]
    fn test_invalid_alpha_gt_one() {
        BaselineModel::new(1.5);
    }

    // Tests for prefix_features
    #[test]
    fn test_extract_prefix_length() {
        use prefix_features::extract_prefix_length;

        assert_eq!(extract_prefix_length("10.0.0.0/8"), Some(8));
        assert_eq!(extract_prefix_length("192.168.1.0/24"), Some(24));
        assert_eq!(extract_prefix_length("2001:db8::/32"), Some(32));
        assert_eq!(
            extract_prefix_length("2001:db8:85a3::8a2e:370:7334/64"),
            Some(64)
        );
        assert_eq!(extract_prefix_length("invalid"), None);
        assert_eq!(extract_prefix_length("10.0.0.0/"), None);
        assert_eq!(extract_prefix_length("10.0.0.0/256"), None); // u8 max is 255
    }

    #[test]
    fn test_is_unusual_prefix_length() {
        use prefix_features::is_unusual_prefix_length;

        // IPv4 tests
        assert!(is_unusual_prefix_length("10.0.0.0/30")); // Too specific
        assert!(is_unusual_prefix_length("10.0.0.0/4")); // Too broad
        assert!(!is_unusual_prefix_length("10.0.0.0/8")); // Normal
        assert!(!is_unusual_prefix_length("10.0.0.0/16")); // Normal
        assert!(!is_unusual_prefix_length("10.0.0.0/24")); // Normal

        // IPv6 tests
        assert!(is_unusual_prefix_length("2001:db8::/64")); // Too specific
        assert!(is_unusual_prefix_length("2001:db8::/16")); // Too broad
        assert!(!is_unusual_prefix_length("2001:db8::/32")); // Normal
        assert!(!is_unusual_prefix_length("2001:db8::/40")); // Normal
        assert!(!is_unusual_prefix_length("2001:db8::/48")); // Normal

        // Invalid prefix
        assert!(!is_unusual_prefix_length("invalid"));
    }

    // Tests for AsKnowledgeTracker
    #[test]
    fn test_as_knowledge_tracker_basic() {
        let tracker = AsKnowledgeTracker::new();

        // Initially, AS should not be known
        assert_eq!(tracker.days_seen(64512), 0);
        assert!(!tracker.is_well_known(64512));
        assert_eq!(tracker.knowledge_score(64512), 0.0);

        // Record AS seen on one day
        tracker.record_as_seen(64512, "2024-01-01");
        assert_eq!(tracker.days_seen(64512), 1);
        assert!(!tracker.is_well_known(64512));
        assert_eq!(tracker.knowledge_score(64512), 1.0 / 30.0);

        // Record same AS on same day (should not duplicate)
        tracker.record_as_seen(64512, "2024-01-01");
        assert_eq!(tracker.days_seen(64512), 1);

        // Record same AS on different day
        tracker.record_as_seen(64512, "2024-01-02");
        assert_eq!(tracker.days_seen(64512), 2);
    }

    #[test]
    fn test_as_knowledge_tracker_well_known() {
        let tracker = AsKnowledgeTracker::new();

        // Record AS seen on 7 different days
        for i in 1..=7 {
            tracker.record_as_seen(64512, &format!("2024-01-{:02}", i));
        }

        assert_eq!(tracker.days_seen(64512), 7);
        assert!(tracker.is_well_known(64512));
        assert_eq!(tracker.knowledge_score(64512), 7.0 / 30.0);
    }

    #[test]
    fn test_as_knowledge_tracker_knowledge_score_capped() {
        let tracker = AsKnowledgeTracker::new();

        // Record AS seen on 40 days (should be capped at 1.0)
        for i in 1..=40 {
            tracker.record_as_seen(64512, &format!("2024-01-{:02}", i));
        }

        assert_eq!(tracker.days_seen(64512), 40);
        assert!(tracker.is_well_known(64512));
        assert_eq!(tracker.knowledge_score(64512), 1.0);
    }

    #[test]
    fn test_as_knowledge_tracker_multiple_as() {
        let tracker = AsKnowledgeTracker::new();

        tracker.record_as_seen(64512, "2024-01-01");
        tracker.record_as_seen(64513, "2024-01-01");
        tracker.record_as_seen(64512, "2024-01-02"); // Same AS, different day
        tracker.record_as_seen(64514, "2024-01-01"); // Different AS

        assert_eq!(tracker.days_seen(64512), 2);
        assert_eq!(tracker.days_seen(64513), 1);
        assert_eq!(tracker.days_seen(64514), 1);
        assert_eq!(tracker.days_seen(99999), 0); // Non-existent AS
    }

    // Tests for BaselineModel new methods
    #[test]
    fn test_record_as_seen() {
        let model = BaselineModel::new(0.1);
        model.record_as_seen(64512, "2024-01-01");
        model.record_as_seen(64512, "2024-01-02");
        model.record_as_seen(64513, "2024-01-01");

        assert_eq!(model.as_knowledge().days_seen(64512), 2);
        assert_eq!(model.as_knowledge().days_seen(64513), 1);
        assert!(!model.as_knowledge().is_well_known(64512));
    }

    #[test]
    fn test_compute_confidence_boost_as_path_anomaly() {
        let model = BaselineModel::new(0.1);

        // Train model with normal AS-path lengths (add some variance)
        for i in 0..10 {
            model.update("10.0.0.0/8", 5.0 + (i % 3) as f64 * 0.1); // Values: 5.0, 5.1, 5.2, 5.0, 5.1, ...
        }

        // Normal AS-path length (5.15) is within normal range - should give 0 boost
        let boost_normal = model.compute_confidence_boost("10.0.0.0/8", 5.15, 64512);
        assert_eq!(boost_normal, 0.0);

        // Anomalous AS-path length (20.0) is far from trained values - should give +0.1 boost
        let boost_anomalous = model.compute_confidence_boost("10.0.0.0/8", 20.0, 64512);
        assert_eq!(boost_anomalous, 0.1);
    }

    #[test]
    fn test_compute_confidence_boost_unusual_prefix() {
        let model = BaselineModel::new(0.1);

        // Unusual prefix length (/30 is too specific for IPv4)
        let boost = model.compute_confidence_boost("10.0.0.0/30", 5.0, 64512);
        assert_eq!(boost, 0.05);

        // Normal prefix length should give 0 boost
        let boost_normal = model.compute_confidence_boost("10.0.0.0/16", 5.0, 64512);
        assert_eq!(boost_normal, 0.0);
    }

    #[test]
    fn test_compute_confidence_boost_well_known_as() {
        let model = BaselineModel::new(0.1);

        // Make AS well-known by recording it on 7+ days
        for i in 1..=7 {
            model.record_as_seen(64512, &format!("2024-01-{:02}", i));
        }

        // Well-known AS should give -0.1 boost
        let boost = model.compute_confidence_boost("10.0.0.0/8", 5.0, 64512);
        assert_eq!(boost, -0.1);
    }

    #[test]
    fn test_compute_confidence_boost_combined() {
        let model = BaselineModel::new(0.1);

        // Train model on the SAME prefix that will be used in the boost call
        // so that EMA data exists for it (unusual prefix /30)
        for _ in 0..10 {
            model.update("10.0.0.0/30", 5.0); // All values exactly 5.0
        }

        // Make AS well-known
        for i in 1..=7 {
            model.record_as_seen(64512, &format!("2024-01-{:02}", i));
        }

        // Test combined: anomalous AS-path (+0.1) + unusual prefix (+0.05) + well-known AS (-0.1) = +0.05
        // Use an extreme value (100.0) to ensure it's detected as anomalous
        let boost = model.compute_confidence_boost("10.0.0.0/30", 100.0, 64512);
        assert!(
            (boost - 0.05).abs() < 1e-10,
            "expected ~0.05, got {}",
            boost
        );

        // Test with cap: anomalous (+0.1) + unusual (+0.05) = +0.15 (capped)
        let model2 = BaselineModel::new(0.1);
        for _ in 0..10 {
            model2.update("10.0.0.0/30", 5.0); // Consistent values on same prefix
        }
        let boost_capped = model2.compute_confidence_boost("10.0.0.0/30", 100.0, 64513);
        assert!(
            (boost_capped - 0.15).abs() < 1e-10,
            "expected ~0.15, got {}",
            boost_capped
        );
    }

    #[test]
    fn test_enhance_confidence() {
        let model = BaselineModel::new(0.1);

        // Train model
        for _ in 0..10 {
            model.update("10.0.0.0/8", 5.0);
        }

        // Base confidence 0.5 with anomalous AS-path (+0.1) = 0.6
        let enhanced = model.enhance_confidence(0.5, "10.0.0.0/8", 20.0, 64512);
        assert_eq!(enhanced, 0.6);

        // Base confidence 0.9 with boost should be capped at 1.0
        let enhanced_capped = model.enhance_confidence(0.9, "10.0.0.0/8", 20.0, 64512);
        assert_eq!(enhanced_capped, 1.0);

        // Base confidence 0.1 with negative boost should be capped at 0.0
        // Make AS well-known first
        for i in 1..=7 {
            model.record_as_seen(64512, &format!("2024-01-{:02}", i));
        }
        let enhanced_negative = model.enhance_confidence(0.05, "10.0.0.0/8", 5.0, 64512);
        assert_eq!(enhanced_negative, 0.0);
    }

    #[test]
    fn test_as_knowledge_accessor() {
        let model = BaselineModel::new(0.1);
        let tracker = model.as_knowledge();

        // Should be able to use tracker methods
        assert_eq!(tracker.days_seen(64512), 0);
        assert!(!tracker.is_well_known(64512));
    }
}

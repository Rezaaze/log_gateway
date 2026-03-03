use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::Serialize;
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing;
use uuid::Uuid;

use crate::baseline_model::BaselineModel;
use crate::bgp_query::ClickHouseQueryClient;
use crate::clickhouse_exporter::BgpClickHouseRecord;
use crate::irr_cache::IrrStatus;
use crate::metrics::GatewayMetrics;
use crate::rpki_cache::{RpkiCache, RpkiStatus};

/// Type of BGP anomaly detected.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub enum AnomalyType {
    PossibleHijack,
    PrefixFlapping,
}

impl std::fmt::Display for AnomalyType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AnomalyType::PossibleHijack => write!(f, "PossibleHijack"),
            AnomalyType::PrefixFlapping => write!(f, "PrefixFlapping"),
        }
    }
}

/// A detected BGP anomaly.
#[derive(Debug, Clone, Serialize)]
pub struct Anomaly {
    pub id: Uuid,
    pub anomaly_type: AnomalyType,
    pub prefix: String,
    pub origin_as: u32,
    pub confidence: f64,
    pub detected_at: DateTime<Utc>,
    pub details: String,
    pub tenant_id: String,
}

/// Trait for BGP anomaly detectors.
pub trait Detector: Send + Sync {
    /// Returns the detector's name for logging.
    fn name(&self) -> &'static str;

    /// Checks a BGP event for anomalies.
    ///
    /// This method is called on the hot ingest path and must be synchronous
    /// (no async, no .await).
    fn check(&self, event: &BgpClickHouseRecord) -> Option<Anomaly>;
}

/// Detects possible BGP hijacks by tracking which ASNs announce each prefix.
#[derive(Debug, Default)]
pub struct HijackDetector {
    /// Maps prefix → set of known origin ASNs.
    prefix_to_asns: DashMap<String, HashSet<u32>>,
}

impl HijackDetector {
    /// Creates a new hijack detector with empty state.
    pub fn new() -> Self {
        Self {
            prefix_to_asns: DashMap::new(),
        }
    }

    /// Warms up the detector with historical data from ClickHouse.
    ///
    /// Queries the last 7 days of BGP events to learn which ASNs have been
    /// seen announcing each prefix.
    pub async fn warmup(&self, query_client: &ClickHouseQueryClient) {
        // Local struct for deserializing ClickHouse query results.
        #[derive(serde::Deserialize)]
        struct WarmupRow {
            prefix: String,
            known_as: Vec<u32>,
        }

        let sql = "\
            SELECT prefix, groupUniqArray(origin_as) AS known_as \
            FROM bgp_events \
            WHERE timestamp >= now() - INTERVAL 7 DAY \
            GROUP BY prefix";

        match query_client.query::<WarmupRow>(sql).await {
            Ok(rows) => {
                for row in rows {
                    let set: HashSet<u32> = row.known_as.into_iter().collect();
                    self.prefix_to_asns.insert(row.prefix, set);
                }
                tracing::info!(
                    "HijackDetector warmup complete: loaded {} prefixes",
                    self.prefix_to_asns.len()
                );
            }
            Err(e) => {
                tracing::warn!("HijackDetector warmup failed: {}", e);
                // Do NOT panic, do NOT fail startup.
            }
        }
    }

    /// Checks a BGP event for hijacks with pre-fetched RPKI and IRR status.
    ///
    /// This is an async enrichment layer that runs after the synchronous check().
    /// The caller is responsible for fetching the RPKI status (and recording metrics)
    /// before calling this method — avoids double-querying Routinator.
    pub fn check_with_rpki_status(
        &self,
        event: &BgpClickHouseRecord,
        rpki_status: RpkiStatus,
        irr_status: &IrrStatus,
    ) -> Option<Anomaly> {
        // Skip withdraw events for hijack detection
        if event.event_type == "withdraw" {
            return None;
        }

        // Run the synchronous check
        let mut anomaly = self.check(event);

        // Helper: check if IRR signals inconsistency
        let irr_inconsistent = matches!(irr_status, IrrStatus::Inconsistent { .. });

        match (anomaly.take(), rpki_status) {
            // Case 1: No anomaly from check() (known AS) but RPKI invalid → Warning
            (None, RpkiStatus::InvalidAsn | RpkiStatus::InvalidLength) => {
                // Known AS but RPKI invalid → adjust confidence based on IRR
                let (confidence, irr_note) = if irr_inconsistent {
                    (0.75, " (IRR: also inconsistent)")
                } else {
                    (0.60, "")
                };
                Some(Anomaly {
                    id: Uuid::new_v4(),
                    anomaly_type: AnomalyType::PossibleHijack,
                    prefix: event.prefix.clone(),
                    origin_as: event.origin_as,
                    confidence,
                    detected_at: Utc::now(),
                    details: format!(
                        "RPKI INVALID: AS{} announced {} — ROA violation (known AS, possible misconfiguration){}",
                        event.origin_as, event.prefix, irr_note
                    ),
                    tenant_id: event.tenant_id.clone(),
                })
            }
            // Case 2: No anomaly and RPKI is valid/not-found/unavailable → no alert
            (None, _) => None,

            // Case 3: Anomaly detected (new AS) with RPKI and IRR validation
            (Some(mut anomaly), rpki_status) => {
                match rpki_status {
                    RpkiStatus::Valid => {
                        // RPKI valid → lower confidence (may be legitimate new origin)
                        anomaly.confidence = 0.3;
                        anomaly
                            .details
                            .push_str(" (RPKI: Valid — may be legitimate)");
                    }
                    RpkiStatus::InvalidAsn | RpkiStatus::InvalidLength => {
                        // RPKI invalid → higher confidence, boosted further if IRR inconsistent
                        anomaly.confidence = if irr_inconsistent { 0.99 } else { 0.97 };
                        anomaly.details.push_str(" (RPKI: INVALID)");
                        if irr_inconsistent {
                            anomaly.details.push_str(" (IRR: also inconsistent)");
                        }
                    }
                    RpkiStatus::NotFound | RpkiStatus::Unavailable => {
                        // No ROA or RPKI unavailable → adjust confidence if IRR inconsistent
                        if irr_inconsistent {
                            anomaly.confidence = 0.90;
                            anomaly.details.push_str(" (IRR: inconsistent)");
                        }
                        // otherwise keep original confidence (0.85)
                    }
                }
                Some(anomaly)
            }
        }
    }
}

impl Detector for HijackDetector {
    fn name(&self) -> &'static str {
        "HijackDetector"
    }

    fn check(&self, event: &BgpClickHouseRecord) -> Option<Anomaly> {
        // Skip withdraw events for hijack detection
        if event.event_type == "withdraw" {
            return None;
        }

        let prefix = &event.prefix;
        let origin_as = event.origin_as;

        // Get or insert the set of known ASNs for this prefix.
        let entry = self.prefix_to_asns.entry(prefix.clone());
        let mut asn_set = entry.or_insert_with(HashSet::new);

        // If this ASN is already known, it's legitimate.
        if asn_set.contains(&origin_as) {
            return None;
        }

        // New ASN for this prefix → possible hijack.
        asn_set.insert(origin_as);

        // Format details string.
        let known_asns: Vec<u32> = asn_set.iter().copied().collect();
        let details = format!(
            "AS{} announced {} but known originators are: {:?}",
            origin_as, prefix, known_asns
        );

        Some(Anomaly {
            id: Uuid::new_v4(),
            anomaly_type: AnomalyType::PossibleHijack,
            prefix: prefix.clone(),
            origin_as,
            confidence: 0.85,
            detected_at: Utc::now(),
            details,
            tenant_id: event.tenant_id.clone(),
        })
    }
}

impl Detector for Arc<HijackDetector> {
    fn name(&self) -> &'static str {
        "HijackDetector"
    }

    fn check(&self, event: &BgpClickHouseRecord) -> Option<Anomaly> {
        (**self).check(event)
    }
}

/// Detects prefix flapping (rapid announcements/withdrawals).
#[derive(Debug, Default)]
pub struct FlappingDetector {
    /// Maps prefix → recent event timestamps within the sliding window.
    prefix_to_timestamps: DashMap<String, VecDeque<DateTime<Utc>>>,
}

impl FlappingDetector {
    /// Creates a new flapping detector.
    pub fn new() -> Self {
        Self {
            prefix_to_timestamps: DashMap::new(),
        }
    }
}

impl Detector for FlappingDetector {
    fn name(&self) -> &'static str {
        "FlappingDetector"
    }

    fn check(&self, event: &BgpClickHouseRecord) -> Option<Anomaly> {
        const FLAP_WINDOW_SECS: u64 = 300; // 5-minute window
        const FLAP_THRESHOLD: usize = 10; // 10 events = flapping

        let prefix = &event.prefix;
        let now = Utc::now();

        // Get or create the timestamp queue for this prefix.
        let entry = self.prefix_to_timestamps.entry(prefix.clone());
        let mut timestamps = entry.or_insert_with(VecDeque::new);

        // Add current event timestamp.
        timestamps.push_back(event.timestamp);

        // Remove timestamps outside the sliding window.
        let cutoff = now - chrono::Duration::seconds(FLAP_WINDOW_SECS as i64);
        while timestamps.front().is_some_and(|&ts| ts < cutoff) {
            timestamps.pop_front();
        }

        // Check if we've exceeded the threshold.
        if timestamps.len() >= FLAP_THRESHOLD {
            let details = format!(
                "Prefix {} had {} events in the last {} seconds",
                prefix,
                timestamps.len(),
                FLAP_WINDOW_SECS
            );

            Some(Anomaly {
                id: Uuid::new_v4(),
                anomaly_type: AnomalyType::PrefixFlapping,
                prefix: prefix.clone(),
                origin_as: event.origin_as,
                confidence: 0.75,
                detected_at: now,
                details,
                tenant_id: event.tenant_id.clone(),
            })
        } else {
            None
        }
    }
}

/// Orchestrates multiple detectors and sends anomalies to an alert channel.
pub struct AnomalyDetector {
    detectors: Vec<Box<dyn Detector>>,
    hijack_detector: Arc<HijackDetector>,
    baseline: Arc<BaselineModel>,
    alert_tx: mpsc::Sender<Anomaly>,
    /// Optional metrics for A/B testing
    metrics: Option<Arc<GatewayMetrics>>,
}

impl std::fmt::Debug for AnomalyDetector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnomalyDetector")
            .field("detectors", &self.detectors.len())
            .field("hijack_detector", &self.hijack_detector)
            .field("baseline", &self.baseline)
            .field("alert_tx", &"mpsc::Sender<Anomaly>")
            .finish()
    }
}

impl AnomalyDetector {
    /// Creates a new anomaly detector with built‑in hijack and flapping detectors.
    pub fn new(alert_tx: mpsc::Sender<Anomaly>) -> Self {
        let hijack_detector = Arc::new(HijackDetector::new());
        let flapping_detector = Box::new(FlappingDetector::new());
        let baseline = Arc::new(BaselineModel::default());

        // Store the hijack detector both as Arc for warmup and as a boxed trait object.
        let hijack_boxed: Box<dyn Detector> = Box::new(Arc::clone(&hijack_detector));

        Self {
            detectors: vec![hijack_boxed, flapping_detector],
            hijack_detector,
            baseline,
            alert_tx,
            metrics: None,
        }
    }

    /// Creates a new anomaly detector with metrics for A/B testing.
    pub fn with_metrics(alert_tx: mpsc::Sender<Anomaly>, metrics: Arc<GatewayMetrics>) -> Self {
        let hijack_detector = Arc::new(HijackDetector::new());
        let flapping_detector = Box::new(FlappingDetector::new());
        let baseline = Arc::new(BaselineModel::default());

        // Store the hijack detector both as Arc for warmup and as a boxed trait object.
        let hijack_boxed: Box<dyn Detector> = Box::new(Arc::clone(&hijack_detector));

        Self {
            detectors: vec![hijack_boxed, flapping_detector],
            hijack_detector,
            baseline,
            alert_tx,
            metrics: Some(metrics),
        }
    }

    /// Returns a reference to the hijack detector for warmup.
    pub fn hijack_detector(&self) -> &HijackDetector {
        &self.hijack_detector
    }

    /// Returns a clone of the alert channel sender.
    pub fn alert_tx(&self) -> mpsc::Sender<Anomaly> {
        self.alert_tx.clone()
    }

    /// Returns an Arc clone of the hijack detector.
    pub fn hijack_detector_arc(&self) -> Arc<HijackDetector> {
        Arc::clone(&self.hijack_detector)
    }

    /// Returns an Arc clone of the baseline model (for external training/retraining).
    pub fn baseline_arc(&self) -> Arc<BaselineModel> {
        Arc::clone(&self.baseline)
    }

    /// Checks a BGP event with all detectors and sends any anomalies.
    ///
    /// This method is synchronous and must not block the ingest path.
    pub fn check(&self, event: &BgpClickHouseRecord) {
        // Update baseline model with AS-path length
        let as_path_len = event.as_path.len() as f64;
        self.baseline.update(&event.prefix, as_path_len);

        for detector in &self.detectors {
            if let Some(mut anomaly) = detector.check(event) {
                // Save rule-based confidence before enhancement
                let rule_based_confidence = anomaly.confidence;

                // Enhance confidence using multiple features: AS-path length,
                // prefix length, and AS knowledge
                let ml_confidence = self.baseline.enhance_confidence(
                    rule_based_confidence,
                    &event.prefix,
                    as_path_len,
                    event.origin_as,
                );

                // A/B logging: both values for observability
                tracing::debug!(
                    rule_based = rule_based_confidence,
                    ml_enhanced = ml_confidence,
                    anomaly_type = %anomaly.anomaly_type,
                    prefix = %event.prefix,
                    "ab_test_confidence"
                );

                // Record A/B confidence + anomaly count metrics
                if let Some(ref metrics) = self.metrics {
                    metrics.record_ab_confidence(
                        &anomaly.anomaly_type.to_string(),
                        rule_based_confidence,
                        ml_confidence,
                    );
                    metrics.record_anomaly_detected(&anomaly.anomaly_type.to_string());
                }

                // Production decision: use ML-enhanced confidence (as before)
                anomaly.confidence = ml_confidence;

                // Try to send without blocking; drop the anomaly if the channel is full.
                if let Err(e) = self.alert_tx.try_send(anomaly) {
                    tracing::warn!(
                        "Alert channel full, dropping anomaly from {}: {}",
                        detector.name(),
                        e
                    );
                }
            }
        }
    }
}

/// Helper function to check anomaly against rules and route if threshold is met.
async fn check_rules_and_route(
    anomaly: &Anomaly,
    cached_rules: &[crate::alert_manager::AlertRule],
    escalation_router: &Option<Arc<crate::escalation::EscalationRouter>>,
    metrics: &Arc<GatewayMetrics>,
    current_tenant_id: Option<&str>,
) {
    // Check if anomaly.confidence >= threshold of any rule
    // Note: In 3.1.1-C we'll add rule_type matching (rule_type vs. AnomalyType)
    for rule in cached_rules {
        // Check tenant filter: rule applies if tenant_id is None (global) or matches current tenant
        let tenant_matches =
            rule.tenant_id.is_none() || rule.tenant_id.as_deref() == current_tenant_id;

        if tenant_matches && anomaly.confidence >= rule.threshold {
            tracing::info!(
                "Anomaly confidence {} meets rule '{}' threshold {} (tenant: {:?})",
                anomaly.confidence,
                rule.name,
                rule.threshold,
                current_tenant_id
            );
            // Route through escalation router if available
            if let Some(router) = escalation_router {
                router.route(anomaly).await;
            }
            // Increment metric for rule-triggered alerts
            metrics.record_alert_rule_triggered();
            break; // Only need to trigger once per anomaly
        }
    }
}

/// Async task that receives anomalies and logs them, incrementing metrics.
pub async fn run_alert_logger(
    mut rx: mpsc::Receiver<Anomaly>,
    metrics: Arc<GatewayMetrics>,
    escalation_router: Option<Arc<crate::escalation::EscalationRouter>>,
    // NEU:
    alert_manager: Option<Arc<crate::alert_manager::AlertManagerClient>>,
    mut reload_rx: tokio::sync::watch::Receiver<()>,
    _tenant_id: Option<String>,
) {
    tracing::info!("Alert logger task started");

    // Beim Start: Rules laden und in lokale Variable cachen
    let mut cached_rules: Vec<crate::alert_manager::AlertRule> = vec![];
    if let Some(ref am) = alert_manager {
        match am.list_rules().await {
            Ok(rules) => {
                cached_rules = rules;
                tracing::info!("Loaded {} alert rules on startup", cached_rules.len());
            }
            Err(e) => tracing::warn!("Failed to load alert rules on startup: {}", e),
        }
    }

    loop {
        tokio::select! {
            anomaly = rx.recv() => {
                let Some(anomaly) = anomaly else { break; };
                // Log the anomaly.
                tracing::warn!(
                    "BGP anomaly detected: {:?} prefix={} origin_as={} confidence={}",
                    anomaly.anomaly_type,
                    anomaly.prefix,
                    anomaly.origin_as,
                    anomaly.confidence
                );

                // Increment the appropriate metric.
                match anomaly.anomaly_type {
                    AnomalyType::PossibleHijack => metrics.record_bgp_anomaly_hijack(),
                    AnomalyType::PrefixFlapping => metrics.record_bgp_anomaly_flap(),
                }

                // Prüfe ob anomaly.confidence >= threshold einer Rule
                check_rules_and_route(&anomaly, &cached_rules, &escalation_router, &metrics, Some(&anomaly.tenant_id)).await;
            }
            _ = reload_rx.changed() => {
                // SIGHUP: Rules neu laden
                if let Some(ref am) = alert_manager {
                    match am.list_rules().await {
                        Ok(rules) => {
                            tracing::info!("Alert rules reloaded: {} rules active", rules.len());
                            cached_rules = rules;
                        }
                        Err(e) => tracing::warn!("Alert rules reload failed: {}", e),
                    }
                }
            }
        }
    }

    tracing::info!("Alert logger task finished");
}

/// Async task that enriches BGP events with RPKI and IRR validation.
///
/// This runs parallel to the existing anomaly detection pipeline.
/// It receives BGP records, validates them against RPKI and IRR, and sends
/// enriched anomalies to the alert channel.
pub async fn run_rpki_enrichment(
    mut rx: tokio::sync::mpsc::Receiver<BgpClickHouseRecord>,
    hijack_detector: Arc<HijackDetector>,
    rpki_cache: Arc<RpkiCache>,
    irr_cache: Option<Arc<crate::irr_cache::IrrCache>>,
    alert_tx: tokio::sync::mpsc::Sender<Anomaly>,
    metrics: Arc<GatewayMetrics>,
) {
    tracing::info!("RPKI+IRR enrichment task started");

    while let Some(record) = rx.recv().await {
        // Only process ANNOUNCE events (skip withdraw for RPKI enrichment)
        if record.event_type != "announce" {
            continue;
        }

        // Validate against RPKI once — reuse the result for both metrics and
        // hijack detection to avoid querying Routinator twice per event.
        let rpki_status = rpki_cache.validate(&record.prefix, record.origin_as).await;
        match &rpki_status {
            RpkiStatus::Valid => metrics.record_rpki_valid(),
            RpkiStatus::InvalidAsn | RpkiStatus::InvalidLength => metrics.record_rpki_invalid(),
            RpkiStatus::NotFound | RpkiStatus::Unavailable => {}
        }

        // IRR-Check — only if cache is available, otherwise Unavailable
        let irr_status = if let Some(ref irr) = irr_cache {
            irr.check(&record.prefix, record.origin_as).await
        } else {
            crate::irr_cache::IrrStatus::Unavailable
        };

        // Check for hijacks using the already-fetched RPKI and IRR status
        if let Some(anomaly) =
            hijack_detector.check_with_rpki_status(&record, rpki_status, &irr_status)
        {
            // Try to send without blocking
            if let Err(e) = alert_tx.try_send(anomaly) {
                tracing::warn!(
                    "Alert channel full, dropping RPKI+IRR-enriched anomaly: {}",
                    e
                );
            }
        }
    }

    tracing::info!("RPKI+IRR enrichment task finished");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpki_cache::RpkiStatus;

    // Helper function to create a BGP event for testing
    fn make_event(prefix: &str, origin_as: u32, event_type: &str) -> BgpClickHouseRecord {
        BgpClickHouseRecord {
            timestamp: Utc::now(),
            prefix: prefix.to_string(),
            origin_as,
            event_type: event_type.to_string(),
            peer_ip: "192.0.2.1".to_string(),
            peer_asn: 64512,
            as_path: vec![64512, origin_as],
            community: vec![],
            source: "test".to_string(),
            tenant_id: "test-tenant".to_string(),
        }
    }

    // Test 1: Bekanntes AS + RPKI invalid + IRR inconsistent → confidence 0.75
    #[test]
    fn test_check_rpki_invalid_irr_inconsistent_known_as() {
        let detector = HijackDetector::new();
        let event = make_event("1.2.3.0/24", 64512, "announce");

        // Simulate known AS by adding it to the detector's state
        detector
            .prefix_to_asns
            .entry("1.2.3.0/24".to_string())
            .or_insert_with(HashSet::new)
            .insert(64512);

        let rpki_status = RpkiStatus::InvalidAsn;
        let irr_status = IrrStatus::Inconsistent {
            irr_asns: vec![64513, 64514],
        };

        let anomaly = detector.check_with_rpki_status(&event, rpki_status, &irr_status);
        assert!(anomaly.is_some());
        let anomaly = anomaly.unwrap();
        assert_eq!(anomaly.confidence, 0.75);
        assert!(anomaly.details.contains("IRR: also inconsistent"));
    }

    // Test 2: Neues AS + RPKI invalid + IRR inconsistent → confidence 0.99
    #[test]
    fn test_check_rpki_invalid_irr_inconsistent_new_as() {
        let detector = HijackDetector::new();
        let event = make_event("1.2.3.0/24", 64512, "announce");

        let rpki_status = RpkiStatus::InvalidAsn;
        let irr_status = IrrStatus::Inconsistent {
            irr_asns: vec![64513, 64514],
        };

        let anomaly = detector.check_with_rpki_status(&event, rpki_status, &irr_status);
        assert!(anomaly.is_some());
        let anomaly = anomaly.unwrap();
        assert_eq!(anomaly.confidence, 0.99);
        assert!(anomaly.details.contains("IRR: also inconsistent"));
    }

    // Test 3: Neues AS + RPKI not-found + IRR inconsistent → confidence 0.90
    #[test]
    fn test_check_rpki_notfound_irr_inconsistent() {
        let detector = HijackDetector::new();
        let event = make_event("1.2.3.0/24", 64512, "announce");

        let rpki_status = RpkiStatus::NotFound;
        let irr_status = IrrStatus::Inconsistent {
            irr_asns: vec![64513, 64514],
        };

        let anomaly = detector.check_with_rpki_status(&event, rpki_status, &irr_status);
        assert!(anomaly.is_some());
        let anomaly = anomaly.unwrap();
        assert_eq!(anomaly.confidence, 0.90);
        assert!(anomaly.details.contains("IRR: inconsistent"));
    }

    // Test 4: Neues AS + RPKI not-found + IRR consistent → confidence 0.85 (unverändert)
    #[test]
    fn test_check_rpki_notfound_irr_consistent_no_boost() {
        let detector = HijackDetector::new();
        let event = make_event("1.2.3.0/24", 64512, "announce");

        let rpki_status = RpkiStatus::NotFound;
        let irr_status = IrrStatus::Consistent;

        let anomaly = detector.check_with_rpki_status(&event, rpki_status, &irr_status);
        assert!(anomaly.is_some());
        let anomaly = anomaly.unwrap();
        assert_eq!(anomaly.confidence, 0.85); // Default confidence for new AS
        assert!(!anomaly.details.contains("IRR"));
    }

    // Test 5: Bekanntes AS + RPKI invalid + IRR consistent → confidence 0.60 (unverändert)
    #[test]
    fn test_check_rpki_invalid_irr_consistent_known_as() {
        let detector = HijackDetector::new();
        let event = make_event("1.2.3.0/24", 64512, "announce");

        // Simulate known AS by adding it to the detector's state
        detector
            .prefix_to_asns
            .entry("1.2.3.0/24".to_string())
            .or_insert_with(HashSet::new)
            .insert(64512);

        let rpki_status = RpkiStatus::InvalidAsn;
        let irr_status = IrrStatus::Consistent;

        let anomaly = detector.check_with_rpki_status(&event, rpki_status, &irr_status);
        assert!(anomaly.is_some());
        let anomaly = anomaly.unwrap();
        assert_eq!(anomaly.confidence, 0.60);
        assert!(!anomaly.details.contains("IRR"));
    }

    // Test 6: Neues AS + RPKI valid + IRR inconsistent → confidence 0.30 (IRR doesn't matter when RPKI valid)
    #[test]
    fn test_check_rpki_valid_irr_inconsistent_new_as() {
        let detector = HijackDetector::new();
        let event = make_event("1.2.3.0/24", 64512, "announce");

        let rpki_status = RpkiStatus::Valid;
        let irr_status = IrrStatus::Inconsistent {
            irr_asns: vec![64513, 64514],
        };

        let anomaly = detector.check_with_rpki_status(&event, rpki_status, &irr_status);
        assert!(anomaly.is_some());
        let anomaly = anomaly.unwrap();
        assert_eq!(anomaly.confidence, 0.30);
        assert!(!anomaly.details.contains("IRR")); // IRR note not added when RPKI valid
    }

    // Test 7: Withdraw event should return None regardless of RPKI/IRR status
    #[test]
    fn test_withdraw_event_returns_none() {
        let detector = HijackDetector::new();
        let event = make_event("1.2.3.0/24", 64512, "withdraw");

        let rpki_status = RpkiStatus::InvalidAsn;
        let irr_status = IrrStatus::Inconsistent {
            irr_asns: vec![64513, 64514],
        };

        let anomaly = detector.check_with_rpki_status(&event, rpki_status, &irr_status);
        assert!(anomaly.is_none());
    }

    // ── run_alert_logger tests ────────────────────────────────────────────────

    #[tokio::test]
    async fn test_run_alert_logger_loads_no_rules_without_manager() {
        // When alert_manager = None → cached_rules bleibt leer, kein Fehler
        let (tx, rx) = mpsc::channel::<Anomaly>(8);
        let metrics = Arc::new(crate::metrics::GatewayMetrics::new());
        let (_reload_tx, reload_rx) = tokio::sync::watch::channel(());

        // Task starten
        let handle = tokio::spawn(run_alert_logger(
            rx, metrics, None, // kein EscalationRouter
            None, // kein AlertManager
            reload_rx, None, // tenant_id
        ));

        // Sender droppen → Task beendet sich sauber
        drop(tx);
        // Sollte ohne Panic beenden
        handle.await.expect("alert_logger panicked");
    }

    #[tokio::test]
    async fn test_run_alert_logger_continues_on_reload_signal_without_manager() {
        // reload_rx feuert, aber kein AlertManager → kein Fehler, Task läuft weiter
        let (tx, rx) = mpsc::channel::<Anomaly>(8);
        let metrics = Arc::new(crate::metrics::GatewayMetrics::new());
        let (reload_tx, reload_rx) = tokio::sync::watch::channel(());

        let handle = tokio::spawn(run_alert_logger(
            rx, metrics, None, None, reload_rx, None, // tenant_id
        ));

        // Reload-Signal senden — kein AlertManager → warn! aber kein Absturz
        reload_tx.send(()).unwrap();

        // Kurz warten, dann Sender droppen
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        drop(tx);
        handle
            .await
            .expect("alert_logger panicked after reload signal");
    }

    #[tokio::test]
    async fn test_run_alert_logger_processes_anomaly_without_rules() {
        // Anomalie kommt an, keine Rules geladen → nur Metric-Increment, kein Panic
        let (tx, rx) = mpsc::channel::<Anomaly>(8);
        let metrics = Arc::new(crate::metrics::GatewayMetrics::new());
        let (_reload_tx, reload_rx) = tokio::sync::watch::channel(());

        let handle = tokio::spawn(run_alert_logger(
            rx,
            Arc::clone(&metrics),
            None,
            None,
            reload_rx,
            None, // tenant_id
        ));

        let anomaly = Anomaly {
            id: uuid::Uuid::new_v4(),
            anomaly_type: AnomalyType::PossibleHijack,
            prefix: "10.0.0.0/8".to_string(),
            origin_as: 64512,
            confidence: 0.95,
            detected_at: Utc::now(),
            details: "test".to_string(),
            tenant_id: "test-tenant".to_string(),
        };

        tx.send(anomaly).await.unwrap();
        drop(tx);
        handle.await.expect("alert_logger panicked on anomaly");
    }

    // ── Tenant-based alert rule tests ─────────────────────────────────────────

    #[test]
    fn test_rule_with_matching_tenant_fires() {
        // Create a mock rule with tenant_id="t1"
        let rule = crate::alert_manager::AlertRule {
            id: uuid::Uuid::new_v4(),
            name: "Test Rule".to_string(),
            description: "Test".to_string(),
            rule_type: "hijack".to_string(),
            threshold: 0.5,
            enabled: true,
            tenant_id: Some("t1".to_string()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        // Create an anomaly with tenant_id="t1" and confidence above threshold
        let anomaly = Anomaly {
            id: uuid::Uuid::new_v4(),
            anomaly_type: AnomalyType::PossibleHijack,
            prefix: "10.0.0.0/8".to_string(),
            origin_as: 64512,
            confidence: 0.8, // Above threshold
            detected_at: Utc::now(),
            details: "test".to_string(),
            tenant_id: "t1".to_string(),
        };

        // Rule should match because tenant_id matches
        let tenant_matches =
            rule.tenant_id.is_none() || rule.tenant_id.as_deref() == Some(&anomaly.tenant_id);
        assert!(tenant_matches);
        assert!(anomaly.confidence >= rule.threshold);
    }

    #[test]
    fn test_rule_with_different_tenant_skips() {
        // Create a mock rule with tenant_id="t1"
        let rule = crate::alert_manager::AlertRule {
            id: uuid::Uuid::new_v4(),
            name: "Test Rule".to_string(),
            description: "Test".to_string(),
            rule_type: "hijack".to_string(),
            threshold: 0.5,
            enabled: true,
            tenant_id: Some("t1".to_string()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        // Create an anomaly with tenant_id="t2" (different tenant)
        let anomaly = Anomaly {
            id: uuid::Uuid::new_v4(),
            anomaly_type: AnomalyType::PossibleHijack,
            prefix: "10.0.0.0/8".to_string(),
            origin_as: 64512,
            confidence: 0.8, // Above threshold
            detected_at: Utc::now(),
            details: "test".to_string(),
            tenant_id: "t2".to_string(),
        };

        // Rule should NOT match because tenant_id doesn't match
        let tenant_matches =
            rule.tenant_id.is_none() || rule.tenant_id.as_deref() == Some(&anomaly.tenant_id);
        assert!(!tenant_matches); // Should be false
                                  // Even though confidence is above threshold, tenant doesn't match
    }

    #[test]
    fn test_global_rule_fires_for_any_tenant() {
        // Create a mock rule with tenant_id=None (global rule)
        let rule = crate::alert_manager::AlertRule {
            id: uuid::Uuid::new_v4(),
            name: "Global Rule".to_string(),
            description: "Global test".to_string(),
            rule_type: "hijack".to_string(),
            threshold: 0.5,
            enabled: true,
            tenant_id: None, // Global rule
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        // Test with anomaly from tenant "t1"
        let anomaly_t1 = Anomaly {
            id: uuid::Uuid::new_v4(),
            anomaly_type: AnomalyType::PossibleHijack,
            prefix: "10.0.0.0/8".to_string(),
            origin_as: 64512,
            confidence: 0.8,
            detected_at: Utc::now(),
            details: "test".to_string(),
            tenant_id: "t1".to_string(),
        };

        // Test with anomaly from tenant "t2"
        let anomaly_t2 = Anomaly {
            id: uuid::Uuid::new_v4(),
            anomaly_type: AnomalyType::PossibleHijack,
            prefix: "20.0.0.0/8".to_string(),
            origin_as: 64513,
            confidence: 0.8,
            detected_at: Utc::now(),
            details: "test".to_string(),
            tenant_id: "t2".to_string(),
        };

        // Global rule should match both tenants
        let tenant_matches_t1 =
            rule.tenant_id.is_none() || rule.tenant_id.as_deref() == Some(&anomaly_t1.tenant_id);
        let tenant_matches_t2 =
            rule.tenant_id.is_none() || rule.tenant_id.as_deref() == Some(&anomaly_t2.tenant_id);

        assert!(tenant_matches_t1);
        assert!(tenant_matches_t2);
        assert!(anomaly_t1.confidence >= rule.threshold);
        assert!(anomaly_t2.confidence >= rule.threshold);
    }

    #[test]
    fn test_alert_rule_create_with_tenant() {
        // Test serialization/deserialization of AlertRuleCreate with tenant_id
        let rule_create = crate::alert_manager::AlertRuleCreate {
            name: "Test Rule".to_string(),
            description: "Test".to_string(),
            rule_type: "hijack".to_string(),
            threshold: 0.7,
            tenant_id: Some("t1".to_string()),
        };

        // Serialize to JSON
        let json = serde_json::to_string(&rule_create).expect("Serialization should succeed");

        // Deserialize back
        let deserialized: crate::alert_manager::AlertRuleCreate =
            serde_json::from_str(&json).expect("Deserialization should succeed");

        assert_eq!(deserialized.name, "Test Rule");
        assert_eq!(deserialized.tenant_id, Some("t1".to_string()));

        // Test with None tenant_id (global rule)
        let global_rule_create = crate::alert_manager::AlertRuleCreate {
            name: "Global Rule".to_string(),
            description: "Global".to_string(),
            rule_type: "hijack".to_string(),
            threshold: 0.7,
            tenant_id: None,
        };

        let json_global =
            serde_json::to_string(&global_rule_create).expect("Serialization should succeed");
        let deserialized_global: crate::alert_manager::AlertRuleCreate =
            serde_json::from_str(&json_global).expect("Deserialization should succeed");

        assert_eq!(deserialized_global.tenant_id, None);
    }

    // Test baseline model integration
    #[test]
    fn test_baseline_model_integration() {
        // Create anomaly detector
        let (alert_tx, mut alert_rx) = mpsc::channel::<Anomaly>(10);
        let detector = AnomalyDetector::new(alert_tx);

        // Create a BGP event with normal AS-path length
        let mut event = make_event("10.0.0.0/8", 64512, "announce");

        // First few events with normal AS-path length (2-3 hops)
        for i in 0..10 {
            event.as_path = vec![64512, 64513, 64514 + i % 2]; // 2-3 hops
            detector.check(&event);
        }

        // Now create an anomaly with very long AS-path
        event.as_path = vec![64512, 64513, 64514, 64515, 64516, 64517, 64518, 64519]; // 8 hops
        detector.check(&event);

        // Check if an alert was sent
        let anomaly = alert_rx.try_recv();
        assert!(anomaly.is_ok());
        let anomaly = anomaly.unwrap();

        // The anomaly should have boosted confidence due to anomalous AS-path length
        // Base confidence is 0.85 for new AS, boosted by 0.1 to 0.95
        assert!(anomaly.confidence >= 0.85);
        assert!(anomaly.confidence <= 0.95);
    }

    #[test]
    fn test_ab_confidence_with_metrics() {
        // Create metrics
        let metrics = Arc::new(crate::metrics::GatewayMetrics::new());

        // Create anomaly detector with metrics
        let (alert_tx, mut alert_rx) = mpsc::channel::<Anomaly>(10);
        let detector = AnomalyDetector::with_metrics(alert_tx, Arc::clone(&metrics));

        // Create a BGP event that will trigger an anomaly
        let event = make_event("10.0.0.0/8", 64512, "announce");

        // Check the event
        detector.check(&event);

        // Check if an alert was sent
        let anomaly = alert_rx.try_recv();
        assert!(anomaly.is_ok());

        // Metrics should have been recorded (no panic)
        let rendered = metrics.render();
        assert!(rendered.contains("gateway_rule_based_confidence"));
        assert!(rendered.contains("gateway_ml_enhanced_confidence"));
    }

    #[test]
    fn test_ab_confidence_without_metrics() {
        // Create anomaly detector without metrics
        let (alert_tx, mut alert_rx) = mpsc::channel::<Anomaly>(10);
        let detector = AnomalyDetector::new(alert_tx);

        // Create a BGP event that will trigger an anomaly
        let event = make_event("10.0.0.0/8", 64512, "announce");

        // Check the event - should not panic even without metrics
        detector.check(&event);

        // Check if an alert was sent
        let anomaly = alert_rx.try_recv();
        assert!(anomaly.is_ok());
    }

    #[test]
    fn test_anomaly_type_display() {
        // Test Display implementation for AnomalyType
        let hijack = AnomalyType::PossibleHijack;
        let flapping = AnomalyType::PrefixFlapping;

        assert_eq!(hijack.to_string(), "PossibleHijack");
        assert_eq!(flapping.to_string(), "PrefixFlapping");
    }

    #[test]
    fn test_anomaly_detected_called_in_check() {
        // Create metrics
        let metrics = Arc::new(crate::metrics::GatewayMetrics::new());

        // Create anomaly detector with metrics
        let (alert_tx, mut alert_rx) = mpsc::channel::<Anomaly>(10);
        let detector = AnomalyDetector::with_metrics(alert_tx, Arc::clone(&metrics));

        // Create a BGP event that will trigger an anomaly
        let event = make_event("10.0.0.0/8", 64512, "announce");

        // Check the event
        detector.check(&event);

        // Check if an alert was sent
        let anomaly = alert_rx.try_recv();
        assert!(anomaly.is_ok());

        // Check that metrics were recorded
        let rendered = metrics.render();
        assert!(rendered.contains("gateway_anomalies_total"));
        assert!(rendered.contains("anomaly_type=\"PossibleHijack\""));
    }
}

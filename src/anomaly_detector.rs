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

/// What the detector has learned about one prefix.
#[derive(Debug)]
struct PrefixState {
    /// Origin ASNs observed announcing this prefix.
    known_asns: HashSet<u32>,
    /// When this prefix was first observed. A prefix seen for the first time
    /// carries no information about what is normal for it — see
    /// `HijackDetector::check`.
    first_seen: DateTime<Utc>,
}

/// Detects possible BGP hijacks by tracking which ASNs announce each prefix,
/// and by telling a change in RPKI status apart from a persistent state.
#[derive(Debug)]
pub struct HijackDetector {
    /// Maps prefix → what has been learned about it.
    prefix_state: DashMap<String, PrefixState>,
    /// Maps (prefix, origin_as) → the RPKI status last observed for that route.
    /// Lets the detector tell a *change* ("was valid until 14:03") from a
    /// *state* ("has been invalid for months") — only the change is an event.
    rpki_state: DashMap<(String, u32), RpkiStatus>,
    /// How long a prefix must have been observed before a new origin for it
    /// counts as an event. Inside this window the detector is still learning
    /// which origins are normal — many prefixes are legitimately announced by
    /// several ASes (multi-homing, anycast), and those would all look like
    /// hijacks on the second sighting.
    learning_period: chrono::Duration,
}

impl Default for HijackDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl HijackDetector {
    /// Creates a new hijack detector with empty state.
    pub fn new() -> Self {
        Self {
            prefix_state: DashMap::new(),
            rpki_state: DashMap::new(),
            learning_period: chrono::Duration::hours(1),
        }
    }

    /// Overrides how long a prefix is observed before a new origin for it is
    /// treated as an event (default: 1 hour).
    pub fn with_learning_period(mut self, period: chrono::Duration) -> Self {
        self.learning_period = period;
        self
    }

    /// Number of prefixes the detector currently knows about.
    pub fn known_prefix_count(&self) -> usize {
        self.prefix_state.len()
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
                    // Warmed-up prefixes are known history, not fresh
                    // observations: backdate them past the learning period so
                    // a new origin for them is an event immediately.
                    self.prefix_state.insert(
                        row.prefix,
                        PrefixState {
                            known_asns: set,
                            first_seen: Utc::now() - self.learning_period,
                        },
                    );
                }
                tracing::info!(
                    "HijackDetector warmup complete: loaded {} prefixes",
                    self.prefix_state.len()
                );
            }
            Err(e) => {
                tracing::warn!("HijackDetector warmup failed: {}", e);
                // Do NOT panic, do NOT fail startup.
            }
        }
    }

    /// Test helper: seeds a prefix as already learned — known origins, first
    /// seen long enough ago that the learning period has elapsed. Mirrors what
    /// `warmup()` produces from historical data.
    #[cfg(test)]
    pub(crate) fn seed_known_prefix(&self, prefix: &str, asns: &[u32]) {
        self.prefix_state.insert(
            prefix.to_string(),
            PrefixState {
                known_asns: asns.iter().copied().collect(),
                first_seen: Utc::now() - self.learning_period - chrono::Duration::seconds(1),
            },
        );
    }

    /// Test helper: seeds the last-seen RPKI status for one route.
    #[cfg(test)]
    pub(crate) fn seed_rpki_status(&self, prefix: &str, origin_as: u32, status: RpkiStatus) {
        self.rpki_state
            .insert((prefix.to_string(), origin_as), status);
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

        // Record this route's RPKI status and remember what it was before.
        // Measured on the live stream: RPKI-invalid announcements are dominated
        // by a small set of routes that are invalid *continuously* — 2404
        // invalid announcements per minute came from only 145 distinct routes.
        // Those are stale ROAs and misconfigurations, not incidents; alerting
        // on each announcement reports a standing condition over and over. What
        // carries information is the transition into that state.
        let is_invalid = matches!(
            rpki_status,
            RpkiStatus::InvalidAsn | RpkiStatus::InvalidLength
        );
        let previous_status = self
            .rpki_state
            .insert((event.prefix.clone(), event.origin_as), rpki_status.clone());
        let was_invalid = matches!(
            previous_status,
            Some(RpkiStatus::InvalidAsn) | Some(RpkiStatus::InvalidLength)
        );
        let newly_invalid = is_invalid && previous_status.is_some() && !was_invalid;

        match (anomaly.take(), rpki_status) {
            // Case 1: No anomaly from check() (known AS) but RPKI invalid.
            // Only a *change* into invalid is reported; a route that was
            // already invalid last time we saw it is a known condition.
            (None, RpkiStatus::InvalidAsn | RpkiStatus::InvalidLength) if !newly_invalid => None,
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
                        "RPKI status changed to INVALID: AS{} announced {} — was {:?} when last seen, now a ROA violation (known AS){}",
                        event.origin_as,
                        event.prefix,
                        previous_status.unwrap_or(RpkiStatus::Unavailable),
                        irr_note
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

        // First observation of this prefix: record it and stay silent. The
        // detector knows nothing about what is normal for a prefix it has
        // never seen, so calling it a hijack says nothing about the route —
        // only that the process started recently. Without this, a cold start
        // flags essentially the whole visible routing table (~1M prefixes).
        let Some(mut state) = self.prefix_state.get_mut(prefix) else {
            self.prefix_state.insert(
                prefix.clone(),
                PrefixState {
                    known_asns: HashSet::from([origin_as]),
                    first_seen: Utc::now(),
                },
            );
            return None;
        };

        // If this ASN is already known, it's legitimate.
        if state.known_asns.contains(&origin_as) {
            return None;
        }

        // New origin for a known prefix. Inside the learning period the set of
        // legitimate origins is still being collected — a multi-homed prefix
        // announced by a second, entirely legitimate AS would otherwise fire
        // here. Record it and stay silent.
        let observed_for = Utc::now() - state.first_seen;
        state.known_asns.insert(origin_as);
        if observed_for < self.learning_period {
            return None;
        }

        // New origin for a prefix whose normal origins are known → event.
        let known_asns: Vec<u32> = state
            .known_asns
            .iter()
            .copied()
            .filter(|asn| *asn != origin_as)
            .collect();
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
    /// Maps prefix → recent event timestamps and types within the sliding window.
    prefix_to_events: DashMap<String, VecDeque<(DateTime<Utc>, String)>>,
}

impl FlappingDetector {
    /// Creates a new flapping detector.
    pub fn new() -> Self {
        Self {
            prefix_to_events: DashMap::new(),
        }
    }
}

impl Detector for FlappingDetector {
    fn name(&self) -> &'static str {
        "FlappingDetector"
    }

    fn check(&self, event: &BgpClickHouseRecord) -> Option<Anomaly> {
        const FLAP_WINDOW_SECS: u64 = 300; // 5-minute window
        const FLAP_THRESHOLD: usize = 50; // 50 events in 5min (was 10)
        const MIN_DIRECTION_CHANGES: usize = 6; // Minimum direction changes for oscillation

        let prefix = &event.prefix;
        let now = Utc::now();

        // Get or create the event queue for this prefix.
        let entry = self.prefix_to_events.entry(prefix.clone());
        let mut events = entry.or_insert_with(VecDeque::new);

        // Add current event with its type.
        events.push_back((event.timestamp, event.event_type.clone()));

        // Remove events outside the sliding window.
        let cutoff = now - chrono::Duration::seconds(FLAP_WINDOW_SECS as i64);
        while events.front().is_some_and(|&(ts, _)| ts < cutoff) {
            events.pop_front();
        }

        // Check BOTH conditions:
        // 1. Frequency condition: enough events in the window
        if events.len() < FLAP_THRESHOLD {
            return None;
        }

        // 2. Oscillation condition: count direction changes
        let mut direction_changes = 0;
        let mut prev_event_type: Option<&str> = None;

        for (_, event_type) in events.iter() {
            if let Some(prev) = prev_event_type {
                if prev != event_type {
                    direction_changes += 1;
                }
            }
            prev_event_type = Some(event_type);
        }

        // Need at least MIN_DIRECTION_CHANGES direction changes
        if direction_changes < MIN_DIRECTION_CHANGES {
            return None;
        }

        // Both conditions satisfied → flapping detected
        let details = format!(
            "Prefix {} had {} events with {} direction changes in the last {} seconds",
            prefix,
            events.len(),
            direction_changes,
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
        let rpki_status = rpki_cache.validate(&record.prefix, record.origin_as);
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

    // Helper function specifically for flapping tests
    fn make_flap_event(prefix: &str, event_type: &str) -> BgpClickHouseRecord {
        BgpClickHouseRecord {
            prefix: prefix.to_string(),
            origin_as: 64512,
            event_type: event_type.to_string(),
            timestamp: Utc::now(),
            tenant_id: "test".to_string(),
            peer_ip: "1.2.3.4".to_string(),
            peer_asn: 64512,
            as_path: vec![64512],
            community: vec![],
            source: "test".to_string(),
        }
    }

    // --- Zustand vs. Ereignis -------------------------------------------

    /// A prefix the detector has never seen carries no information about what
    /// is normal for it. Flagging the first sighting reports the age of the
    /// process, not a property of the route — and on a cold start that is the
    /// entire visible routing table.
    #[test]
    fn test_first_sighting_of_prefix_is_learning_not_an_event() {
        let detector = HijackDetector::new();
        let event = make_event("203.0.113.0/24", 64512, "announce");

        assert!(
            detector.check(&event).is_none(),
            "first sighting must be silent"
        );
        assert_eq!(detector.known_prefix_count(), 1, "but it must be recorded");

        // The same origin again is still normal.
        assert!(detector.check(&event).is_none());
    }

    /// Many prefixes are legitimately announced by more than one AS
    /// (multi-homing, anycast). Inside the learning period the detector is
    /// still collecting that set and must not treat the second origin as an
    /// attack.
    #[test]
    fn test_new_origin_within_learning_period_is_silent() {
        let detector = HijackDetector::new().with_learning_period(chrono::Duration::hours(1));
        assert!(detector
            .check(&make_event("203.0.113.0/24", 64512, "announce"))
            .is_none());

        let second_origin = make_event("203.0.113.0/24", 64513, "announce");
        assert!(
            detector.check(&second_origin).is_none(),
            "a second origin seen moments after the first is not yet an event"
        );
    }

    /// Once a prefix has been observed long enough, a previously unseen origin
    /// for it is the classic hijack signature.
    #[test]
    fn test_new_origin_after_learning_period_is_an_event() {
        let detector = HijackDetector::new().with_learning_period(chrono::Duration::zero());
        assert!(detector
            .check(&make_event("203.0.113.0/24", 64512, "announce"))
            .is_none());

        let anomaly = detector.check(&make_event("203.0.113.0/24", 64513, "announce"));
        let anomaly = anomaly.expect("new origin for a learned prefix is an event");
        assert_eq!(anomaly.origin_as, 64513);
        assert!(
            anomaly.details.contains("64512"),
            "details must name the known originator, not the announcing AS: {}",
            anomaly.details
        );
        assert!(
            !anomaly.details.contains("[64513]"),
            "the flagged AS must not be listed as a known originator: {}",
            anomaly.details
        );
    }

    /// Measured on the live stream: 2404 RPKI-invalid announcements per minute
    /// came from just 145 distinct routes — the same stale ROAs announcing over
    /// and over. Reporting each announcement restates a standing condition; the
    /// information is in the transition into it.
    #[test]
    fn test_rpki_invalid_alerts_on_transition_not_on_state() {
        let detector = HijackDetector::new();
        detector.seed_known_prefix("203.0.113.0/24", &[64512]);
        let event = make_event("203.0.113.0/24", 64512, "announce");
        let irr = IrrStatus::Consistent;

        // First time this route's status is seen at all — nothing to compare to.
        assert!(
            detector
                .check_with_rpki_status(&event, RpkiStatus::InvalidAsn, &irr)
                .is_none(),
            "the first observation establishes state, it is not a change"
        );

        // Still invalid on the next announcement: a known condition, not news.
        assert!(
            detector
                .check_with_rpki_status(&event, RpkiStatus::InvalidAsn, &irr)
                .is_none(),
            "a chronically invalid route must not re-alert on every announcement"
        );

        // The route becomes valid again — recorded, no alert.
        assert!(detector
            .check_with_rpki_status(&event, RpkiStatus::Valid, &irr)
            .is_none());

        // And now it turns invalid. That is the event.
        let anomaly = detector
            .check_with_rpki_status(&event, RpkiStatus::InvalidAsn, &irr)
            .expect("valid -> invalid is an event");
        assert_eq!(anomaly.confidence, 0.60);
        assert!(
            anomaly.details.contains("changed to INVALID"),
            "details must say what changed: {}",
            anomaly.details
        );
    }

    // Test 1: Bekanntes AS + RPKI invalid + IRR inconsistent → confidence 0.75
    #[test]
    fn test_check_rpki_invalid_irr_inconsistent_known_as() {
        let detector = HijackDetector::new();
        let event = make_event("1.2.3.0/24", 64512, "announce");

        // Known AS, and the route was RPKI-valid when last seen — so this
        // announcement is a transition into invalid, which is the event.
        detector.seed_known_prefix("1.2.3.0/24", &[64512]);
        detector.seed_rpki_status("1.2.3.0/24", 64512, RpkiStatus::Valid);

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
        // "New AS" means: the prefix is known and normally announced by
        // someone else. On a prefix the detector has never seen, AS64512 is
        // not new — it is the first thing known about that prefix.
        detector.seed_known_prefix("1.2.3.0/24", &[64500]);

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
        detector.seed_known_prefix("1.2.3.0/24", &[64500]);

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
        detector.seed_known_prefix("1.2.3.0/24", &[64500]);

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

        // Known AS, previously valid → transition into invalid
        detector.seed_known_prefix("1.2.3.0/24", &[64512]);
        detector.seed_rpki_status("1.2.3.0/24", 64512, RpkiStatus::Valid);

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
        detector.seed_known_prefix("1.2.3.0/24", &[64500]);

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
        // The prefix must be known with a different origin — that is what
        // makes AS64512 "new" for it. On a never-seen prefix the detector is
        // still learning and correctly stays silent.
        detector
            .hijack_detector()
            .seed_known_prefix("10.0.0.0/8", &[64513]);

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
        // The prefix must be known with a different origin — that is what
        // makes AS64512 "new" for it. On a never-seen prefix the detector is
        // still learning and correctly stays silent.
        detector
            .hijack_detector()
            .seed_known_prefix("10.0.0.0/8", &[64513]);

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
        // The prefix must be known with a different origin — that is what
        // makes AS64512 "new" for it. On a never-seen prefix the detector is
        // still learning and correctly stays silent.
        detector
            .hijack_detector()
            .seed_known_prefix("10.0.0.0/8", &[64513]);

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
        // The prefix must be known with a different origin — that is what
        // makes AS64512 "new" for it. On a never-seen prefix the detector is
        // still learning and correctly stays silent.
        detector
            .hijack_detector()
            .seed_known_prefix("10.0.0.0/8", &[64513]);

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

    // ── FlappingDetector tests ────────────────────────────────────────────────

    #[test]
    fn test_flapping_below_threshold_no_alert() {
        let detector = FlappingDetector::new();
        let prefix = "10.0.0.0/24";

        // Sende 49 abwechselnde announce/withdraw Events
        for i in 0..49 {
            let event_type = if i % 2 == 0 { "announce" } else { "withdraw" };
            let event = make_flap_event(prefix, event_type);
            let result = detector.check(&event);
            // Nach 49 Events sollte noch kein Alert kommen (Threshold 50)
            assert!(result.is_none());
        }
    }

    #[test]
    fn test_flapping_threshold_met_but_no_oscillation() {
        let detector = FlappingDetector::new();
        let prefix = "10.0.0.0/24";

        // Sende 50 Events, alle "announce" (kein Richtungswechsel)
        for _ in 0..50 {
            let event = make_flap_event(prefix, "announce");
            let result = detector.check(&event);
            // direction_changes = 0 < MIN_DIRECTION_CHANGES=6 → None
            assert!(result.is_none());
        }
    }

    #[test]
    fn test_flapping_threshold_met_few_direction_changes() {
        let detector = FlappingDetector::new();
        let prefix = "10.0.0.0/24";

        // Sende 50 Events: 25× announce, dann 25× withdraw (nur 1 Richtungswechsel)
        for i in 0..50 {
            let event_type = if i < 25 { "announce" } else { "withdraw" };
            let event = make_flap_event(prefix, event_type);
            let result = detector.check(&event);
            // direction_changes = 1 < 6 → None
            assert!(result.is_none());
        }
    }

    #[test]
    fn test_flapping_both_conditions_met() {
        let detector = FlappingDetector::new();
        let prefix = "10.0.0.0/24";

        // Sende 50 abwechselnde announce/withdraw Events
        for i in 0..50 {
            let event_type = if i % 2 == 0 { "announce" } else { "withdraw" };
            let event = make_flap_event(prefix, event_type);
            let result = detector.check(&event);
            // Erst nach dem 50. Event sollte Alert kommen
            if i == 49 {
                assert!(result.is_some());
                let anomaly = result.unwrap();
                assert_eq!(anomaly.anomaly_type, AnomalyType::PrefixFlapping);
                assert_eq!(anomaly.prefix, prefix);
                assert_eq!(anomaly.confidence, 0.75);
            } else {
                assert!(result.is_none());
            }
        }
    }

    #[test]
    fn test_flapping_exactly_6_direction_changes() {
        let detector = FlappingDetector::new();
        let prefix = "10.0.0.0/24";

        // Sequenz mit 7 Events, 6 Richtungswechsel:
        // announce, withdraw, announce, withdraw, announce, withdraw, announce
        let sequence = [
            "announce", "withdraw", "announce", "withdraw", "announce", "withdraw", "announce",
        ];
        for &event_type in &sequence {
            let event = make_flap_event(prefix, event_type);
            let result = detector.check(&event);
            // Nur 7 Events → len < 50 → None
            assert!(result.is_none());
        }

        // Füge 43 weitere announce Events hinzu → len=50, direction_changes=6
        for _ in 0..43 {
            let event = make_flap_event(prefix, "announce");
            let _result = detector.check(&event);
            // Nach dem 50. Event sollte Alert kommen
        }

        // Letztes Event sollte Alert auslösen
        let event = make_flap_event(prefix, "announce");
        let result = detector.check(&event);
        assert!(result.is_some());
        let anomaly = result.unwrap();
        assert_eq!(anomaly.anomaly_type, AnomalyType::PrefixFlapping);
    }

    #[test]
    fn test_flapping_independent_prefixes() {
        let detector = FlappingDetector::new();
        let prefix1 = "10.0.0.0/24";
        let prefix2 = "192.168.0.0/16";

        // Sende 50 abwechselnde Events für beide Prefixe
        for i in 0..50 {
            let event_type = if i % 2 == 0 { "announce" } else { "withdraw" };
            let event1 = make_flap_event(prefix1, event_type);
            let event2 = make_flap_event(prefix2, event_type);

            let result1 = detector.check(&event1);
            let result2 = detector.check(&event2);

            // Erst nach dem 50. Event sollten Alarme kommen
            if i == 49 {
                assert!(result1.is_some());
                assert!(result2.is_some());
                let anomaly1 = result1.unwrap();
                let anomaly2 = result2.unwrap();
                assert_eq!(anomaly1.prefix, prefix1);
                assert_eq!(anomaly2.prefix, prefix2);
                // Kein Cross-Prefix-Bleeding
                assert_ne!(anomaly1.prefix, anomaly2.prefix);
            } else {
                assert!(result1.is_none());
                assert!(result2.is_none());
            }
        }
    }

    #[test]
    fn test_flapping_direction_changes_5_no_alert() {
        let detector = FlappingDetector::new();
        let prefix = "10.0.0.0/24";

        // Erzeuge Sequenz mit genau 5 Richtungswechseln über 50 Events
        // Pattern: 10× announce, 10× withdraw, 10× announce, 10× withdraw, 10× announce
        // Das sind 5 Blöcke mit 4 Richtungswechseln (announce→withdraw, withdraw→announce, announce→withdraw, withdraw→announce)
        // Das sind 4 Richtungswechsel, nicht 5. Also ändern wir zu:
        // Pattern: 8× announce, 8× withdraw, 8× announce, 8× withdraw, 9× announce, 9× withdraw
        // Das sind 50 Events (8+8+8+8+9+9 = 50) mit 5 Richtungswechseln
        let blocks = [
            ("announce", 8),
            ("withdraw", 8),
            ("announce", 8),
            ("withdraw", 8),
            ("announce", 9),
            ("withdraw", 9),
        ];

        let mut events_sent = 0;
        for (event_type, count) in blocks.iter() {
            for _ in 0..*count {
                let event = make_flap_event(prefix, event_type);
                let result = detector.check(&event);
                events_sent += 1;

                // Nach 50 Events sollte immer noch kein Alert kommen (5 < 6)
                if events_sent == 50 {
                    assert!(
                        result.is_none(),
                        "Should not alert with 5 direction changes"
                    );
                }
            }
        }
        assert_eq!(events_sent, 50);
    }
}

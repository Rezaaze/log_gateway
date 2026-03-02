use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::Serialize;
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing;
use uuid::Uuid;

use crate::bgp_query::ClickHouseQueryClient;
use crate::clickhouse_exporter::BgpClickHouseRecord;
use crate::metrics::GatewayMetrics;
use crate::rpki_cache::{RpkiCache, RpkiStatus};

/// Type of BGP anomaly detected.
#[derive(Debug, Clone, Serialize)]
pub enum AnomalyType {
    PossibleHijack,
    PrefixFlapping,
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

    /// Checks a BGP event for hijacks with a pre-fetched RPKI status.
    ///
    /// This is an async enrichment layer that runs after the synchronous check().
    /// The caller is responsible for fetching the RPKI status (and recording metrics)
    /// before calling this method — avoids double-querying Routinator.
    pub fn check_with_rpki_status(
        &self,
        event: &BgpClickHouseRecord,
        rpki_status: RpkiStatus,
    ) -> Option<Anomaly> {
        // Skip withdraw events for hijack detection
        if event.event_type == "withdraw" {
            return None;
        }

        // Run the synchronous check
        let mut anomaly = self.check(event);

        match (anomaly.take(), rpki_status) {
            // Case 1: No anomaly from check() (known AS) but RPKI invalid → Warning
            (None, RpkiStatus::InvalidAsn | RpkiStatus::InvalidLength) => {
                // Known AS but RPKI invalid → lower confidence (Roadmap: 0.6)
                Some(Anomaly {
                    id: Uuid::new_v4(),
                    anomaly_type: AnomalyType::PossibleHijack,
                    prefix: event.prefix.clone(),
                    origin_as: event.origin_as,
                    confidence: 0.6,
                    detected_at: Utc::now(),
                    details: format!(
                        "RPKI INVALID: AS{} announced {} — ROA violation (known AS, possible misconfiguration)",
                        event.origin_as, event.prefix
                    ),
                })
            }
            // Case 2: No anomaly and RPKI is valid/not-found/unavailable → no alert
            (None, _) => None,

            // Case 3: Anomaly detected (new AS) with RPKI validation
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
                        // RPKI invalid → higher confidence
                        anomaly.confidence = 0.97;
                        anomaly.details.push_str(" (RPKI: INVALID)");
                    }
                    RpkiStatus::NotFound | RpkiStatus::Unavailable => {
                        // No ROA or RPKI unavailable → keep original confidence (0.85)
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
    alert_tx: mpsc::Sender<Anomaly>,
}

impl std::fmt::Debug for AnomalyDetector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnomalyDetector")
            .field("detectors", &self.detectors.len())
            .field("hijack_detector", &self.hijack_detector)
            .field("alert_tx", &"mpsc::Sender<Anomaly>")
            .finish()
    }
}

impl AnomalyDetector {
    /// Creates a new anomaly detector with built‑in hijack and flapping detectors.
    pub fn new(alert_tx: mpsc::Sender<Anomaly>) -> Self {
        let hijack_detector = Arc::new(HijackDetector::new());
        let flapping_detector = Box::new(FlappingDetector::new());

        // Store the hijack detector both as Arc for warmup and as a boxed trait object.
        let hijack_boxed: Box<dyn Detector> = Box::new(Arc::clone(&hijack_detector));

        Self {
            detectors: vec![hijack_boxed, flapping_detector],
            hijack_detector,
            alert_tx,
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

    /// Checks a BGP event with all detectors and sends any anomalies.
    ///
    /// This method is synchronous and must not block the ingest path.
    pub fn check(&self, event: &BgpClickHouseRecord) {
        for detector in &self.detectors {
            if let Some(anomaly) = detector.check(event) {
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

/// Async task that receives anomalies and logs them, incrementing metrics.
pub async fn run_alert_logger(
    mut rx: mpsc::Receiver<Anomaly>,
    metrics: Arc<GatewayMetrics>,
    escalation_router: Option<Arc<crate::escalation::EscalationRouter>>,
) {
    tracing::info!("Alert logger task started");

    while let Some(anomaly) = rx.recv().await {
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

        // Route through escalation router if available
        if let Some(router) = &escalation_router {
            router.route(&anomaly).await;
        }
    }

    tracing::info!("Alert logger task finished");
}

/// Async task that enriches BGP events with RPKI validation.
///
/// This runs parallel to the existing anomaly detection pipeline.
/// It receives BGP records, validates them against RPKI, and sends
/// enriched anomalies to the alert channel.
pub async fn run_rpki_enrichment(
    mut rx: tokio::sync::mpsc::Receiver<BgpClickHouseRecord>,
    hijack_detector: Arc<HijackDetector>,
    rpki_cache: Arc<RpkiCache>,
    alert_tx: tokio::sync::mpsc::Sender<Anomaly>,
    metrics: Arc<GatewayMetrics>,
) {
    tracing::info!("RPKI enrichment task started");

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

        // Check for hijacks using the already-fetched RPKI status
        if let Some(anomaly) = hijack_detector.check_with_rpki_status(&record, rpki_status) {
            // Try to send without blocking
            if let Err(e) = alert_tx.try_send(anomaly) {
                tracing::warn!("Alert channel full, dropping RPKI-enriched anomaly: {}", e);
            }
        }
    }

    tracing::info!("RPKI enrichment task finished");
}

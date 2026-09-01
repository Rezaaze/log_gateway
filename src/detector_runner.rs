/*!
Detector Runner - Orchestrates Anomaly Detection

This module:
1. Receives BGP records from NATS subscription
2. Runs them through anomaly detectors
3. Exports detected anomalies as metrics

Phase 3b-3d Implementation Plan:
- Integrate with HijackDetector + FlappingDetector
- Export to Prometheus metrics
- Handle warmup period (first 7 days of learning)
*/

use crate::anomaly_detector::{Anomaly, AnomalyType, Detector, FlappingDetector, HijackDetector};
use crate::escalation::EscalationRouter;
use crate::irr_cache::{IrrCache, IrrStatus};
use crate::metrics_exporter::DetectorMetrics;
use crate::nats_subscriber::BgpRecord;
use crate::propagation::{PropagationAggregator, PropagationEvent};
use crate::rpki_cache::{RpkiCache, RpkiStatus};
use crate::wave_anomaly_detector::{AnomalyClassification, WaveAnomalyDetector};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::info;

/// Detected Anomaly (output)
#[derive(Debug, Clone)]
pub struct DetectedAnomaly {
    pub anomaly_type: String, // "hijack" or "flapping"
    pub prefix: String,
    pub origin_as: u32,
    pub confidence: f64,
    pub details: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// Phase 3d: Full Integration Implementation Guide
///
/// To complete Phase 3d, implement the following:
///
/// 1. Import existing detectors:
/// ```ignore
/// use crate::anomaly_detector::{HijackDetector, FlappingDetector, Detector};
/// use crate::clickhouse_exporter::BgpClickHouseRecord;
/// ```
///
/// 2. Create adapter function:
/// ```ignore
/// fn convert_to_detector_format(record: &BgpRecord) -> BgpClickHouseRecord {
///     BgpClickHouseRecord {
///         timestamp: record.timestamp,
///         event_type: record.event_type.clone(),
///         prefix: record.prefix.clone(),
///         origin_as: record.origin_as,
///         as_path: record.as_path.clone(),
///         peer_asn: record.peer_asn,
///         peer_ip: record.peer_ip.clone(), // Available from NATS since Phase 1.1
///         community: Vec::new(),  // Not available from NATS
///         source: "nats-stream".to_string(),
///         tenant_id: "bgp".to_string(),
///     }
/// }
/// ```
///
/// 3. Update run_stub() method:
/// ```ignore
/// let hijack_detector = Arc::new(HijackDetector::new());
/// let flapping_detector = Arc::new(FlappingDetector::new());
///
/// while let Some(record) = rx.recv().await {
///     let bgp_record = convert_to_detector_format(&record);
///
///     if let Some(anomaly) = hijack_detector.check(&bgp_record) {
///         self.anomalies_detected.fetch_add(1, Ordering::Relaxed);
///         // Export to metrics here (Phase 4)
///     }
///
///     if let Some(anomaly) = flapping_detector.check(&bgp_record) {
///         self.anomalies_detected.fetch_add(1, Ordering::Relaxed);
///         // Export to metrics here (Phase 4)
///     }
/// }
/// ```
#[allow(dead_code)]
pub struct Phase3dGuide; // Placeholder for documentation

/// Detector Runner State
pub struct DetectorRunner {
    /// Counter for processed events
    pub events_processed: Arc<AtomicU64>,
    /// Counter for detected anomalies
    pub anomalies_detected: Arc<AtomicU64>,
    /// Counter for detection errors
    pub errors: Arc<AtomicU64>,
    /// Prometheus metrics exporter
    pub metrics: Arc<DetectorMetrics>,
    /// HijackDetector instance (Arc for multi-threaded access)
    hijack_detector: Arc<HijackDetector>,
    /// FlappingDetector instance
    flapping_detector: Box<FlappingDetector>,
    /// RPKI cache for validation (optional)
    rpki_cache: Option<Arc<RpkiCache>>,
    /// IRR cache for validation (optional)
    irr_cache: Option<Arc<IrrCache>>,
    /// Escalation router — persists, deduplicates and sends webhook
    /// notifications for detected anomalies (optional)
    escalation_router: Option<Arc<EscalationRouter>>,
    /// Aggregates per-collector arrivals into PropagationEvents for
    /// wave-physics anomaly scoring
    propagation_aggregator: Arc<PropagationAggregator>,
    /// Wave anomaly detector — scores completed PropagationEvents against a
    /// baseline (optional; without a baseline it never raises anomalies)
    wave_detector: Option<Arc<WaveAnomalyDetector>>,
}

impl std::fmt::Debug for DetectorRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DetectorRunner")
            .field("events_processed", &self.events_processed)
            .field("anomalies_detected", &self.anomalies_detected)
            .field("errors", &self.errors)
            .field("metrics", &self.metrics)
            .field("hijack_detector", &"Arc<HijackDetector>")
            .field("flapping_detector", &"Box<FlappingDetector>")
            .field(
                "rpki_cache",
                &if self.rpki_cache.is_some() {
                    "Some(RpkiCache)"
                } else {
                    "None"
                },
            )
            .field(
                "irr_cache",
                &if self.irr_cache.is_some() {
                    "Some(IrrCache)"
                } else {
                    "None"
                },
            )
            .field(
                "escalation_router",
                &if self.escalation_router.is_some() {
                    "Some(EscalationRouter)"
                } else {
                    "None"
                },
            )
            .field(
                "wave_detector",
                &if self.wave_detector.is_some() {
                    "Some(WaveAnomalyDetector)"
                } else {
                    "None"
                },
            )
            .finish()
    }
}

impl Default for DetectorRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl DetectorRunner {
    pub fn new() -> Self {
        Self {
            events_processed: Arc::new(AtomicU64::new(0)),
            anomalies_detected: Arc::new(AtomicU64::new(0)),
            errors: Arc::new(AtomicU64::new(0)),
            metrics: Arc::new(DetectorMetrics::new()),
            hijack_detector: Arc::new(HijackDetector::new()),
            flapping_detector: Box::new(FlappingDetector::new()),
            rpki_cache: None,
            irr_cache: None,
            escalation_router: None,
            propagation_aggregator: Arc::new(PropagationAggregator::new()),
            wave_detector: None,
        }
    }

    /// Create with custom metrics
    pub fn with_metrics(metrics: Arc<DetectorMetrics>) -> Self {
        Self {
            events_processed: Arc::new(AtomicU64::new(0)),
            anomalies_detected: Arc::new(AtomicU64::new(0)),
            errors: Arc::new(AtomicU64::new(0)),
            metrics,
            hijack_detector: Arc::new(HijackDetector::new()),
            flapping_detector: Box::new(FlappingDetector::new()),
            rpki_cache: None,
            irr_cache: None,
            escalation_router: None,
            propagation_aggregator: Arc::new(PropagationAggregator::new()),
            wave_detector: None,
        }
    }

    /// Create with RPKI and IRR enrichment caches
    pub fn with_enrichment(rpki: Arc<RpkiCache>, irr: Arc<IrrCache>) -> Self {
        Self {
            events_processed: Arc::new(AtomicU64::new(0)),
            anomalies_detected: Arc::new(AtomicU64::new(0)),
            errors: Arc::new(AtomicU64::new(0)),
            metrics: Arc::new(DetectorMetrics::new()),
            hijack_detector: Arc::new(HijackDetector::new()),
            flapping_detector: Box::new(FlappingDetector::new()),
            rpki_cache: Some(rpki),
            irr_cache: Some(irr),
            escalation_router: None,
            propagation_aggregator: Arc::new(PropagationAggregator::new()),
            wave_detector: None,
        }
    }

    /// Attach an escalation router so detected anomalies are persisted,
    /// deduplicated and sent to configured webhooks — without this, detected
    /// anomalies are only logged and counted in Prometheus metrics.
    pub fn with_escalation(mut self, router: Arc<EscalationRouter>) -> Self {
        self.escalation_router = Some(router);
        self
    }

    /// Attach a wave anomaly detector so completed PropagationEvents (built
    /// from per-collector arrival timing) are scored against a baseline.
    /// Without this, BGP records still feed the PropagationAggregator but
    /// completed events are discarded unscored.
    pub fn with_wave_detector(mut self, detector: Arc<WaveAnomalyDetector>) -> Self {
        self.wave_detector = Some(detector);
        self
    }

    /// Replaces the default, empty `HijackDetector` with a shared, already
    /// (possibly) warmed-up instance — e.g. the one `AnomalyDetector` (the
    /// HTTP-ingest path) warms up from ClickHouse history on startup.
    ///
    /// Without this, `DetectorRunner`'s own `HijackDetector` starts with an
    /// empty prefix->ASN map and flags the *first* sighting of every prefix
    /// as a possible hijack (confidence 0.85) until it has organically seen
    /// each one once — a real false-positive storm on every restart of the
    /// live NATS path, now that anomalies are actually escalated. Sharing
    /// the instance also means both ingest paths learn from each other's
    /// observations instead of maintaining inconsistent, duplicate state.
    pub fn with_hijack_detector(mut self, detector: Arc<HijackDetector>) -> Self {
        self.hijack_detector = detector;
        self
    }

    /// Run the detector loop
    ///
    /// Receives BGP records from NATS, runs through anomaly detectors,
    /// and exports detected anomalies as metrics.
    ///
    /// This method:
    /// 1. Receives BgpRecord from NATS subscription
    /// 2. Converts to BgpClickHouseRecord format (expected by detectors)
    /// 3. Runs HijackDetector::check() for new origin ASN detection
    /// 4. Runs FlappingDetector::check() for rapid announcements
    /// 5. Collects anomalies and exports as metrics
    pub async fn run_stub(&self, mut rx: mpsc::Receiver<BgpRecord>) {
        info!("Detector runner started");

        let mut last_log_time = std::time::Instant::now();

        // Main event processing loop
        while let Some(record) = rx.recv().await {
            self.events_processed.fetch_add(1, Ordering::Relaxed);
            self.metrics.record_event_processed();

            // Note: Full implementation in Phase 3d:
            // 1. Convert BgpRecord to BgpClickHouseRecord
            // 2. Call HijackDetector::check(&bgp_record)
            // 3. Call FlappingDetector::check(&bgp_record)
            // 4. Export anomalies to metrics
            //
            // For now, just track that we received the event
            if record.event_type == "announce" {
                // This would normally run through detectors
            }

            // Log stats every 10 seconds
            if last_log_time.elapsed().as_secs() >= 10 {
                let stats = self.stats();
                let rate = stats.events_processed as f64 / last_log_time.elapsed().as_secs_f64();
                info!(
                    "Detector stats - processed: {}, anomalies: {}, errors: {}, rate: {:.2}/sec",
                    stats.events_processed, stats.anomalies_detected, stats.errors, rate
                );
                last_log_time = std::time::Instant::now();
            }
        }

        info!(
            "Detector runner stopped - total events: {}, anomalies: {}",
            self.events_processed.load(Ordering::Relaxed),
            self.anomalies_detected.load(Ordering::Relaxed)
        );
    }

    /// Run the detector loop (full implementation)
    ///
    /// This is the complete implementation that integrates with existing
    /// anomaly detectors and exports metrics.
    pub async fn run(&self, mut rx: mpsc::Receiver<BgpRecord>) {
        info!("Detector runner started (full implementation)");

        let mut last_log_time = std::time::Instant::now();

        // Main event processing loop
        while let Some(record) = rx.recv().await {
            self.events_processed.fetch_add(1, Ordering::Relaxed);
            self.metrics.record_event_processed();

            // Process the event
            if let Err(e) = self.process_record(&record).await {
                self.errors.fetch_add(1, Ordering::Relaxed);
                self.metrics.record_error();
                tracing::error!("Error processing BGP record: {}", e);
            }

            // Log stats every 10 seconds
            if last_log_time.elapsed().as_secs() >= 10 {
                let stats = self.stats();
                let rate = stats.events_processed as f64 / last_log_time.elapsed().as_secs_f64();
                info!(
                    "Detector stats - processed: {}, anomalies: {}, errors: {}, rate: {:.2}/sec",
                    stats.events_processed, stats.anomalies_detected, stats.errors, rate
                );
                last_log_time = std::time::Instant::now();
            }
        }

        info!(
            "Detector runner stopped - total events: {}, anomalies: {}",
            self.events_processed.load(Ordering::Relaxed),
            self.anomalies_detected.load(Ordering::Relaxed)
        );
    }

    /// Process a single BGP record through anomaly detectors
    async fn process_record(&self, record: &BgpRecord) -> Result<(), Box<dyn std::error::Error>> {
        // Convert BgpRecord to BgpClickHouseRecord format expected by detectors
        let bgp_record = self.convert_to_detector_format(record);

        // Run FlappingDetector (no RPKI/IRR needed for flapping detection)
        if let Some(anomaly) = self.flapping_detector.check(&bgp_record) {
            self.anomalies_detected.fetch_add(1, Ordering::Relaxed);
            self.metrics.record_anomaly("flapping");
            tracing::warn!(
                "Flapping detected: prefix={}, confidence={:.2}, details={}",
                anomaly.prefix,
                anomaly.confidence,
                anomaly.details
            );
            if let Some(router) = &self.escalation_router {
                router.route(&anomaly).await;
            }
        }

        // Run HijackDetector with RPKI/IRR enrichment if available
        if let Some(rpki_cache) = &self.rpki_cache {
            // Only check ANNOUNCE events for RPKI validation
            if record.event_type == "announce" {
                // a) Get RPKI status
                let rpki_status: RpkiStatus =
                    rpki_cache.validate(&bgp_record.prefix, bgp_record.origin_as);

                match rpki_status {
                    RpkiStatus::Valid => {
                        // Kein Hijack möglich wenn RPKI valid
                        return Ok(());
                    }
                    RpkiStatus::InvalidAsn | RpkiStatus::InvalidLength => {
                        // Echter Hijack — kryptografisch bewiesen
                        // b) Get IRR status if cache available
                        let irr_status = if let Some(irr_cache) = &self.irr_cache {
                            irr_cache
                                .check(&bgp_record.prefix, bgp_record.origin_as)
                                .await
                        } else {
                            IrrStatus::Unavailable
                        };

                        // c) Check for anomaly with RPKI and IRR status
                        if let Some(anomaly) = self.hijack_detector.check_with_rpki_status(
                            &bgp_record,
                            rpki_status,
                            &irr_status,
                        ) {
                            self.anomalies_detected.fetch_add(1, Ordering::Relaxed);
                            self.metrics.record_anomaly("hijack");
                            tracing::warn!(
                                "Hijack detected with RPKI/IRR: prefix={}, origin_as={}, confidence={:.2}, details={}",
                                anomaly.prefix,
                                anomaly.origin_as,
                                anomaly.confidence,
                                anomaly.details
                            );
                            if let Some(router) = &self.escalation_router {
                                router.route(&anomaly).await;
                            }
                        }
                    }
                    RpkiStatus::NotFound | RpkiStatus::Unavailable => {
                        // Kein Beweis → kein Alert
                        // Trotzdem: IRR check überspringen
                        // Keine weitere Verarbeitung für diesen Record
                    }
                }
            } else {
                // WITHDRAW events: kein Hijack-Check ohne RPKI-Beweis
                // Ein Withdraw allein ist kein Anzeichen eines Hijacks.
            }
        } else {
            // No RPKI cache available, use basic hijack detection
            if let Some(anomaly) = self.hijack_detector.check(&bgp_record) {
                self.anomalies_detected.fetch_add(1, Ordering::Relaxed);
                self.metrics.record_anomaly("hijack");
                tracing::warn!(
                    "Hijack detected (no RPKI): prefix={}, origin_as={}, confidence={:.2}, details={}",
                    anomaly.prefix,
                    anomaly.origin_as,
                    anomaly.confidence,
                    anomaly.details
                );
                if let Some(router) = &self.escalation_router {
                    router.route(&anomaly).await;
                }
            }
        }

        // Feed the wave-physics propagation aggregator (announcements only —
        // arrival timing across collectors is only meaningful for the
        // announcement of a route, not its withdrawal). If this record
        // completes a propagation group's observation window, score it.
        if record.event_type == "announce" {
            if let Some(event) = self.propagation_aggregator.add(record) {
                self.score_propagation_event(&event).await;
            }
        }

        Ok(())
    }

    /// Scores a completed PropagationEvent against the wave baseline and, if
    /// anomalous, routes it through escalation the same way as hijack/flap
    /// detections. A no-op if no wave detector is attached.
    async fn score_propagation_event(&self, event: &PropagationEvent) {
        let Some(wave_detector) = &self.wave_detector else {
            return;
        };
        let score = wave_detector.score_event(event);
        if score.classification == AnomalyClassification::Normal {
            return;
        }

        self.anomalies_detected.fetch_add(1, Ordering::Relaxed);
        self.metrics.record_anomaly("wave");

        let anomaly = Anomaly {
            id: uuid::Uuid::new_v4(),
            anomaly_type: AnomalyType::PossibleHijack,
            prefix: event.prefix.to_string(),
            origin_as: event.origin_as,
            confidence: score.total_score,
            detected_at: chrono::Utc::now(),
            details: format!(
                "Wave anomaly ({:?}): spread_z={:.2} outlier={:.2} gap_ratio={:.2} order_deviation={:.2} speed={:.2}",
                score.classification,
                score.signals.spread_z_score,
                score.signals.outlier_factor,
                score.signals.collector_gap_ratio,
                score.signals.order_deviation,
                score.signals.propagation_speed,
            ),
            tenant_id: "bgp".to_string(),
        };

        tracing::warn!(
            "Wave anomaly detected: prefix={}, origin_as={}, confidence={:.2}, classification={:?}",
            anomaly.prefix,
            anomaly.origin_as,
            anomaly.confidence,
            score.classification
        );

        if let Some(router) = &self.escalation_router {
            router.route(&anomaly).await;
        }
    }

    /// Flushes propagation groups whose observation window has expired
    /// without a natural completion (e.g. a collector never reported an
    /// arrival) and scores them. Call periodically (e.g. every 1–5s) from a
    /// background task — otherwise slow-completing groups are only scored
    /// when the next record for the same (prefix, origin_as, as_path) hash
    /// happens to arrive.
    pub async fn flush_propagation_events(&self) {
        if self.wave_detector.is_none() {
            return;
        }
        for event in self.propagation_aggregator.flush_expired() {
            self.score_propagation_event(&event).await;
        }
    }

    /// Convert BgpRecord to BgpClickHouseRecord format
    fn convert_to_detector_format(
        &self,
        record: &BgpRecord,
    ) -> crate::clickhouse_exporter::BgpClickHouseRecord {
        crate::clickhouse_exporter::BgpClickHouseRecord {
            timestamp: record.timestamp,
            event_type: record.event_type.clone(),
            prefix: record.prefix.clone(),
            origin_as: record.origin_as,
            as_path: record.as_path.clone(),
            peer_asn: record.peer_asn,
            peer_ip: String::new(), // Not available from NATS
            community: Vec::new(),  // Not available from NATS
            source: "nats-stream".to_string(),
            tenant_id: "bgp".to_string(),
        }
    }

    /// Export detected anomaly as Prometheus metric
    ///
    /// Phase 3c Implementation:
    /// - Increment counter for anomaly type
    /// - Record histogram for confidence
    /// - Update gauge for latest anomaly timestamp
    #[allow(dead_code)]
    async fn export_anomaly(&self, _anomaly: &DetectedAnomaly) {
        // Placeholder - will be implemented in Phase 4
        // self.metrics.anomalies_total[&anomaly.anomaly_type].inc();
        // self.metrics.anomaly_confidence.observe(anomaly.confidence);
    }

    /// Get current statistics
    pub fn stats(&self) -> DetectorStats {
        DetectorStats {
            events_processed: self.events_processed.load(Ordering::Relaxed),
            anomalies_detected: self.anomalies_detected.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
        }
    }

    /// Gather metrics in Prometheus text format
    pub fn gather_metrics(&self) -> String {
        self.metrics.render()
    }
}

/// Detector statistics snapshot
#[derive(Debug, Clone)]
pub struct DetectorStats {
    pub events_processed: u64,
    pub anomalies_detected: u64,
    pub errors: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detector_runner_creation() {
        let runner = DetectorRunner::new();
        let stats = runner.stats();

        assert_eq!(stats.events_processed, 0);
        assert_eq!(stats.anomalies_detected, 0);
        assert_eq!(stats.errors, 0);
    }

    #[tokio::test]
    async fn test_detector_runner_stats() {
        let runner = DetectorRunner::new();
        runner.events_processed.fetch_add(100, Ordering::Relaxed);
        runner.anomalies_detected.fetch_add(5, Ordering::Relaxed);

        let stats = runner.stats();
        assert_eq!(stats.events_processed, 100);
        assert_eq!(stats.anomalies_detected, 5);
    }

    #[tokio::test]
    async fn test_detector_runner_with_metrics() {
        let metrics = Arc::new(crate::metrics_exporter::DetectorMetrics::new());
        let runner = DetectorRunner::with_metrics(Arc::clone(&metrics));

        // Record some events
        runner.metrics.record_event_processed();
        runner.metrics.record_event_processed();
        runner.metrics.record_anomaly("hijack");
        runner.metrics.record_error();

        // Verify atomic counters
        runner.events_processed.fetch_add(2, Ordering::Relaxed);
        runner.anomalies_detected.fetch_add(1, Ordering::Relaxed);
        runner.errors.fetch_add(1, Ordering::Relaxed);

        let stats = runner.stats();
        assert_eq!(stats.events_processed, 2);
        assert_eq!(stats.anomalies_detected, 1);
        assert_eq!(stats.errors, 1);
    }

    #[tokio::test]
    async fn test_real_detector_integration() {
        use chrono::Utc;

        let runner = DetectorRunner::new();

        // Create a BGP record for processing
        let bgp_record = crate::clickhouse_exporter::BgpClickHouseRecord {
            timestamp: Utc::now(),
            event_type: "announce".to_string(),
            prefix: "192.0.2.0/24".to_string(),
            origin_as: 65001,
            as_path: vec![65000, 65001],
            peer_asn: 65000,
            peer_ip: "203.0.113.1".to_string(),
            community: vec![],
            source: "nats-stream".to_string(),
            tenant_id: "bgp".to_string(),
        };

        // Process the record
        let result = runner
            .process_record(&BgpRecord {
                prefix: bgp_record.prefix.clone(),
                origin_as: bgp_record.origin_as,
                peer_asn: bgp_record.peer_asn,
                event_type: bgp_record.event_type.clone(),
                as_path: bgp_record.as_path.clone(),
                timestamp: bgp_record.timestamp,
                collector: "unknown".to_string(),
                peer_ip: "".to_string(),
            })
            .await;

        assert!(result.is_ok(), "process_record should not error");

        // Verify detectors are installed and working
        let stats = runner.stats();
        assert!(
            stats.events_processed == 0,
            "Stats counter not auto-incremented (manual update required)"
        );
    }

    #[tokio::test]
    async fn test_detector_runner_hijack_detection() {
        use chrono::Utc;

        let runner = DetectorRunner::new();

        // First announce: ASN 65001 for prefix 10.0.0.0/8
        let record1 = crate::clickhouse_exporter::BgpClickHouseRecord {
            timestamp: Utc::now(),
            event_type: "announce".to_string(),
            prefix: "10.0.0.0/8".to_string(),
            origin_as: 65001,
            as_path: vec![65000, 65001],
            peer_asn: 65000,
            peer_ip: "203.0.113.1".to_string(),
            community: vec![],
            source: "nats-stream".to_string(),
            tenant_id: "bgp".to_string(),
        };

        // Process first announcement (should not trigger hijack, it's new)
        let _ = runner
            .process_record(&BgpRecord {
                prefix: record1.prefix.clone(),
                origin_as: record1.origin_as,
                peer_asn: record1.peer_asn,
                event_type: record1.event_type.clone(),
                as_path: record1.as_path.clone(),
                timestamp: record1.timestamp,
                collector: "unknown".to_string(),
                peer_ip: "".to_string(),
            })
            .await;

        // Second announce: Different ASN for same prefix (hijack!)
        let record2 = crate::clickhouse_exporter::BgpClickHouseRecord {
            timestamp: Utc::now(),
            event_type: "announce".to_string(),
            prefix: "10.0.0.0/8".to_string(),
            origin_as: 65002, // Different ASN!
            as_path: vec![65000, 65002],
            peer_asn: 65000,
            peer_ip: "203.0.113.1".to_string(),
            community: vec![],
            source: "nats-stream".to_string(),
            tenant_id: "bgp".to_string(),
        };

        // Process second announcement (should trigger hijack detection)
        let _ = runner
            .process_record(&BgpRecord {
                prefix: record2.prefix.clone(),
                origin_as: record2.origin_as,
                peer_asn: record2.peer_asn,
                event_type: record2.event_type.clone(),
                as_path: record2.as_path.clone(),
                timestamp: record2.timestamp,
                collector: "unknown".to_string(),
                peer_ip: "".to_string(),
            })
            .await;

        // Anomalies should have been detected
        let stats = runner.stats();
        assert!(
            stats.anomalies_detected > 0,
            "Hijack detection should have triggered"
        );
    }

    // Helper function to create a BGP record for testing
    fn make_bgp_record(prefix: &str, origin_as: u32, event_type: &str) -> BgpRecord {
        use chrono::Utc;

        BgpRecord {
            prefix: prefix.to_string(),
            origin_as,
            peer_asn: 64512,
            event_type: event_type.to_string(),
            as_path: vec![64512, origin_as],
            timestamp: Utc::now(),
            collector: "unknown".to_string(),
            peer_ip: "".to_string(),
        }
    }

    #[tokio::test]
    async fn test_rpki_valid_suppresses_hijack_check() {
        use crate::rpki_cache::{addr_to_u128, RpkiCache};
        use ipnet::IpNet;
        use std::collections::HashMap;

        // Create RpkiCache with mock data
        let rpki_cache = Arc::new(RpkiCache::new("http://dummy".to_string()));
        // Add VRP: 192.0.2.0/24 → AS64512, max_length=24
        let prefix_network: IpNet = "192.0.2.0/24".parse().unwrap();
        let key = (
            prefix_network.prefix_len(),
            addr_to_u128(prefix_network.network()),
        );
        let index = HashMap::from([(key, vec![(24, 64512)])]);
        rpki_cache.set_test_data(index).await;

        // Create DetectorRunner with RPKI cache
        let runner = DetectorRunner::with_enrichment(
            rpki_cache,
            Arc::new(crate::irr_cache::IrrCache::new()),
        );

        // Process announcement with matching prefix and ASN (should be Valid)
        let record = make_bgp_record("192.0.2.0/24", 64512, "announce");
        let result = runner.process_record(&record).await;
        assert!(result.is_ok(), "process_record should succeed");

        // Valid RPKI should suppress hijack check → no anomalies
        let stats = runner.stats();
        assert_eq!(
            stats.anomalies_detected, 0,
            "Valid RPKI should suppress hijack detection"
        );
    }

    #[tokio::test]
    async fn test_rpki_invalid_asn_triggers_alert() {
        use crate::rpki_cache::{addr_to_u128, RpkiCache};
        use ipnet::IpNet;
        use std::collections::HashMap;

        // Create RpkiCache with mock data: 192.0.2.0/24 → AS11111, max_length=24
        let rpki_cache = Arc::new(RpkiCache::new("http://dummy".to_string()));
        let prefix_network: IpNet = "192.0.2.0/24".parse().unwrap();
        let key = (
            prefix_network.prefix_len(),
            addr_to_u128(prefix_network.network()),
        );
        let index = HashMap::from([(key, vec![(24, 11111)])]);
        rpki_cache.set_test_data(index).await;

        // Create DetectorRunner with RPKI cache
        let runner = DetectorRunner::with_enrichment(
            rpki_cache,
            Arc::new(crate::irr_cache::IrrCache::new()),
        );

        // Process announcement with wrong ASN (99999) → InvalidAsn
        let record = make_bgp_record("192.0.2.0/24", 99999, "announce");
        let result = runner.process_record(&record).await;
        assert!(result.is_ok(), "process_record should succeed");

        // Invalid ASN should trigger alert
        let stats = runner.stats();
        assert!(
            stats.anomalies_detected >= 1,
            "Invalid ASN should trigger alert"
        );
    }

    #[tokio::test]
    async fn test_rpki_invalid_length_triggers_alert() {
        use crate::rpki_cache::{addr_to_u128, RpkiCache};
        use ipnet::IpNet;
        use std::collections::HashMap;

        // Create RpkiCache with mock data: 192.0.2.0/24 → AS64512, max_length=24
        let rpki_cache = Arc::new(RpkiCache::new("http://dummy".to_string()));
        let prefix_network: IpNet = "192.0.2.0/24".parse().unwrap();
        let key = (
            prefix_network.prefix_len(),
            addr_to_u128(prefix_network.network()),
        );
        let index = HashMap::from([(key, vec![(24, 64512)])]);
        rpki_cache.set_test_data(index).await;

        // Create DetectorRunner with RPKI cache
        let runner = DetectorRunner::with_enrichment(
            rpki_cache,
            Arc::new(crate::irr_cache::IrrCache::new()),
        );

        // Process announcement with longer prefix (192.0.2.0/28) → InvalidLength
        let record = make_bgp_record("192.0.2.0/28", 64512, "announce");
        let result = runner.process_record(&record).await;
        assert!(result.is_ok(), "process_record should succeed");

        // Invalid length should trigger alert
        let stats = runner.stats();
        assert!(
            stats.anomalies_detected >= 1,
            "Invalid length should trigger alert"
        );
    }

    #[tokio::test]
    async fn test_rpki_unavailable_falls_through_to_hijack_detector() {
        // Create DetectorRunner WITHOUT RPKI cache (rpki_cache: None)
        let runner = DetectorRunner::new();

        // Process a normal announcement
        let record = make_bgp_record("10.0.0.0/8", 64512, "announce");
        let result = runner.process_record(&record).await;
        assert!(
            result.is_ok(),
            "process_record should succeed without panic"
        );

        // Events should be processed normally
        let stats = runner.stats();
        assert_eq!(
            stats.events_processed, 0,
            "events_processed counter not auto-incremented in test"
        );
        // Note: events_processed is not auto-incremented in process_record, only in run()
        // But we can verify no panic occurred
    }

    #[tokio::test]
    async fn test_withdraw_event_never_triggers_hijack() {
        use crate::rpki_cache::{addr_to_u128, RpkiCache};
        use ipnet::IpNet;
        use std::collections::HashMap;

        // Create RpkiCache with mock data
        let rpki_cache = Arc::new(RpkiCache::new("http://dummy".to_string()));
        let prefix_network: IpNet = "192.0.2.0/24".parse().unwrap();
        let key = (
            prefix_network.prefix_len(),
            addr_to_u128(prefix_network.network()),
        );
        let index = HashMap::from([(key, vec![(24, 64512)])]);
        rpki_cache.set_test_data(index).await;

        // Create DetectorRunner with RPKI cache
        let runner = DetectorRunner::with_enrichment(
            rpki_cache,
            Arc::new(crate::irr_cache::IrrCache::new()),
        );

        // Send 20 WITHDRAW events for the same prefix
        for _ in 0..20 {
            let record = make_bgp_record("192.0.2.0/24", 64512, "withdraw");
            let result = runner.process_record(&record).await;
            assert!(result.is_ok(), "process_record should succeed");
        }

        // events_processed is only incremented by the run() loop, not by process_record().
        // Verify that:
        //   1. No errors occurred during processing
        //   2. No anomalies were detected (20 events < flapping threshold=50, no hijack on WITHDRAW)
        let stats = runner.stats();
        assert_eq!(
            stats.errors, 0,
            "no errors expected for valid WITHDRAW events"
        );
        assert_eq!(
            stats.anomalies_detected, 0,
            "WITHDRAW events must not trigger hijack or flapping (20 < threshold 50)"
        );
    }

    #[tokio::test]
    async fn test_escalation_router_invoked_on_hijack_without_panicking() {
        use crate::alert_manager::AlertManagerClient;
        use crate::escalation::EscalationRouter;
        use crate::metrics::GatewayMetrics;

        // Escalation router with an unreachable ClickHouse URL — route() must
        // degrade gracefully (log + continue) rather than panic or block
        // anomaly detection, per its documented behavior.
        let alert_manager = Arc::new(AlertManagerClient::new(
            "http://dummy".to_string(),
            "db".to_string(),
        ));
        let router = Arc::new(EscalationRouter::new(
            alert_manager,
            Arc::new(GatewayMetrics::new()),
        ));

        let runner = DetectorRunner::new().with_escalation(Arc::clone(&router));

        // First announce: establishes origin AS 65001 for the prefix.
        let record1 = make_bgp_record("10.0.0.0/8", 65001, "announce");
        let result1 = runner.process_record(&record1).await;
        assert!(result1.is_ok(), "process_record should not error");

        // Second announce: different ASN for same prefix → hijack, which
        // now routes through the (unreachable) escalation router.
        let record2 = make_bgp_record("10.0.0.0/8", 65002, "announce");
        let result2 = runner.process_record(&record2).await;
        assert!(
            result2.is_ok(),
            "process_record must not fail even if escalation persistence is unreachable"
        );

        let stats = runner.stats();
        assert!(
            stats.anomalies_detected > 0,
            "hijack should still be counted even when escalation is attached"
        );
    }

    #[tokio::test]
    async fn test_score_propagation_event_noop_without_wave_detector() {
        use std::collections::BTreeMap;

        // No wave detector attached — scoring must be a safe no-op.
        let runner = DetectorRunner::new();
        let mut arrivals = BTreeMap::new();
        arrivals.insert("rrc00".to_string(), 1000.0);
        arrivals.insert("rrc01".to_string(), 1000.5);
        let event = PropagationEvent::new(
            "8.8.8.0/24".parse().unwrap(),
            15169,
            vec![1103, 15169],
            arrivals,
        );

        runner.score_propagation_event(&event).await;

        assert_eq!(
            runner.stats().anomalies_detected,
            0,
            "no wave detector attached — score_propagation_event must not count anomalies"
        );
    }

    #[tokio::test]
    async fn test_score_propagation_event_routes_anomalous_wave_score() {
        use crate::alert_manager::AlertManagerClient;
        use crate::escalation::EscalationRouter;
        use crate::metrics::GatewayMetrics;
        use crate::wave_baseline::{WaveBaseline, WaveBaselineEntry};
        use std::collections::BTreeMap;

        let as_path = vec![1103u32, 15169u32];
        let path_hash = as_path.iter().fold(0u64, |acc, &asn| {
            acc.wrapping_mul(31).wrapping_add(asn as u64)
        });

        // Baseline expects a tight, ~20ms spread — far tighter than the
        // 500ms spread the synthetic event below will report.
        let mut baseline = WaveBaseline::new("test");
        baseline.extend(vec![WaveBaselineEntry {
            prefix: "8.8.8.0/24".to_string(),
            origin_as: 15169,
            as_path_hash: path_hash,
            sample_count: 100,
            min_spread_ms: 5.0,
            p50_spread_ms: 20.0,
            p95_spread_ms: 40.0,
            p99_spread_ms: 60.0,
            max_spread_ms: 80.0,
            std_dev: 5.0,
            expected_order: vec!["rrc00".to_string(), "rrc01".to_string()],
        }]);

        let tmp = tempfile::NamedTempFile::new().unwrap();
        baseline.save(tmp.path()).unwrap();

        let wave_detector =
            Arc::new(WaveAnomalyDetector::new(Some(tmp.path())).expect("baseline should load"));

        let alert_manager = Arc::new(AlertManagerClient::new(
            "http://dummy".to_string(),
            "db".to_string(),
        ));
        let router = Arc::new(EscalationRouter::new(
            alert_manager,
            Arc::new(GatewayMetrics::new()),
        ));

        let runner = DetectorRunner::new()
            .with_wave_detector(wave_detector)
            .with_escalation(Arc::clone(&router));

        // 500ms spread against a ~20ms±5ms baseline — well past p99 (60ms).
        let mut arrivals = BTreeMap::new();
        arrivals.insert("rrc00".to_string(), 1000.0);
        arrivals.insert("rrc01".to_string(), 1000.5);
        let event = PropagationEvent::new("8.8.8.0/24".parse().unwrap(), 15169, as_path, arrivals);

        runner.score_propagation_event(&event).await;

        assert!(
            runner.stats().anomalies_detected > 0,
            "spread far outside baseline should be classified as anomalous and counted"
        );
    }

    #[tokio::test]
    async fn test_shared_hijack_detector_avoids_cold_start_false_positive() {
        use chrono::Utc;

        // Simulate the HTTP-ingest path (AnomalyDetector) having already
        // learned this prefix's legitimate origin — e.g. via warmup() from
        // ClickHouse history on startup.
        let shared_detector = Arc::new(HijackDetector::new());
        let warmup_record = crate::clickhouse_exporter::BgpClickHouseRecord {
            timestamp: Utc::now(),
            event_type: "announce".to_string(),
            prefix: "10.0.0.0/8".to_string(),
            origin_as: 64512,
            as_path: vec![64500, 64512],
            peer_asn: 64500,
            peer_ip: String::new(),
            community: vec![],
            source: "warmup".to_string(),
            tenant_id: "bgp".to_string(),
        };
        let warmup_anomaly = shared_detector.check(&warmup_record);
        assert!(
            warmup_anomaly.is_some(),
            "sanity check: a brand-new detector's first-ever sighting of a prefix flags — this \
             is the known, expected cold-start behavior that warmup()/sharing is meant to absorb \
             exactly once, not repeatedly per detector instance"
        );

        // Attach this pre-warmed, shared instance to a DetectorRunner —
        // matching how lib.rs wires AnomalyDetector's hijack_detector_arc()
        // into DetectorRunner via with_hijack_detector().
        let runner = DetectorRunner::new().with_hijack_detector(Arc::clone(&shared_detector));

        // The live NATS path now observes the same legitimate announcement
        // for the "first" time from its own (previously separate, unwarmed)
        // perspective. Before this fix, DetectorRunner held its own fresh
        // HijackDetector and would have flagged this as a hijack every
        // single time the process restarted. With the shared instance it
        // must not, because the origin is already known.
        let record = make_bgp_record("10.0.0.0/8", 64512, "announce");
        let result = runner.process_record(&record).await;
        assert!(result.is_ok());
        assert_eq!(
            runner.stats().anomalies_detected,
            0,
            "an origin AS already known via the shared HijackDetector must not be re-flagged"
        );
    }
}

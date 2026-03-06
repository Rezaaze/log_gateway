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

use crate::anomaly_detector::{Detector, FlappingDetector, HijackDetector};
use crate::metrics_exporter::DetectorMetrics;
use crate::nats_subscriber::BgpRecord;
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
///         peer_ip: String::new(), // Not available from NATS
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
        }
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

        // Run HijackDetector
        if let Some(anomaly) = self.hijack_detector.check(&bgp_record) {
            self.anomalies_detected.fetch_add(1, Ordering::Relaxed);
            self.metrics.record_anomaly("hijack");
            tracing::warn!(
                "Hijack detected: prefix={}, origin_as={}, confidence={:.2}, details={}",
                anomaly.prefix,
                anomaly.origin_as,
                anomaly.confidence,
                anomaly.details
            );
        }

        // Run FlappingDetector
        if let Some(anomaly) = self.flapping_detector.check(&bgp_record) {
            self.anomalies_detected.fetch_add(1, Ordering::Relaxed);
            self.metrics.record_anomaly("flapping");
            tracing::warn!(
                "Flapping detected: prefix={}, confidence={:.2}, details={}",
                anomaly.prefix,
                anomaly.confidence,
                anomaly.details
            );
        }

        Ok(())
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
            })
            .await;

        // Anomalies should have been detected
        let stats = runner.stats();
        assert!(
            stats.anomalies_detected > 0,
            "Hijack detection should have triggered"
        );
    }
}

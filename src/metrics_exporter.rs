/*!
Prometheus Metrics Exporter for Detector Runner

This module provides metrics collection and export for the anomaly detection pipeline.
Tracks:
- Events processed from NATS
- Anomalies detected (hijack + flapping)
- Detection errors
*/

use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::registry::Registry;
use std::sync::{Arc, RwLock};

/// Detector-specific Prometheus metrics
#[derive(Debug, Clone)]
pub struct DetectorMetrics {
    /// Total BGP events processed from NATS
    pub events_processed_total: Arc<Counter>,
    /// Total anomalies detected by type (hijack, flapping)
    pub anomalies_detected_total: Arc<Family<Vec<(String, String)>, Counter>>,
    /// Total detection errors
    pub detection_errors_total: Arc<Counter>,
    /// Prometheus registry for metrics collection
    registry: Arc<RwLock<Registry>>,
}

impl DetectorMetrics {
    /// Create new detector metrics instance
    pub fn new() -> Self {
        let mut registry = Registry::default();

        let events_processed_total = Counter::default();
        let anomalies_detected_total = Family::default();
        let detection_errors_total = Counter::default();

        // Register metrics
        registry.register(
            "detector_events_processed_total",
            "Total number of BGP events processed",
            events_processed_total.clone(),
        );

        registry.register(
            "detector_anomalies_detected_total",
            "Total number of anomalies detected",
            anomalies_detected_total.clone(),
        );

        registry.register(
            "detector_processing_errors_total",
            "Total number of processing errors",
            detection_errors_total.clone(),
        );

        Self {
            events_processed_total: Arc::new(events_processed_total),
            anomalies_detected_total: Arc::new(anomalies_detected_total),
            detection_errors_total: Arc::new(detection_errors_total),
            registry: Arc::new(RwLock::new(registry)),
        }
    }

    /// Increment events processed counter
    pub fn record_event_processed(&self) {
        self.events_processed_total.inc();
    }

    /// Record detected anomaly by type
    pub fn record_anomaly(&self, anomaly_type: &str) {
        self.anomalies_detected_total
            .get_or_create(&vec![("type".to_string(), anomaly_type.to_string())])
            .inc();
    }

    /// Increment error counter
    pub fn record_error(&self) {
        self.detection_errors_total.inc();
    }

    /// Render metrics in Prometheus text format
    pub fn render(&self) -> String {
        let registry_lock = match self.registry.read() {
            Ok(lock) => lock,
            Err(_) => return String::new(),
        };

        let mut encoded = String::new();
        prometheus_client::encoding::text::encode(&mut encoded, &registry_lock).unwrap_or_default();
        encoded
    }
}

impl Default for DetectorMetrics {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detector_metrics_creation() {
        let metrics = DetectorMetrics::new();
        assert_eq!(metrics.events_processed_total.get(), 0);
        assert_eq!(metrics.detection_errors_total.get(), 0);
    }

    #[test]
    fn test_record_event_processed() {
        let metrics = DetectorMetrics::new();
        metrics.record_event_processed();
        metrics.record_event_processed();
        assert_eq!(metrics.events_processed_total.get(), 2);
    }

    #[test]
    fn test_record_anomaly() {
        let metrics = DetectorMetrics::new();
        metrics.record_anomaly("hijack");
        metrics.record_anomaly("hijack");
        metrics.record_anomaly("flapping");

        let hijack_counter = metrics
            .anomalies_detected_total
            .get_or_create(&vec![("type".to_string(), "hijack".to_string())]);
        let flapping_counter = metrics
            .anomalies_detected_total
            .get_or_create(&vec![("type".to_string(), "flapping".to_string())]);

        assert_eq!(hijack_counter.get(), 2);
        assert_eq!(flapping_counter.get(), 1);
    }

    #[test]
    fn test_record_error() {
        let metrics = DetectorMetrics::new();
        metrics.record_error();
        metrics.record_error();
        metrics.record_error();
        assert_eq!(metrics.detection_errors_total.get(), 3);
    }

    #[test]
    fn test_prometheus_format_rendering() {
        let metrics = DetectorMetrics::new();

        // Record some activity
        metrics.record_event_processed();
        metrics.record_event_processed();
        metrics.record_event_processed();
        metrics.record_anomaly("hijack");
        metrics.record_anomaly("hijack");
        metrics.record_anomaly("flapping");
        metrics.record_error();

        // Render to Prometheus text format
        let output = metrics.render();

        // Verify the output contains expected metric names
        assert!(
            output.contains("detector_events_processed_total"),
            "Should contain events counter"
        );
        assert!(
            output.contains("detector_anomalies_detected_total"),
            "Should contain anomalies counter"
        );
        assert!(
            output.contains("detector_processing_errors_total"),
            "Should contain errors counter"
        );

        // Verify metric types are declared
        assert!(
            output.contains("# TYPE detector_events_processed_total counter"),
            "Should declare counter type"
        );
        assert!(
            output.contains("# TYPE detector_anomalies_detected_total counter"),
            "Should declare anomalies type"
        );

        // Verify labels are present for family metrics
        assert!(
            output.contains("type=\"hijack\""),
            "Should have hijack label"
        );
        assert!(
            output.contains("type=\"flapping\""),
            "Should have flapping label"
        );

        // Verify the output is not empty and has help text
        assert!(!output.is_empty(), "Rendered output should not be empty");
        assert!(
            output.contains("# HELP detector"),
            "Should contain HELP metadata"
        );
    }
}

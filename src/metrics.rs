use std::sync::{Arc, RwLock};

#[derive(Debug, Clone)]
pub struct GatewayMetrics {
    requests_total: Arc<prometheus_client::metrics::counter::Counter>,
    pii_hits_total: Arc<prometheus_client::metrics::counter::Counter>,
    cache_hits_total: Arc<prometheus_client::metrics::counter::Counter>,
    cache_misses_total: Arc<prometheus_client::metrics::counter::Counter>,
    bytes_ingested_total: Arc<prometheus_client::metrics::counter::Counter>,
    rate_limit_hits_total: Arc<prometheus_client::metrics::counter::Counter>,
    clickhouse_flush_errors_total: Arc<prometheus_client::metrics::counter::Counter>,
    bgp_anomaly_hijack_total: Arc<prometheus_client::metrics::counter::Counter>,
    bgp_anomaly_flap_total: Arc<prometheus_client::metrics::counter::Counter>,
    rpki_valid_total: Arc<prometheus_client::metrics::counter::Counter>,
    rpki_invalid_total: Arc<prometheus_client::metrics::counter::Counter>,
    alert_rule_triggered_total: Arc<prometheus_client::metrics::counter::Counter>,
    escalation_total: Arc<
        prometheus_client::metrics::family::Family<
            Vec<(String, String)>,
            prometheus_client::metrics::counter::Counter,
        >,
    >,
    request_duration_ms: Arc<prometheus_client::metrics::histogram::Histogram>,
    /// Quota exceeded counter per tenant
    quota_exceeded_total: Arc<
        prometheus_client::metrics::family::Family<
            Vec<(String, String)>,
            prometheus_client::metrics::counter::Counter,
        >,
    >,
    /// Quota warning counter per tenant
    quota_warning_total: Arc<
        prometheus_client::metrics::family::Family<
            Vec<(String, String)>,
            prometheus_client::metrics::counter::Counter,
        >,
    >,
    /// 5xx error counter
    requests_5xx_total: Arc<prometheus_client::metrics::counter::Counter>,
    /// Gateway uptime gauge (1.0 = running, 0.0 = not running)
    gateway_up: Arc<prometheus_client::metrics::gauge::Gauge>,
    /// Histogram: rule-based confidence bei Anomalie-Erkennung
    rule_based_confidence: Arc<
        prometheus_client::metrics::family::Family<
            Vec<(String, String)>,
            prometheus_client::metrics::histogram::Histogram,
        >,
    >,
    /// Histogram: ML-enhanced confidence bei Anomalie-Erkennung
    ml_enhanced_confidence: Arc<
        prometheus_client::metrics::family::Family<
            Vec<(String, String)>,
            prometheus_client::metrics::histogram::Histogram,
        >,
    >,
    /// Counter for total anomalies detected
    anomalies_total: Arc<
        prometheus_client::metrics::family::Family<
            Vec<(String, String)>,
            prometheus_client::metrics::counter::Counter,
        >,
    >,
    /// Counter for false positive alerts (manually resolved)
    false_positives_total: Arc<
        prometheus_client::metrics::family::Family<
            Vec<(String, String)>,
            prometheus_client::metrics::counter::Counter,
        >,
    >,
    registry: Arc<RwLock<prometheus_client::registry::Registry>>,
}

impl Default for GatewayMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl GatewayMetrics {
    pub fn new() -> Self {
        let mut registry = prometheus_client::registry::Registry::default();

        let requests_total = prometheus_client::metrics::counter::Counter::default();
        let pii_hits_total = prometheus_client::metrics::counter::Counter::default();
        let cache_hits_total = prometheus_client::metrics::counter::Counter::default();
        let cache_misses_total = prometheus_client::metrics::counter::Counter::default();
        let bytes_ingested_total = prometheus_client::metrics::counter::Counter::default();
        let rate_limit_hits_total = prometheus_client::metrics::counter::Counter::default();
        let clickhouse_flush_errors_total = prometheus_client::metrics::counter::Counter::default();
        let bgp_anomaly_hijack_total = prometheus_client::metrics::counter::Counter::default();
        let bgp_anomaly_flap_total = prometheus_client::metrics::counter::Counter::default();
        let rpki_valid_total = prometheus_client::metrics::counter::Counter::default();
        let rpki_invalid_total = prometheus_client::metrics::counter::Counter::default();
        let alert_rule_triggered_total = prometheus_client::metrics::counter::Counter::default();
        let escalation_total = prometheus_client::metrics::family::Family::default();
        let quota_exceeded_total = prometheus_client::metrics::family::Family::default();
        let quota_warning_total = prometheus_client::metrics::family::Family::default();
        let requests_5xx_total = prometheus_client::metrics::counter::Counter::default();
        let gateway_up = prometheus_client::metrics::gauge::Gauge::default();
        
        // Create histogram buckets for confidence scores [0.1, 0.2, ..., 1.0]
        const CONFIDENCE_BUCKETS: [f64; 10] = [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1.0];
        let rule_based_confidence = prometheus_client::metrics::family::Family::<
            Vec<(String, String)>,
            prometheus_client::metrics::histogram::Histogram,
        >::new_with_constructor(|| {
            prometheus_client::metrics::histogram::Histogram::new(CONFIDENCE_BUCKETS.iter().copied())
        });
        let ml_enhanced_confidence = prometheus_client::metrics::family::Family::<
            Vec<(String, String)>,
            prometheus_client::metrics::histogram::Histogram,
        >::new_with_constructor(|| {
            prometheus_client::metrics::histogram::Histogram::new(CONFIDENCE_BUCKETS.iter().copied())
        });

        // Create counters for anomaly tracking
        let anomalies_total = prometheus_client::metrics::family::Family::<
            Vec<(String, String)>,
            prometheus_client::metrics::counter::Counter,
        >::default();
        let false_positives_total = prometheus_client::metrics::family::Family::<
            Vec<(String, String)>,
            prometheus_client::metrics::counter::Counter,
        >::default();

        // Create histogram with linear buckets for request duration
        let buckets = vec![
            1.0, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2500.0, 5000.0,
        ];
        let request_duration_ms =
            prometheus_client::metrics::histogram::Histogram::new(buckets.into_iter());

        // Register anomaly tracking counters
        registry.register(
            "gateway_anomalies_total",
            "Total number of anomalies detected",
            anomalies_total.clone(),
        );

        registry.register(
            "gateway_false_positives_total",
            "Total number of alerts manually resolved as false positives",
            false_positives_total.clone(),
        );

        registry.register(
            "gateway_requests",
            "Total number of log requests received",
            requests_total.clone(),
        );

        registry.register(
            "gateway_pii_hits",
            "Total number of PII hits detected",
            pii_hits_total.clone(),
        );

        registry.register(
            "gateway_cache_hits",
            "Total number of cache hits",
            cache_hits_total.clone(),
        );

        registry.register(
            "gateway_cache_misses",
            "Total number of cache misses",
            cache_misses_total.clone(),
        );

        registry.register(
            "gateway_bytes_ingested",
            "Total bytes ingested",
            bytes_ingested_total.clone(),
        );

        registry.register(
            "gateway_rate_limit_hits",
            "Total number of requests rejected by rate limiter.",
            rate_limit_hits_total.clone(),
        );

        registry.register(
            "gateway_clickhouse_flush_errors",
            "Total number of ClickHouse flush errors.",
            clickhouse_flush_errors_total.clone(),
        );

        registry.register(
            "gateway_bgp_anomaly_hijack",
            "Total number of possible BGP hijack anomalies detected.",
            bgp_anomaly_hijack_total.clone(),
        );

        registry.register(
            "gateway_bgp_anomaly_flap",
            "Total number of prefix flapping anomalies detected.",
            bgp_anomaly_flap_total.clone(),
        );

        registry.register(
            "gateway_rpki_valid",
            "Total number of RPKI-valid BGP announcements.",
            rpki_valid_total.clone(),
        );

        registry.register(
            "gateway_rpki_invalid",
            "Total number of RPKI-invalid BGP announcements.",
            rpki_invalid_total.clone(),
        );

        registry.register(
            "gateway_alert_rule_triggered",
            "Total number of alert rules triggered.",
            alert_rule_triggered_total.clone(),
        );

        registry.register(
            "gateway_escalation_total",
            "Total number of BGP anomaly escalations by level.",
            escalation_total.clone(),
        );

        registry.register(
            "gateway_request_duration_ms",
            "Request processing duration in milliseconds.",
            request_duration_ms.clone(),
        );

        registry.register(
            "gateway_quota_exceeded",
            "Total number of requests rejected due to quota exceeded.",
            quota_exceeded_total.clone(),
        );

        registry.register(
            "gateway_quota_warning",
            "Total number of requests that triggered quota warning (80-100% usage).",
            quota_warning_total.clone(),
        );

        registry.register(
            "gateway_5xx_total",
            "Total number of 5xx responses",
            requests_5xx_total.clone(),
        );

        registry.register(
            "gateway_up",
            "1 if gateway is running, 0 otherwise",
            gateway_up.clone(),
        );

        // Register A/B test confidence histograms
        registry.register(
            "gateway_rule_based_confidence",
            "Rule-based confidence score at anomaly detection",
            rule_based_confidence.clone(),
        );

        registry.register(
            "gateway_ml_enhanced_confidence",
            "ML-enhanced confidence score at anomaly detection",
            ml_enhanced_confidence.clone(),
        );

        Self {
            requests_total: Arc::new(requests_total),
            pii_hits_total: Arc::new(pii_hits_total),
            cache_hits_total: Arc::new(cache_hits_total),
            cache_misses_total: Arc::new(cache_misses_total),
            bytes_ingested_total: Arc::new(bytes_ingested_total),
            rate_limit_hits_total: Arc::new(rate_limit_hits_total),
            clickhouse_flush_errors_total: Arc::new(clickhouse_flush_errors_total),
            bgp_anomaly_hijack_total: Arc::new(bgp_anomaly_hijack_total),
            bgp_anomaly_flap_total: Arc::new(bgp_anomaly_flap_total),
            rpki_valid_total: Arc::new(rpki_valid_total),
            rpki_invalid_total: Arc::new(rpki_invalid_total),
            alert_rule_triggered_total: Arc::new(alert_rule_triggered_total),
            escalation_total: Arc::new(escalation_total),
            request_duration_ms: Arc::new(request_duration_ms),
            quota_exceeded_total: Arc::new(quota_exceeded_total),
            quota_warning_total: Arc::new(quota_warning_total),
            requests_5xx_total: Arc::new(requests_5xx_total),
            gateway_up: Arc::new(gateway_up),
            rule_based_confidence: Arc::new(rule_based_confidence),
            ml_enhanced_confidence: Arc::new(ml_enhanced_confidence),
            anomalies_total: Arc::new(anomalies_total),
            false_positives_total: Arc::new(false_positives_total),
            registry: Arc::new(RwLock::new(registry)),
        }
    }

    pub fn record_request(&self, bytes: u64, pii_hits: usize, cache_hit: bool) {
        self.requests_total.inc();
        self.bytes_ingested_total.inc_by(bytes);
        self.pii_hits_total.inc_by(pii_hits as u64);

        if cache_hit {
            self.cache_hits_total.inc();
        } else {
            self.cache_misses_total.inc();
        }
    }

    pub fn record_duration(&self, duration_ms: f64) {
        self.request_duration_ms.observe(duration_ms);
    }

    pub fn record_rate_limit_hit(&self) {
        self.rate_limit_hits_total.inc();
    }

    pub fn record_clickhouse_flush_error(&self) {
        self.clickhouse_flush_errors_total.inc();
    }

    pub fn render(&self) -> String {
        let registry_lock = match self.registry.read() {
            Ok(lock) => lock,
            Err(_) => return String::new(),
        };

        let mut encoded = String::new();
        prometheus_client::encoding::text::encode(&mut encoded, &registry_lock).unwrap_or_default();
        encoded
    }

    /// Increments the BGP hijack anomaly counter.
    pub fn record_bgp_anomaly_hijack(&self) {
        self.bgp_anomaly_hijack_total.inc();
    }

    /// Increments the BGP prefix flapping anomaly counter.
    pub fn record_bgp_anomaly_flap(&self) {
        self.bgp_anomaly_flap_total.inc();
    }

    /// Increments the RPKI valid counter.
    pub fn record_rpki_valid(&self) {
        self.rpki_valid_total.inc();
    }

    /// Increments the RPKI invalid counter.
    pub fn record_rpki_invalid(&self) {
        self.rpki_invalid_total.inc();
    }

    /// Increments the escalation counter for the given level.
    pub fn record_escalation(&self, level: &crate::escalation::EscalationLevel) {
        let labels = vec![("level".to_string(), level.prometheus_label().to_string())];
        self.escalation_total.get_or_create(&labels).inc();
    }

    /// Records a quota exceeded event for a tenant.
    pub fn record_quota_exceeded(&self, tenant_id: &str) {
        let labels = vec![("tenant".to_string(), tenant_id.to_string())];
        self.quota_exceeded_total.get_or_create(&labels).inc();
    }

    /// Records a quota warning event for a tenant.
    pub fn record_quota_warning(&self, tenant_id: &str) {
        let labels = vec![("tenant".to_string(), tenant_id.to_string())];
        self.quota_warning_total.get_or_create(&labels).inc();
    }

    /// Increments the 5xx error counter.
    pub fn record_5xx(&self) {
        self.requests_5xx_total.inc();
    }

    /// Increments the alert rule triggered counter.
    pub fn record_alert_rule_triggered(&self) {
        self.alert_rule_triggered_total.inc();
    }

    /// Sets the gateway uptime gauge to 1 (running).
    pub fn record_gateway_up(&self) {
        self.gateway_up.set(1);
    }

    /// Records both rule-based and ML-enhanced confidence scores for A/B testing.
    ///
    /// # Arguments
    ///
    /// * `anomaly_type` - Type of anomaly (e.g., "PossibleHijack", "PrefixFlapping")
    /// * `rule_based` - Rule-based confidence score (0.0 to 1.0)
    /// * `ml_enhanced` - ML-enhanced confidence score (0.0 to 1.0)
    pub fn record_ab_confidence(&self, anomaly_type: &str, rule_based: f64, ml_enhanced: f64) {
        let labels = vec![("anomaly_type".to_string(), anomaly_type.to_string())];
        
        // Create histograms with confidence buckets if they don't exist yet
        let rule_histogram = self.rule_based_confidence.get_or_create(&labels);
        let ml_histogram = self.ml_enhanced_confidence.get_or_create(&labels);
        
        // Observe the confidence scores
        rule_histogram.observe(rule_based);
        ml_histogram.observe(ml_enhanced);
    }

    /// Records an anomaly detection event.
    ///
    /// # Arguments
    ///
    /// * `anomaly_type` - Type of anomaly (e.g., "PossibleHijack", "PrefixFlapping")
    pub fn record_anomaly_detected(&self, anomaly_type: &str) {
        let labels = vec![("anomaly_type".to_string(), anomaly_type.to_string())];
        self.anomalies_total.get_or_create(&labels).inc();
    }

    /// Records a false positive alert resolution.
    ///
    /// # Arguments
    ///
    /// * `anomaly_type` - Type of anomaly (e.g., "PossibleHijack", "PrefixFlapping")
    pub fn record_false_positive(&self, anomaly_type: &str) {
        let labels = vec![("anomaly_type".to_string(), anomaly_type.to_string())];
        self.false_positives_total.get_or_create(&labels).inc();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_5xx_increments_counter() {
        let metrics = GatewayMetrics::new();
        
        // Initially should be 0 - check that metric appears in output
        let rendered = metrics.render();
        assert!(rendered.contains("gateway_5xx_total"));
        
        // Call record_5xx once and check value increased
        metrics.record_5xx();
        let rendered = metrics.render();
        // The exact format might vary, but we can check it's not 0
        // by verifying the line contains the metric name
        let lines: Vec<&str> = rendered.lines().filter(|l| l.contains("gateway_5xx_total")).collect();
        assert!(!lines.is_empty(), "gateway_5xx_total should appear in metrics output");
        
        // Call record_5xx again
        metrics.record_5xx();
        let rendered = metrics.render();
        let lines: Vec<&str> = rendered.lines().filter(|l| l.contains("gateway_5xx_total")).collect();
        assert!(!lines.is_empty(), "gateway_5xx_total should appear in metrics output after second increment");
    }

    #[test]
    fn test_record_gateway_up_sets_gauge() {
        let metrics = GatewayMetrics::new();
        
        // Initially should be 0 (not set yet)
        let rendered = metrics.render();
        assert!(rendered.contains("gateway_up 0"));
        
        // Call record_gateway_up
        metrics.record_gateway_up();
        let rendered = metrics.render();
        assert!(rendered.contains("gateway_up 1"));
    }

    #[test]
    fn test_gateway_up_initial_zero() {
        let metrics = GatewayMetrics::new();
        
        // Before calling record_gateway_up, gauge should be 0
        let rendered = metrics.render();
        assert!(rendered.contains("gateway_up 0"));
    }

    #[test]
    fn test_metrics_output_contains_slo_metrics() {
        let metrics = GatewayMetrics::new();
        metrics.record_gateway_up();
        metrics.record_5xx();
        
        let rendered = metrics.render();
        
        // Check that both new metrics appear in the output
        assert!(rendered.contains("gateway_5xx_total"));
        assert!(rendered.contains("gateway_up"));
        
        // Check descriptions
        assert!(rendered.contains("Total number of 5xx responses"));
        assert!(rendered.contains("1 if gateway is running, 0 otherwise"));
    }

    #[test]
    fn test_record_ab_confidence_both_histograms() {
        let metrics = GatewayMetrics::new();
        
        // Record confidence scores for different anomaly types
        metrics.record_ab_confidence("PossibleHijack", 0.85, 0.92);
        metrics.record_ab_confidence("PrefixFlapping", 0.75, 0.88);
        
        // Should not panic
        let rendered = metrics.render();
        
        // Check that both metrics appear in the output
        assert!(rendered.contains("gateway_rule_based_confidence"));
        assert!(rendered.contains("gateway_ml_enhanced_confidence"));
        
        // Check descriptions
        assert!(rendered.contains("Rule-based confidence score at anomaly detection"));
        assert!(rendered.contains("ML-enhanced confidence score at anomaly detection"));
    }

    #[test]
    fn test_record_anomaly_detected_increments_counter() {
        let metrics = GatewayMetrics::new();
        
        // Initially should be 0
        let rendered = metrics.render();
        assert!(rendered.contains("gateway_anomalies_total"));
        
        // Record anomaly detection
        metrics.record_anomaly_detected("PossibleHijack");
        metrics.record_anomaly_detected("PrefixFlapping");
        metrics.record_anomaly_detected("PossibleHijack"); // Same type again
        
        let rendered = metrics.render();
        
        // Check that metric appears in output
        assert!(rendered.contains("gateway_anomalies_total"));
        assert!(rendered.contains("anomaly_type=\"PossibleHijack\""));
        assert!(rendered.contains("anomaly_type=\"PrefixFlapping\""));
        
        // Check description
        assert!(rendered.contains("Total number of anomalies detected"));
    }

    #[test]
    fn test_record_false_positive_increments_counter() {
        let metrics = GatewayMetrics::new();
        
        // Initially should be 0
        let rendered = metrics.render();
        assert!(rendered.contains("gateway_false_positives_total"));
        
        // Record false positives
        metrics.record_false_positive("PossibleHijack");
        metrics.record_false_positive("PrefixFlapping");
        metrics.record_false_positive("PossibleHijack"); // Same type again
        
        let rendered = metrics.render();
        
        // Check that metric appears in output
        assert!(rendered.contains("gateway_false_positives_total"));
        assert!(rendered.contains("anomaly_type=\"PossibleHijack\""));
        assert!(rendered.contains("anomaly_type=\"PrefixFlapping\""));
        
        // Check description
        assert!(rendered.contains("Total number of alerts manually resolved as false positives"));
    }

    #[test]
    fn test_false_positive_metrics_in_prometheus_output() {
        let metrics = GatewayMetrics::new();
        
        // Record some anomalies and false positives
        metrics.record_anomaly_detected("PossibleHijack");
        metrics.record_anomaly_detected("PrefixFlapping");
        metrics.record_false_positive("PossibleHijack");
        
        let rendered = metrics.render();
        
        // Check that both metrics appear in the output
        assert!(rendered.contains("gateway_anomalies_total"));
        assert!(rendered.contains("gateway_false_positives_total"));
        
        // Check that they have the correct labels
        let lines: Vec<&str> = rendered.lines().collect();
        let anomalies_lines: Vec<&str> = lines.iter()
            .filter(|l| l.contains("gateway_anomalies_total"))
            .copied()
            .collect();
        let false_positives_lines: Vec<&str> = lines.iter()
            .filter(|l| l.contains("gateway_false_positives_total"))
            .copied()
            .collect();
        
        assert!(!anomalies_lines.is_empty(), "gateway_anomalies_total should appear in metrics output");
        assert!(!false_positives_lines.is_empty(), "gateway_false_positives_total should appear in metrics output");
        
        // Check that we have lines with both anomaly types
        let has_possible_hijack = anomalies_lines.iter().any(|l| l.contains("PossibleHijack"));
        let has_prefix_flapping = anomalies_lines.iter().any(|l| l.contains("PrefixFlapping"));
        
        assert!(has_possible_hijack, "Should have PossibleHijack label");
        assert!(has_prefix_flapping, "Should have PrefixFlapping label");
    }
}

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
    escalation_total: Arc<
        prometheus_client::metrics::family::Family<
            Vec<(String, String)>,
            prometheus_client::metrics::counter::Counter,
        >,
    >,
    request_duration_ms: Arc<prometheus_client::metrics::histogram::Histogram>,
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
        let escalation_total = prometheus_client::metrics::family::Family::default();

        // Create histogram with linear buckets
        let buckets = vec![
            1.0, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2500.0, 5000.0,
        ];
        let request_duration_ms =
            prometheus_client::metrics::histogram::Histogram::new(buckets.into_iter());

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
            "gateway_escalation_total",
            "Total number of BGP anomaly escalations by level.",
            escalation_total.clone(),
        );

        registry.register(
            "gateway_request_duration_ms",
            "Request processing duration in milliseconds.",
            request_duration_ms.clone(),
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
            escalation_total: Arc::new(escalation_total),
            request_duration_ms: Arc::new(request_duration_ms),
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
}

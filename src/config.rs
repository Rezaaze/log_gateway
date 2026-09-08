use anyhow::Result;
use serde::Deserialize;

fn default_otlp_endpoint() -> String {
    "http://localhost:4317".to_string()
}

fn default_service_name() -> String {
    "log-gateway".to_string()
}

fn default_loki_endpoint() -> String {
    "http://localhost:3100".to_string()
}

fn default_loki_service_name() -> String {
    "log-gateway".to_string()
}

fn default_true() -> bool {
    true
}

fn default_check_interval() -> u64 {
    60
}

fn default_timeout_secs() -> u64 {
    300
}

#[derive(Debug, Deserialize, Clone)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TlsConfig {
    pub enabled: bool,
    pub cert_path: String,
    pub key_path: String,
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            cert_path: "certs/cert.pem".to_string(),
            key_path: "certs/key.pem".to_string(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct CacheConfig {
    pub max_capacity: u64,
    pub ttl_seconds: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct CostConfig {
    pub enabled: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub struct MetricsConfig {
    pub enabled: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SinkConfig {
    pub enabled: bool,
    pub output_dir: String,
    pub max_buffer_size: usize,
    pub flush_interval_secs: u64,
    pub compress: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub struct RateLimitConfig {
    pub enabled: bool,
    pub requests_per_second: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            requests_per_second: 1000,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct S3Config {
    pub enabled: bool,
    pub endpoint_url: Option<String>,
    pub bucket: String,
    pub region: String,
    pub prefix: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub delete_after_upload: bool,
}

impl Default for S3Config {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint_url: None,
            bucket: String::new(),
            region: "us-east-1".to_string(),
            prefix: "logs/".to_string(),
            access_key_id: String::new(),
            secret_access_key: String::new(),
            delete_after_upload: false,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct ClickHouseConfig {
    pub enabled: bool,
    pub url: String,
    pub database: String,
    pub table: String,
    pub batch_size: usize,
    pub flush_interval_secs: u64,
}

impl Default for ClickHouseConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: "http://clickhouse:8123".to_string(),
            database: "bgp".to_string(),
            table: "bgp_events".to_string(),
            batch_size: 1000,
            flush_interval_secs: 5,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct RpkiConfig {
    pub enabled: bool,
    /// Either a self-hosted validator's base address
    /// (`http://routinator:8323`, dump served under `/json`) or the full URL
    /// of a public VRP feed ending in `.json`, which is then used verbatim.
    pub routinator_url: String,
    /// How often the VRP table is refetched. A self-hosted validator can be
    /// polled aggressively; a public feed is a ~100 MB download per fetch and
    /// belongs to someone else, so poll it sparingly (3600 s or more).
    #[serde(default = "default_rpki_refresh_secs")]
    pub refresh_interval_secs: u64,
}

fn default_rpki_refresh_secs() -> u64 {
    600
}

impl Default for RpkiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            routinator_url: "http://routinator:8323".to_string(),
            refresh_interval_secs: default_rpki_refresh_secs(),
        }
    }
}

/// Configuration for a single webhook target.
#[derive(Debug, Deserialize, Clone)]
pub struct WebhookTargetConfig {
    /// Type of webhook target: "slack" or "generic"
    pub target_type: String,
    /// URL to send webhook requests to
    pub url: String,
    /// Channel for Slack webhooks (only for target_type = "slack")
    #[serde(default)]
    pub channel: Option<String>,
    /// Custom headers for generic webhooks (only for target_type = "generic")
    #[serde(default)]
    pub headers: Option<std::collections::HashMap<String, String>>,
}

/// Configuration for webhook notifications.
#[derive(Debug, Deserialize, Clone, Default)]
pub struct WebhooksConfig {
    /// Whether webhook notifications are enabled
    #[serde(default)]
    pub enabled: bool,
    /// List of webhook targets
    #[serde(default)]
    pub targets: Vec<WebhookTargetConfig>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct TelemetryConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_otlp_endpoint")]
    pub otlp_endpoint: String,
    #[serde(default = "default_service_name")]
    pub service_name: String,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            otlp_endpoint: default_otlp_endpoint(),
            service_name: default_service_name(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct LokiConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_loki_endpoint")]
    pub endpoint: String,
    #[serde(default = "default_loki_service_name")]
    pub service_name: String,
}

impl Default for LokiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: default_loki_endpoint(),
            service_name: default_loki_service_name(),
        }
    }
}

/// Configuration for automatic escalation of unacknowledged alerts.
#[derive(Debug, Deserialize, Clone)]
pub struct EscalationConfig {
    #[serde(default = "default_true")]
    pub auto_escalation_enabled: bool,
    #[serde(default = "default_check_interval")]
    pub check_interval_secs: u64,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

impl Default for EscalationConfig {
    fn default() -> Self {
        Self {
            auto_escalation_enabled: default_true(),
            check_interval_secs: default_check_interval(),
            timeout_secs: default_timeout_secs(),
        }
    }
}

/// Configuration for SMTP email reporting
#[derive(Debug, Deserialize, Clone)]
pub struct SmtpConfig {
    #[serde(default = "default_false")]
    pub enabled: bool,
    #[serde(default = "default_smtp_host")]
    pub host: String,
    #[serde(default = "default_smtp_port")]
    pub port: u16,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default = "default_smtp_from")]
    pub from: String,
    #[serde(default)]
    pub to: Vec<String>,
}

fn default_false() -> bool {
    false
}

fn default_smtp_host() -> String {
    "localhost".to_string()
}

fn default_smtp_port() -> u16 {
    587
}

fn default_smtp_from() -> String {
    "gateway@localhost".to_string()
}

impl Default for SmtpConfig {
    fn default() -> Self {
        Self {
            enabled: default_false(),
            host: default_smtp_host(),
            port: default_smtp_port(),
            username: String::new(),
            password: String::new(),
            from: default_smtp_from(),
            to: Vec::new(),
        }
    }
}

/// Configuration for NATS subscription
#[derive(Debug, Deserialize, Clone)]
pub struct NatsConfig {
    #[serde(default = "default_false")]
    pub enabled: bool,
    #[serde(default = "default_nats_url")]
    pub url: String,
    #[serde(default = "default_nats_subject")]
    pub subject: String,
}

fn default_nats_url() -> String {
    "nats://localhost:4222".to_string()
}

fn default_nats_subject() -> String {
    "bgp.events".to_string()
}

impl Default for NatsConfig {
    fn default() -> Self {
        Self {
            enabled: default_false(),
            url: default_nats_url(),
            subject: default_nats_subject(),
        }
    }
}

/// Configuration for baseline model snapshot persistence
#[derive(Debug, Deserialize, Clone)]
pub struct SnapshotConfig {
    #[serde(default = "default_snapshot_dir")]
    pub dir: String,
    #[serde(default = "default_snapshot_retention")]
    pub retention_days: u32,
}

fn default_snapshot_dir() -> String {
    "/data/snapshots".to_string()
}

fn default_snapshot_retention() -> u32 {
    7
}

impl Default for SnapshotConfig {
    fn default() -> Self {
        Self {
            dir: default_snapshot_dir(),
            retention_days: default_snapshot_retention(),
        }
    }
}

/// Configuration for the wave-physics propagation anomaly detector
/// (`PropagationAggregator` + `WaveAnomalyDetector`). Safe to leave enabled
/// even without a baseline file present — the detector degrades to
/// "no anomalies" until a baseline is built via `tools/baseline_builder`.
#[derive(Debug, Deserialize, Clone)]
pub struct WaveConfig {
    #[serde(default = "default_wave_enabled")]
    pub enabled: bool,
    #[serde(default = "default_wave_baseline_path")]
    pub baseline_path: String,
}

fn default_wave_enabled() -> bool {
    true
}

fn default_wave_baseline_path() -> String {
    "data/baselines/baseline.bin.zst".to_string()
}

impl Default for WaveConfig {
    fn default() -> Self {
        Self {
            enabled: default_wave_enabled(),
            baseline_path: default_wave_baseline_path(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct GatewayConfig {
    pub server: ServerConfig,
    pub cache: CacheConfig,
    pub cost: CostConfig,
    pub metrics: MetricsConfig,
    pub sink: SinkConfig,
    pub rate_limit: RateLimitConfig,
    pub s3: S3Config,
    pub clickhouse: ClickHouseConfig,
    pub rpki: RpkiConfig,
    pub tls: TlsConfig,
    /// Webhook notification configuration
    #[serde(default)]
    pub webhooks: WebhooksConfig,
    /// Telemetry configuration for distributed tracing
    #[serde(default)]
    pub telemetry: TelemetryConfig,
    /// Loki logging configuration
    #[serde(default)]
    pub loki: LokiConfig,
    /// Automatic escalation configuration
    #[serde(default)]
    pub escalation: EscalationConfig,
    /// SMTP email reporting configuration
    #[serde(default)]
    pub smtp: SmtpConfig,
    /// NATS subscription configuration for BGP events
    #[serde(default)]
    pub nats: NatsConfig,
    /// Baseline model snapshot configuration
    #[serde(default)]
    pub snapshot: SnapshotConfig,
    /// Wave-physics propagation anomaly detector configuration
    #[serde(default)]
    pub wave: WaveConfig,
}

impl GatewayConfig {
    pub fn load() -> Result<Self> {
        let config_dir = std::env::current_dir()?.join("config");
        let config = config::Config::builder()
            .add_source(config::File::from(config_dir.join("default.toml")))
            .add_source(
                config::Environment::with_prefix("GATEWAY")
                    .separator("__")
                    .try_parsing(true),
            )
            .build()?;

        let gateway_config = config.try_deserialize()?;
        Ok(gateway_config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_webhooks_config_default() {
        let cfg = WebhooksConfig::default();
        assert!(!cfg.enabled);
        assert!(cfg.targets.is_empty());
    }

    #[test]
    fn test_webhook_target_config_slack_deserialization() {
        let toml = r##"
            enabled = true
            [[targets]]
            target_type = "slack"
            url = "https://hooks.slack.com/services/XXX"
            channel = "#alerts"
        "##;
        let cfg: WebhooksConfig = toml::from_str(toml).unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.targets.len(), 1);
        assert_eq!(cfg.targets[0].target_type, "slack");
        assert_eq!(cfg.targets[0].url, "https://hooks.slack.com/services/XXX");
        assert_eq!(cfg.targets[0].channel.as_deref(), Some("#alerts"));
    }

    #[test]
    fn test_webhook_target_config_generic_deserialization() {
        let toml = r#"
            enabled = true
            [[targets]]
            target_type = "generic"
            url = "https://webhook.example.com/alert"
            [targets.headers]
            X-API-Key = "secret"
        "#;
        let cfg: WebhooksConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.targets[0].target_type, "generic");
        let headers = cfg.targets[0].headers.as_ref().unwrap();
        assert_eq!(headers.get("X-API-Key").map(String::as_str), Some("secret"));
    }

    #[test]
    fn test_telemetry_config_default() {
        let cfg = TelemetryConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.otlp_endpoint, "http://localhost:4317");
        assert_eq!(cfg.service_name, "log-gateway");
    }

    #[test]
    fn test_telemetry_config_deserialization() {
        let toml = r#"
            enabled = true
            otlp_endpoint = "http://jaeger:4317"
            service_name = "my-service"
        "#;
        let cfg: TelemetryConfig = toml::from_str(toml).unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.otlp_endpoint, "http://jaeger:4317");
        assert_eq!(cfg.service_name, "my-service");
    }

    #[test]
    fn test_telemetry_config_partial_deserialization() {
        let toml = r#"
            enabled = true
        "#;
        let cfg: TelemetryConfig = toml::from_str(toml).unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.otlp_endpoint, "http://localhost:4317"); // default
        assert_eq!(cfg.service_name, "log-gateway"); // default
    }

    #[test]
    fn test_loki_config_default() {
        let cfg = LokiConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.endpoint, "http://localhost:3100");
        assert_eq!(cfg.service_name, "log-gateway");
    }

    #[test]
    fn test_loki_config_deserialization() {
        let toml = r#"
            enabled = true
            endpoint = "http://loki:3100"
            service_name = "custom-service"
        "#;
        let cfg: LokiConfig = toml::from_str(toml).unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.endpoint, "http://loki:3100");
        assert_eq!(cfg.service_name, "custom-service");
    }

    #[test]
    fn test_loki_config_partial_deserialization() {
        let toml = r#"
            enabled = true
        "#;
        let cfg: LokiConfig = toml::from_str(toml).unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.endpoint, "http://localhost:3100"); // default
        assert_eq!(cfg.service_name, "log-gateway"); // default
    }

    #[test]
    fn test_escalation_config_default() {
        let cfg = EscalationConfig::default();
        assert!(cfg.auto_escalation_enabled);
        assert_eq!(cfg.check_interval_secs, 60);
        assert_eq!(cfg.timeout_secs, 300);
    }

    #[test]
    fn test_escalation_config_deserialization() {
        let toml = r#"
            auto_escalation_enabled = false
            check_interval_secs = 30
            timeout_secs = 120
        "#;
        let cfg: EscalationConfig = toml::from_str(toml).unwrap();
        assert!(!cfg.auto_escalation_enabled);
        assert_eq!(cfg.check_interval_secs, 30);
        assert_eq!(cfg.timeout_secs, 120);
    }

    #[test]
    fn test_escalation_config_partial_deserialization() {
        let toml = r#"
            auto_escalation_enabled = false
        "#;
        let cfg: EscalationConfig = toml::from_str(toml).unwrap();
        assert!(!cfg.auto_escalation_enabled);
        assert_eq!(cfg.check_interval_secs, 60); // default
        assert_eq!(cfg.timeout_secs, 300); // default
    }
}

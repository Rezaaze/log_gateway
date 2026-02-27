use anyhow::Result;
use serde::Deserialize;

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
pub struct GatewayConfig {
    pub server: ServerConfig,
    pub cache: CacheConfig,
    pub cost: CostConfig,
    pub metrics: MetricsConfig,
    pub sink: SinkConfig,
    pub rate_limit: RateLimitConfig,
    pub s3: S3Config,
    pub tls: TlsConfig,
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

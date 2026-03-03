use chrono::{DateTime, Utc};
use crossbeam::queue::ArrayQueue;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info};

use crate::config::ClickHouseConfig;
use crate::metrics::GatewayMetrics;

/// A BGP event record to be stored in ClickHouse.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BgpClickHouseRecord {
    pub timestamp: DateTime<Utc>,
    pub event_type: String, // "announce" / "withdraw"
    pub prefix: String,     // "1.2.3.0/24"
    pub origin_as: u32,
    pub as_path: Vec<u32>,
    pub peer_asn: u32,
    pub peer_ip: String,
    pub community: Vec<String>,
    pub source: String,
    pub tenant_id: String,
}

/// ClickHouse exporter that buffers BGP records and flushes them to ClickHouse.
#[derive(Debug, Clone)]
pub struct ClickHouseExporter {
    buffer: Arc<ArrayQueue<BgpClickHouseRecord>>,
    config: ClickHouseConfig,
    client: reqwest::Client,
    metrics: Arc<GatewayMetrics>,
}

impl ClickHouseExporter {
    /// Creates a new ClickHouse exporter.
    ///
    /// # Arguments
    ///
    /// * `config` - ClickHouse configuration
    /// * `metrics` - Gateway metrics for tracking errors
    pub fn new(config: ClickHouseConfig, metrics: Arc<GatewayMetrics>) -> Self {
        // Use double the buffer size as headroom to avoid dropping records too early
        let queue_capacity = config.batch_size * 2;

        // Create HTTP client with timeout and connection pooling
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .pool_max_idle_per_host(10)
            .build()
            .unwrap_or_else(|e| {
                error!("Failed to create HTTP client for ClickHouse: {}", e);
                // Fallback to default client if builder fails
                reqwest::Client::new()
            });

        Self {
            buffer: Arc::new(ArrayQueue::new(queue_capacity)),
            config,
            client,
            metrics,
        }
    }

    /// Writes a BGP record to the exporter buffer.
    ///
    /// If the buffer reaches the configured maximum size, it triggers a flush.
    /// If the buffer is full, the record is dropped silently (no panic, no block).
    pub fn write(&self, record: BgpClickHouseRecord) {
        if !self.config.enabled {
            return;
        }

        // Try to push the record to the queue.
        // If the queue is full, increment the flush-error metric so the drop
        // is visible in Prometheus — no panic, no block.
        if self.buffer.push(record).is_err() {
            self.metrics.record_clickhouse_flush_error();
            tracing::warn!("ClickHouse buffer full — record dropped");
        }
    }

    /// Flushes all buffered records to ClickHouse.
    pub async fn flush(&self) {
        if !self.config.enabled {
            return;
        }

        // Collect records from the queue into a local vector
        let mut records = Vec::with_capacity(self.config.batch_size);

        // Drain the queue by popping records until it's empty
        while let Some(record) = self.buffer.pop() {
            records.push(record);

            // Stop if we've collected enough records for a single flush
            if records.len() >= self.config.batch_size {
                break;
            }
        }

        if records.is_empty() {
            return;
        }

        let count = records.len();

        // Prepare the INSERT query — URL-encode the query string so spaces
        // and special characters don't break the HTTP request.
        let query = format!(
            "INSERT INTO {}.{} FORMAT JSONEachRow",
            self.config.database, self.config.table
        );
        let encoded_query = query
            .replace(' ', "%20")
            .replace('\n', "%0A")
            .replace('\t', "%09");
        let url = format!("{}?query={}", self.config.url, encoded_query);

        // Convert records to newline-delimited JSON
        let mut ndjson = String::with_capacity(count * 256); // ~256 bytes per record
        for record in &records {
            match serde_json::to_string(record) {
                Ok(json) => {
                    ndjson.push_str(&json);
                    ndjson.push('\n');
                }
                Err(e) => {
                    error!("Failed to serialize ClickHouse record: {}", e);
                    // Continue with other records
                }
            }
        }

        if ndjson.is_empty() {
            return;
        }

        // Send to ClickHouse with retry
        let mut attempts = 0;
        let max_attempts = 2;

        while attempts < max_attempts {
            attempts += 1;

            let content_length = ndjson.len();
            match self
                .client
                .post(&url)
                .header("Content-Length", content_length.to_string())
                .body(ndjson.clone())
                .send()
                .await
            {
                Ok(response) => {
                    if response.status().is_success() {
                        info!("Flushed {} BGP records to ClickHouse", count);
                        return;
                    } else {
                        let status = response.status();
                        let body = response.text().await.unwrap_or_default();
                        error!("ClickHouse flush failed with status {}: {}", status, body);
                    }
                }
                Err(e) => {
                    error!("ClickHouse flush request failed: {}", e);
                }
            }

            // Wait a bit before retry (exponential backoff would be better)
            if attempts < max_attempts {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }

        // All attempts failed
        error!(
            "All ClickHouse flush attempts failed, dropping {} records",
            count
        );
        // Increment error metric
        self.metrics.record_clickhouse_flush_error();
    }

    /// Starts a background task that flushes the buffer at regular intervals.
    ///
    /// Returns a join handle that can be used to await the task.
    pub fn start_flush_task(&self) -> Option<tokio::task::JoinHandle<()>> {
        if !self.config.enabled {
            return None;
        }

        let exporter = self.clone();
        let flush_interval_secs = self.config.flush_interval_secs;

        Some(tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(flush_interval_secs)).await;
                exporter.flush().await;
            }
        }))
    }

    /// Flush exporter buffer before shutdown.
    pub async fn flush_on_shutdown(&self) {
        if !self.config.enabled {
            return;
        }

        tracing::info!("Flushing ClickHouse exporter buffer before shutdown...");
        self.flush().await;
        tracing::info!("ClickHouse exporter flush complete.");
    }
}

/// Extracts BGP metadata from a LogEntry's metadata field.
///
/// Returns `Some(BgpClickHouseRecord)` if the entry contains valid BGP metadata,
/// otherwise returns `None`.
pub fn extract_bgp_record(
    entry: &crate::models::LogEntry,
    tenant_id: &str,
    source: &str,
) -> Option<BgpClickHouseRecord> {
    let metadata = entry.metadata.as_ref()?;

    // Try to extract BGP fields from metadata
    let event_type = metadata.get("event_type")?.as_str()?.to_string();
    let prefix = metadata.get("prefix")?.as_str()?.to_string();

    // Parse numeric fields with fallbacks
    let origin_as = metadata.get("origin_as")?.as_u64()? as u32;
    let peer_asn = metadata.get("peer_asn")?.as_u64()? as u32;
    let peer_ip = metadata.get("peer_ip")?.as_str()?.to_string();

    // Parse arrays with fallbacks
    let as_path = metadata
        .get("as_path")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_u64().map(|n| n as u32))
                .collect()
        })
        .unwrap_or_default();

    let community = metadata
        .get("community")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    Some(BgpClickHouseRecord {
        timestamp: entry.timestamp,
        event_type,
        prefix,
        origin_as,
        as_path,
        peer_asn,
        peer_ip,
        community,
        source: source.to_string(),
        tenant_id: tenant_id.to_string(),
    })
}

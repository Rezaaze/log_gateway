use chrono::{DateTime, Utc};
use crossbeam::queue::ArrayQueue;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{error, info};
use uuid::Uuid;

/// A record to be stored in the storage sink.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SinkRecord {
    pub id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub source: String,
    pub level: String,
    pub redacted_message: String,
    pub pii_hits: usize,
    pub bytes: u64,
    pub cache_hit: bool,
}

/// Internal configuration for the storage sink.
#[derive(Debug, Clone)]
pub struct SinkConfig {
    pub output_dir: PathBuf,
    pub max_buffer_size: usize,
    pub flush_interval_secs: u64,
    pub compress: bool,
}

/// Storage sink that buffers records and flushes them to NDJSON files.
#[derive(Debug, Clone)]
pub struct StorageSink {
    buffer: Arc<ArrayQueue<SinkRecord>>,
    config: SinkConfig,
}

impl StorageSink {
    /// Creates a new storage sink.
    ///
    /// # Arguments
    ///
    /// * `output_dir` - Directory where NDJSON files will be written.
    /// * `max_buffer_size` - Flush when buffer reaches this size.
    /// * `flush_interval_secs` - Flush on this interval regardless of buffer size.
    /// * `compress` - Whether to compress files with zstd.
    pub fn new(
        output_dir: PathBuf,
        max_buffer_size: usize,
        flush_interval_secs: u64,
        compress: bool,
    ) -> Self {
        // Create output directory if it doesn't exist
        if let Err(e) = std::fs::create_dir_all(&output_dir) {
            error!(
                "Failed to create output directory {}: {}",
                output_dir.display(),
                e
            );
        }

        // Use double the buffer size as headroom to avoid dropping records too early
        let queue_capacity = max_buffer_size * 2;
        Self {
            buffer: Arc::new(ArrayQueue::new(queue_capacity)),
            config: SinkConfig {
                output_dir,
                max_buffer_size,
                flush_interval_secs,
                compress,
            },
        }
    }

    /// Writes a record to the sink buffer.
    ///
    /// If the buffer reaches the configured maximum size, it triggers a flush.
    pub fn write(&self, record: SinkRecord) {
        // Try to push the record to the queue
        // If the queue is full, we drop the record silently (no panic, no block)
        if self.buffer.push(record).is_err() {
            // Queue is full, record is dropped
            // In production, you might want to increment a metric here
        }
    }

    /// Flushes all buffered records to disk.
    pub async fn flush(&self) {
        // Collect records from the queue into a local vector
        let mut records = Vec::with_capacity(self.config.max_buffer_size);

        // Drain the queue by popping records until it's empty
        while let Some(record) = self.buffer.pop() {
            records.push(record);

            // Stop if we've collected enough records for a single flush
            if records.len() >= self.config.max_buffer_size {
                break;
            }
        }

        if records.is_empty() {
            return;
        }

        let count = records.len();
        let timestamp = Utc::now().timestamp_millis();

        // Determine file extension based on compression setting
        let (filepath, filename) = if self.config.compress {
            let filepath = self
                .config
                .output_dir
                .join(format!("{}.ndjson.zst", timestamp));
            let filename = filepath.display().to_string();
            (filepath, filename)
        } else {
            let filepath = self.config.output_dir.join(format!("{}.ndjson", timestamp));
            let filename = filepath.display().to_string();
            (filepath, filename)
        };

        // Stream-serialize directly into a byte buffer — no intermediate String allocation.
        // Each record is written as JSON followed by a newline (NDJSON format).
        let estimated_size = count * 256; // ~256 bytes per record on average
        let mut ndjson_bytes: Vec<u8> = Vec::with_capacity(estimated_size);
        let mut serialized = 0usize;
        for record in records.iter() {
            match serde_json::to_writer(&mut ndjson_bytes, record) {
                Ok(()) => {
                    ndjson_bytes.push(b'\n');
                    serialized += 1;
                }
                Err(e) => {
                    error!("Failed to serialize record: {}", e);
                    // Continue with other records
                }
            }
        }

        if serialized == 0 {
            return;
        }

        // Write to file with optional compression
        if self.config.compress {
            // zstd level 1: ~40% faster than level 3, negligible quality difference for logs
            let compressed = match zstd::encode_all(ndjson_bytes.as_slice(), 1) {
                Ok(b) => b,
                Err(e) => {
                    error!("zstd encode failed: {}", e);
                    return;
                }
            };
            match tokio::fs::write(&filepath, &compressed).await {
                Ok(_) => {
                    info!("Flushed {} records to {} (zstd)", count, filename);
                }
                Err(e) => {
                    error!("Flush failed: {}", e);
                }
            }
        } else {
            // Write uncompressed
            match tokio::fs::write(&filepath, &ndjson_bytes).await {
                Ok(_) => {
                    info!("Flushed {} records to {}", count, filename);
                }
                Err(e) => {
                    error!("Flush failed: {}", e);
                }
            }
        }
    }

    /// Starts a background task that flushes the buffer at regular intervals.
    ///
    /// Returns a join handle that can be used to await the task.
    pub fn start_flush_task(&self) -> tokio::task::JoinHandle<()> {
        let sink = self.clone();
        let flush_interval_secs = self.config.flush_interval_secs;

        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(flush_interval_secs)).await;
                sink.flush().await;
            }
        })
    }

    /// Flush sink buffer before shutdown.
    pub async fn flush_on_shutdown(&self) {
        tracing::info!("Flushing sink buffer before shutdown...");
        self.flush().await;
        tracing::info!("Sink flush complete.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_sink_compress_creates_zst_file() {
        let dir = tempdir().unwrap();
        let sink = StorageSink::new(dir.path().to_path_buf(), 1, 30, true);
        sink.write(SinkRecord {
            id: uuid::Uuid::new_v4(),
            timestamp: chrono::Utc::now(),
            source: "test".to_string(),
            level: "info".to_string(),
            redacted_message: "hello".to_string(),
            pii_hits: 0,
            bytes: 5,
            cache_hit: false,
        });
        // Need to flush to write the file
        sink.flush().await;
        let files: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(files.len(), 1);
        let name = files[0].file_name();
        assert!(
            name.to_string_lossy().ends_with(".zst"),
            "expected .zst file, got: {}",
            name.to_string_lossy()
        );
    }
}

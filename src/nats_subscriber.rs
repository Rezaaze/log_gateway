/*!
NATS Subscriber for BGP Events

This module provides NATS event subscription with:
- Connection to NATS Jetstream
- BGP event parsing
- Forwarding to detector channel
*/

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tracing::{error, info, warn};
use uuid::Uuid;

/// BGP Event from NATS stream (mirrors bgp-stream publishing format)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BgpEvent {
    pub id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub level: String,
    pub source: String,
    pub message: String,
    pub metadata: Option<serde_json::Value>,
}

/// Extracted BGP record from BgpEvent metadata for detector processing
#[derive(Debug, Clone)]
pub struct BgpRecord {
    pub prefix: String,
    pub origin_as: u32,
    pub peer_asn: u32,
    pub event_type: String, // "announce" or "withdraw"
    pub as_path: Vec<u32>,
    pub timestamp: DateTime<Utc>,
}

/// Configuration for NATS subscriber
#[derive(Clone, Debug)]
pub struct SubscriberConfig {
    pub nats_url: String,
    pub subject: String,
}

impl Default for SubscriberConfig {
    fn default() -> Self {
        Self {
            nats_url: "nats://localhost:4222".to_string(),
            subject: "bgp.events".to_string(),
        }
    }
}

/// Parse a BgpEvent and extract BGP record
///
/// Extracts fields from the metadata JSON object:
/// - prefix: CIDR notation string
/// - origin_as: u32 origin ASN
/// - peer_asn: u32 peer ASN
/// - event_type: "announce" or "withdraw"
/// - as_path: array of u32 ASNs
pub fn extract_bgp_record(event: &BgpEvent) -> Option<BgpRecord> {
    let metadata = event.metadata.as_ref()?;

    let prefix = metadata.get("prefix")?.as_str()?.to_string();
    let origin_as = metadata.get("origin_as")?.as_u64()? as u32;
    let peer_asn = metadata.get("peer_asn")?.as_u64()? as u32;
    let event_type = metadata.get("event_type")?.as_str()?.to_string();

    let as_path = metadata
        .get("as_path")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|n| n.as_u64().map(|u| u as u32))
                .collect()
        })
        .unwrap_or_default();

    Some(BgpRecord {
        prefix,
        origin_as,
        peer_asn,
        event_type,
        as_path,
        timestamp: event.timestamp,
    })
}

/// Subscribe to BGP events from NATS
///
/// Connects to NATS, subscribes to bgp.events subject, and forwards
/// deserialized BGP records to the detector channel.
///
/// This function runs indefinitely with automatic reconnection.
/// Returns Ok() only if detector channel closes.
pub async fn subscribe_bgp_events(
    config: SubscriberConfig,
    tx: mpsc::Sender<BgpRecord>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut retry_count = 0;

    loop {
        info!(
            "Connecting to NATS at {} (attempt {})",
            config.nats_url,
            retry_count + 1
        );

        // Connect to NATS
        let client = match async_nats::connect(&config.nats_url).await {
            Ok(c) => {
                info!("Connected to NATS successfully");
                retry_count = 0; // Reset on successful connection
                c
            }
            Err(e) => {
                error!("Failed to connect to NATS: {}", e);
                retry_count += 1;
                let delay = std::cmp::min(30, 5 * retry_count); // Exponential backoff, max 30s
                warn!("Retrying in {} seconds...", delay);
                tokio::time::sleep(Duration::from_secs(delay)).await;
                continue;
            }
        };

        // Subscribe to subject
        let mut subscriber = match client.subscribe(config.subject.clone()).await {
            Ok(s) => {
                info!("Subscribed to subject: {}", config.subject);
                s
            }
            Err(e) => {
                error!("Failed to subscribe to {}: {}", config.subject, e);
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };

        let mut message_count: u64 = 0;
        let mut error_count: u64 = 0;
        let last_stats_time = std::time::Instant::now();

        // Consume messages until connection fails
        while let Some(message) = subscriber.next().await {
            // Try to deserialize as BgpEvent
            match serde_json::from_slice::<BgpEvent>(&message.payload) {
                Ok(event) => {
                    // Extract BGP record
                    if let Some(record) = extract_bgp_record(&event) {
                        // Send to detector (non-blocking)
                        match tx.try_send(record) {
                            Ok(_) => {
                                message_count += 1;
                            }
                            Err(mpsc::error::TrySendError::Full(_)) => {
                                error_count += 1;
                                // Detector channel full - drop event (detector too slow)
                                if error_count.is_multiple_of(1000) {
                                    warn!("Detector channel full, dropped {} events", error_count);
                                }
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => {
                                info!("Detector channel closed, stopping subscription");
                                return Ok(());
                            }
                        }
                    }

                    // Log stats every 10k messages
                    if message_count.is_multiple_of(10000) {
                        let elapsed = last_stats_time.elapsed().as_secs_f64();
                        let rate = message_count as f64 / elapsed;
                        info!(
                            "NATS subscription: {} events (rate: {:.0}/sec), errors: {}",
                            message_count, rate, error_count
                        );
                    }
                }
                Err(e) => {
                    error!("Failed to deserialize BgpEvent from NATS: {}", e);
                    // Continue processing other messages
                }
            }
        }

        warn!(
            "NATS subscription ended (processed {} events), reconnecting in 5s",
            message_count
        );
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_bgp_record() {
        let event = BgpEvent {
            id: Uuid::new_v4(),
            timestamp: Utc::now(),
            level: "info".to_string(),
            source: "ripe-ris".to_string(),
            message: "ANNOUNCE 192.0.2.0/24 via AS64512".to_string(),
            metadata: Some(serde_json::json!({
                "prefix": "192.0.2.0/24",
                "origin_as": 64512,
                "peer_asn": 64513,
                "event_type": "announce",
                "as_path": [64513, 64512]
            })),
        };

        let record = extract_bgp_record(&event).unwrap();
        assert_eq!(record.prefix, "192.0.2.0/24");
        assert_eq!(record.origin_as, 64512);
        assert_eq!(record.peer_asn, 64513);
        assert_eq!(record.event_type, "announce");
        assert_eq!(record.as_path, vec![64513, 64512]);
    }

    #[test]
    fn test_extract_bgp_record_withdraw() {
        let event = BgpEvent {
            id: Uuid::new_v4(),
            timestamp: Utc::now(),
            level: "warn".to_string(),
            source: "ripe-ris".to_string(),
            message: "WITHDRAW 192.0.2.0/24".to_string(),
            metadata: Some(serde_json::json!({
                "prefix": "192.0.2.0/24",
                "origin_as": 64512,
                "peer_asn": 64513,
                "event_type": "withdraw",
                "as_path": [64513, 64512]
            })),
        };

        let record = extract_bgp_record(&event).unwrap();
        assert_eq!(record.event_type, "withdraw");
    }

    #[test]
    fn test_extract_bgp_record_empty_metadata() {
        let event = BgpEvent {
            id: Uuid::new_v4(),
            timestamp: Utc::now(),
            level: "info".to_string(),
            source: "ripe-ris".to_string(),
            message: "Message".to_string(),
            metadata: None,
        };

        assert!(extract_bgp_record(&event).is_none());
    }

    #[test]
    fn test_extract_bgp_record_missing_fields() {
        let event = BgpEvent {
            id: Uuid::new_v4(),
            timestamp: Utc::now(),
            level: "info".to_string(),
            source: "ripe-ris".to_string(),
            message: "Message".to_string(),
            metadata: Some(serde_json::json!({
                "prefix": "192.0.2.0/24",
                // Missing origin_as and other fields
            })),
        };

        assert!(extract_bgp_record(&event).is_none());
    }
}

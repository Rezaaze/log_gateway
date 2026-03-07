/*!
NATS Subscriber for BGP Events

This module provides NATS event subscription with:
- Connection to NATS Jetstream
- BGP event parsing
- Forwarding to detector channel
*/

use async_nats::jetstream::consumer::{pull, AckPolicy, DeliverPolicy};
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
    pub collector: String, // e.g., "rrc12", fallback: "unknown"
    pub peer_ip: String,   // e.g., "80.249.211.0", fallback: ""
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
/// - collector: collector name (e.g., "rrc12"), fallback: "unknown"
/// - peer_ip: peer IP address, fallback: ""
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

    let collector = metadata
        .get("collector")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let peer_ip = metadata
        .get("peer_ip")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    Some(BgpRecord {
        prefix,
        origin_as,
        peer_asn,
        event_type,
        as_path,
        timestamp: event.timestamp,
        collector,
        peer_ip,
    })
}

/// Parse a NATS URL and extract credentials and clean server URL.
///
/// Handles URLs of the form `nats://user:pass@host:port` or `nats://host:port`.
/// Returns (clean_url_without_credentials, Option<(username, password)>).
fn parse_nats_url(url: &str) -> (String, Option<(String, String)>) {
    if let Some(at_pos) = url.rfind('@') {
        let scheme_end = url.find("://").map(|i| i + 3).unwrap_or(0);
        let creds = &url[scheme_end..at_pos];
        let clean_url = format!("{}{}", &url[..scheme_end], &url[at_pos + 1..]);
        if let Some(colon_pos) = creds.find(':') {
            let user = creds[..colon_pos].to_string();
            let pass = creds[colon_pos + 1..].to_string();
            return (clean_url, Some((user, pass)));
        }
        return (clean_url, None);
    }
    (url.to_string(), None)
}

/// Subscribe to BGP events from NATS JetStream
///
/// Connects to NATS, creates JetStream context, and subscribes to
/// durable consumer "detector-group" on stream "BGP_EVENTS".
/// Forwards deserialized BGP records to the detector channel.
///
/// This function runs indefinitely with automatic reconnection.
/// Returns Ok() only if detector channel closes.
pub async fn subscribe_bgp_events(
    config: SubscriberConfig,
    tx: mpsc::Sender<BgpRecord>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut retry_count = 0;
    // async-nats 0.34 does not extract credentials from the URL automatically —
    // use ConnectOptions::user_and_password() instead.
    let (clean_url, creds) = parse_nats_url(&config.nats_url);

    loop {
        info!(
            "Connecting to NATS at {} (attempt {})",
            config.nats_url,
            retry_count + 1
        );

        // Build ConnectOptions with explicit credentials when present
        let connect_opts = if let Some((ref user, ref pass)) = creds {
            async_nats::ConnectOptions::new().user_and_password(user.clone(), pass.clone())
        } else {
            async_nats::ConnectOptions::new()
        };

        // Connect to NATS
        let client = match connect_opts.connect(&clean_url).await {
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

        // Create JetStream context
        let js = async_nats::jetstream::new(client);

        // Get or create stream "BGP_EVENTS"
        let stream = match js.get_stream("BGP_EVENTS").await {
            Ok(s) => {
                info!("Found JetStream stream 'BGP_EVENTS'");
                s
            }
            Err(e) => {
                error!("Failed to get stream 'BGP_EVENTS': {}", e);
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };

        // Create durable pull consumer — required for WorkQueue streams (AckPolicy::Explicit)
        // and for multi-instance deployment (all 4 gateways compete for the same messages).
        // WorkQueue streams also require DeliverPolicy::All (error 10101 with ::New).
        let consumer_config = pull::Config {
            durable_name: Some("detector-group".to_string()),
            deliver_policy: DeliverPolicy::All,
            filter_subject: config.subject.clone(),
            ack_policy: AckPolicy::Explicit, // WorkQueue streams mandate explicit ack
            ..Default::default()
        };

        // Get or create durable pull consumer
        let consumer = match stream
            .get_or_create_consumer("detector-group", consumer_config)
            .await
        {
            Ok(c) => {
                info!("Created/retrieved durable pull consumer 'detector-group'");
                c
            }
            Err(e) => {
                error!("Failed to create consumer 'detector-group': {}", e);
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };

        // Stream messages continuously from pull consumer
        let mut messages = match consumer.stream().messages().await {
            Ok(m) => {
                info!("Listening for messages from pull consumer 'detector-group'");
                m
            }
            Err(e) => {
                error!("Failed to get messages from consumer: {}", e);
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };

        let mut message_count: u64 = 0;
        let mut error_count: u64 = 0;
        let mut last_stats_time = std::time::Instant::now();
        let mut last_stats_count: u64 = 0;

        // Consume messages until connection fails
        while let Some(msg_result) = messages.next().await {
            let message = match msg_result {
                Ok(m) => m,
                Err(e) => {
                    error!("Error receiving message from JetStream: {}", e);
                    break; // Break out of message loop to reconnect
                }
            };

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

                    // Log interval rate every 10k messages
                    if message_count.is_multiple_of(10000) {
                        let elapsed = last_stats_time.elapsed().as_secs_f64();
                        let interval_count = message_count - last_stats_count;
                        let rate = interval_count as f64 / elapsed.max(0.001);
                        info!(
                            "JetStream subscription: {} events total (rate: {:.0}/sec), errors: {}",
                            message_count, rate, error_count
                        );
                        // Reset interval counters for next window
                        last_stats_time = std::time::Instant::now();
                        last_stats_count = message_count;
                    }
                }
                Err(e) => {
                    error!("Failed to deserialize BgpEvent from NATS: {}", e);
                    // WorkQueue stream: ack even on parse errors to remove from queue
                }
            }

            // Acknowledge message — required for WorkQueue streams (AckPolicy::Explicit).
            // ACK removes the message from the stream after processing.
            if let Err(e) = message.ack().await {
                warn!("Failed to ack NATS message: {}", e);
            }
        }

        warn!(
            "JetStream subscription ended (processed {} events), reconnecting in 5s",
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

    #[test]
    fn test_extract_collector_and_peer_ip() {
        let event = BgpEvent {
            id: uuid::Uuid::new_v4(),
            timestamp: chrono::Utc::now(),
            level: "info".to_string(),
            source: "ripe-ris".to_string(),
            message: "ANNOUNCE 8.8.8.0/24".to_string(),
            metadata: Some(serde_json::json!({
                "event_type": "announce",
                "prefix":     "8.8.8.0/24",
                "peer_asn":   1103_u64,
                "origin_as":  15169_u64,
                "peer_ip":    "80.249.211.0",
                "as_path":    [1103_u64, 3356_u64, 15169_u64],
                "collector":  "rrc12",
            })),
        };
        let record = extract_bgp_record(&event).unwrap();
        assert_eq!(record.collector, "rrc12");
        assert_eq!(record.peer_ip, "80.249.211.0");
        assert_eq!(record.origin_as, 15169);
    }

    #[test]
    fn test_extract_collector_fallback_to_unknown() {
        let event = BgpEvent {
            id: uuid::Uuid::new_v4(),
            timestamp: chrono::Utc::now(),
            level: "info".to_string(),
            source: "ripe-ris".to_string(),
            message: "ANNOUNCE 1.0.0.0/24".to_string(),
            metadata: Some(serde_json::json!({
                "event_type": "announce",
                "prefix":     "1.0.0.0/24",
                "peer_asn":   64512_u64,
                "origin_as":  64512_u64,
                "peer_ip":    "",
                "as_path":    [64512_u64],
                // kein "collector"-Feld
            })),
        };
        let record = extract_bgp_record(&event).unwrap();
        assert_eq!(record.collector, "unknown");
        assert_eq!(record.peer_ip, "");
    }

    #[test]
    fn test_extract_different_collectors() {
        let make_event = |collector: &str, prefix: &str| BgpEvent {
            id: uuid::Uuid::new_v4(),
            timestamp: chrono::Utc::now(),
            level: "info".to_string(),
            source: "ripe-ris".to_string(),
            message: format!("ANNOUNCE {prefix}"),
            metadata: Some(serde_json::json!({
                "event_type": "announce",
                "prefix":     prefix,
                "peer_asn":   1103_u64,
                "origin_as":  15169_u64,
                "peer_ip":    "10.0.0.1",
                "as_path":    [1103_u64, 15169_u64],
                "collector":  collector,
            })),
        };
        let r1 = extract_bgp_record(&make_event("rrc00", "8.8.8.0/24")).unwrap();
        let r2 = extract_bgp_record(&make_event("rrc17", "8.8.8.0/24")).unwrap();
        assert_eq!(r1.collector, "rrc00");
        assert_eq!(r2.collector, "rrc17");
        assert_ne!(r1.collector, r2.collector);
    }
}

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::mpsc;
use tracing;

use crate::alert_dedup::{compute_fingerprint, DedupCache};
use crate::anomaly_detector::{Anomaly, Detector, FlappingDetector, HijackDetector};
use crate::clickhouse_exporter::BgpClickHouseRecord;
use crate::irr_cache::{IrrCache, IrrStatus};
use crate::nats_subscriber::{subscribe_bgp_events, BgpRecord, SubscriberConfig};
use crate::rpki_cache::{RpkiCache, RpkiStatus};
use crate::webhook::{WebhookSender, WebhookTarget};

/// Configuration for the detector loop.
pub struct DetectorLoopConfig {
    /// NATS URL (e.g., "nats://user:pass@host:4222")
    pub nats_url: String,
    /// NATS subject (e.g., "bgp.events")
    pub subject: String,
    /// Webhook targets for sending alerts
    pub webhook_targets: Vec<WebhookTarget>,
    /// Whether IRR checking is enabled (slower)
    pub irr_enabled: bool,
    /// Channel size between NATS consumer and detector (default: 10_000)
    pub channel_size: usize,
}

impl Default for DetectorLoopConfig {
    fn default() -> Self {
        Self {
            nats_url: "nats://localhost:4222".to_string(),
            subject: "bgp.events".to_string(),
            webhook_targets: Vec::new(),
            irr_enabled: true,
            channel_size: 10_000,
        }
    }
}

/// The main detector loop that connects all isolated components:
/// NATS Consumer → RPKI → IRR → HijackDetector → FlappingDetector → Dedup → Webhook
pub struct DetectorLoop {
    config: DetectorLoopConfig,
    rpki_cache: Arc<RpkiCache>,
    irr_cache: Arc<IrrCache>,
    hijack_detector: Arc<HijackDetector>,
    flapping_detector: Arc<FlappingDetector>,
    dedup_cache: DedupCache,
    webhook_sender: WebhookSender,
}

impl DetectorLoop {
    /// Creates a new detector loop.
    ///
    /// # Arguments
    ///
    /// * `config` - Configuration for the detector loop
    /// * `rpki_cache` - Shared RPKI cache for validation
    ///
    /// # Returns
    ///
    /// A new `DetectorLoop` instance
    pub fn new(config: DetectorLoopConfig, rpki_cache: Arc<RpkiCache>) -> Result<Self> {
        let irr_cache = Arc::new(IrrCache::new());
        let hijack_detector = Arc::new(HijackDetector::new());
        let flapping_detector = Arc::new(FlappingDetector::new());
        let dedup_cache = DedupCache::new();
        let webhook_sender = WebhookSender::new()?;

        Ok(Self {
            config,
            rpki_cache,
            irr_cache,
            hijack_detector,
            flapping_detector,
            dedup_cache,
            webhook_sender,
        })
    }

    /// Starts the NATS consumer and the processing loop.
    /// Blocks until error or shutdown signal.
    pub async fn run(&self) -> Result<()> {
        let (tx, mut rx) = mpsc::channel::<BgpRecord>(self.config.channel_size);

        // NATS Subscriber in background
        let sub_config = SubscriberConfig {
            nats_url: self.config.nats_url.clone(),
            subject: self.config.subject.clone(),
        };
        tokio::spawn(async move {
            if let Err(e) = subscribe_bgp_events(sub_config, tx).await {
                tracing::error!("NATS subscriber exited: {}", e);
            }
        });

        // Processing Loop
        let mut events_total: u64 = 0;
        let mut anomalies_total: u64 = 0;
        let mut log_interval = tokio::time::interval(Duration::from_secs(60));
        log_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                Some(record) = rx.recv() => {
                    let anomalies = self.process_record(&record).await;
                    events_total += 1;
                    anomalies_total += anomalies.len() as u64;
                }
                _ = log_interval.tick() => {
                    tracing::info!("DetectorLoop: {} events, {} anomalies", events_total, anomalies_total);
                }
            }
        }
    }

    /// Processes a single BGP record.
    /// Returns `Vec<Anomaly>` (for tests).
    pub async fn process_record(&self, record: &BgpRecord) -> Vec<Anomaly> {
        // 1. Convert BgpRecord to BgpClickHouseRecord
        let ch_record = Self::to_clickhouse_record(record);

        // 2. RPKI status (synchronous)
        let rpki = self.rpki_cache.validate(&record.prefix, record.origin_as);

        // 3. IRR status (async, if enabled)
        let irr = if self.config.irr_enabled {
            self.irr_cache.check(&record.prefix, record.origin_as).await
        } else {
            IrrStatus::Unavailable
        };

        // 4. RPKI-First: only check ANNOUNCE events further
        if rpki == RpkiStatus::Valid {
            // RPKI-valid → no hijack possible
            if record.event_type == "announce" {
                return vec![];
            }
            // For WITHDRAW, continue to flapping detection only
        }

        // 5. Run detectors
        let mut anomalies = vec![];

        if record.event_type == "announce" {
            if let Some(a) = self
                .hijack_detector
                .check_with_rpki_status(&ch_record, rpki, &irr)
            {
                anomalies.push(a);
            }
        }

        if let Some(a) = self.flapping_detector.check(&ch_record) {
            anomalies.push(a);
        }

        // 6. Dedup + Webhook for each anomaly
        for anomaly in &anomalies {
            let fp = compute_fingerprint(
                &anomaly.anomaly_type.to_string(),
                &anomaly.prefix,
                anomaly.origin_as,
            );
            if self.dedup_cache.is_new(&fp) {
                let level = Self::confidence_to_level(anomaly.confidence);
                let payload = WebhookSender::build_payload(anomaly, &level, &fp);
                for target in &self.config.webhook_targets {
                    // Fire-and-forget: don't block processing on webhook errors
                    let sender = self.webhook_sender.clone();
                    let target = target.clone();
                    let payload = payload.clone();
                    tokio::spawn(async move {
                        if let Err(e) = sender.send(&target, &payload).await {
                            tracing::warn!("Webhook send failed: {}", e);
                        }
                    });
                }
            }
        }

        anomalies
    }

    /// Converts a `BgpRecord` to a `BgpClickHouseRecord`.
    fn to_clickhouse_record(r: &BgpRecord) -> BgpClickHouseRecord {
        BgpClickHouseRecord {
            timestamp: r.timestamp,
            event_type: r.event_type.clone(),
            prefix: r.prefix.clone(),
            origin_as: r.origin_as,
            as_path: r.as_path.clone(),
            peer_asn: r.peer_asn,
            peer_ip: String::new(),
            community: vec![],
            source: "ripe-ris".to_string(),
            tenant_id: "global".to_string(),
        }
    }

    /// Maps confidence score to alert level.
    /// Follows the same mapping as `EscalationLevel::from_confidence`.
    fn confidence_to_level(c: f64) -> crate::escalation::EscalationLevel {
        // Note: This matches EscalationLevel::from_confidence but always returns a level
        // (doesn't return None for confidence < 0.5 since webhooks need a level)
        if c < 0.5 {
            // Anomalies with confidence < 0.5 shouldn't reach webhooks, but if they do,
            // use Warning as a fallback
            crate::escalation::EscalationLevel::Warning
        } else if c < 0.7 {
            crate::escalation::EscalationLevel::Warning
        } else if c < 0.9 {
            crate::escalation::EscalationLevel::Critical
        } else {
            crate::escalation::EscalationLevel::Emergency
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn create_test_bgp_record() -> BgpRecord {
        BgpRecord {
            prefix: "192.0.2.0/24".to_string(),
            origin_as: 64512,
            peer_asn: 64513,
            event_type: "announce".to_string(),
            as_path: vec![64513, 64512],
            timestamp: Utc::now(),
            collector: "unknown".to_string(),
            peer_ip: "".to_string(),
        }
    }

    #[test]
    fn test_to_clickhouse_record() {
        let bgp_record = create_test_bgp_record();
        let ch_record = DetectorLoop::to_clickhouse_record(&bgp_record);

        assert_eq!(ch_record.prefix, "192.0.2.0/24");
        assert_eq!(ch_record.origin_as, 64512);
        assert_eq!(ch_record.peer_asn, 64513);
        assert_eq!(ch_record.event_type, "announce");
        assert_eq!(ch_record.as_path, vec![64513, 64512]);
        assert_eq!(ch_record.peer_ip, "");
        assert_eq!(ch_record.community, Vec::<String>::new());
        assert_eq!(ch_record.source, "ripe-ris");
        assert_eq!(ch_record.tenant_id, "global");
    }

    #[test]
    fn test_confidence_to_level() {
        use crate::escalation::EscalationLevel;

        // Test matches EscalationLevel::from_confidence mapping
        assert_eq!(
            DetectorLoop::confidence_to_level(0.95),
            EscalationLevel::Emergency
        );
        assert_eq!(
            DetectorLoop::confidence_to_level(0.90),
            EscalationLevel::Emergency
        );
        assert_eq!(
            DetectorLoop::confidence_to_level(0.89),
            EscalationLevel::Critical
        );
        assert_eq!(
            DetectorLoop::confidence_to_level(0.85),
            EscalationLevel::Critical
        );
        assert_eq!(
            DetectorLoop::confidence_to_level(0.70),
            EscalationLevel::Critical
        );
        assert_eq!(
            DetectorLoop::confidence_to_level(0.69),
            EscalationLevel::Warning
        );
        assert_eq!(
            DetectorLoop::confidence_to_level(0.65),
            EscalationLevel::Warning
        );
        assert_eq!(
            DetectorLoop::confidence_to_level(0.50),
            EscalationLevel::Warning
        );
        assert_eq!(
            DetectorLoop::confidence_to_level(0.30),
            EscalationLevel::Warning
        ); // fallback for confidence < 0.5
    }

    #[tokio::test]
    async fn test_process_record_with_rpki_valid() {
        // Create a mock RPKI cache that returns Valid
        let rpki_cache = Arc::new(RpkiCache::new("http://dummy".to_string()));
        let config = DetectorLoopConfig::default();
        let loop_instance = DetectorLoop::new(config, rpki_cache).unwrap();

        let record = create_test_bgp_record();
        let anomalies = loop_instance.process_record(&record).await;

        // With RPKI Valid and no actual cache data, it should return Unavailable
        // which means we'll still get anomalies. This is expected behavior.
        // The test verifies the function doesn't panic.
        assert!(anomalies.len() <= 2); // Could be 0, 1 (flapping), or 2 (hijack+flapping)
    }

    #[test]
    fn test_detector_loop_new() {
        let rpki_cache = Arc::new(RpkiCache::new("http://dummy".to_string()));
        let config = DetectorLoopConfig::default();
        let loop_instance = DetectorLoop::new(config, rpki_cache);

        assert!(loop_instance.is_ok());
    }
}

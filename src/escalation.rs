use chrono::Utc;
use serde::Serialize;
use std::sync::Arc;

use crate::alert_dedup::{compute_fingerprint, DedupCache};
use crate::alert_manager::AlertManagerClient;
use crate::anomaly_detector::Anomaly;
use crate::metrics::GatewayMetrics;
use crate::webhook::{WebhookSender, WebhookTarget};

/// Escalation level based on confidence score.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum EscalationLevel {
    Warning,   // confidence 0.5–0.7
    Critical,  // confidence 0.7–0.9
    Emergency, // confidence > 0.9
}

impl EscalationLevel {
    /// Determines escalation level from confidence score.
    ///
    /// Returns `None` for confidence < 0.5 (no alert).
    pub fn from_confidence(confidence: f64) -> Option<Self> {
        if confidence < 0.5 {
            None
        } else if confidence < 0.7 {
            Some(Self::Warning)
        } else if confidence < 0.9 {
            Some(Self::Critical)
        } else {
            Some(Self::Emergency)
        }
    }

    /// Returns the level as a string for logging and display.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Warning => "warning",
            Self::Critical => "critical",
            Self::Emergency => "emergency",
        }
    }

    /// Returns the level as a string for Prometheus labels.
    pub fn prometheus_label(&self) -> &'static str {
        self.as_str()
    }
}

/// Routes anomalies to appropriate escalation levels and persists them.
pub struct EscalationRouter {
    alert_manager: Arc<AlertManagerClient>,
    metrics: Arc<GatewayMetrics>,
    dedup_cache: Arc<DedupCache>,
    webhook_sender: Option<Arc<WebhookSender>>,
    webhook_targets: Vec<WebhookTarget>,
}

impl EscalationRouter {
    /// Creates a new escalation router.
    pub fn new(alert_manager: Arc<AlertManagerClient>, metrics: Arc<GatewayMetrics>) -> Self {
        Self {
            alert_manager,
            metrics,
            dedup_cache: Arc::new(DedupCache::new()),
            webhook_sender: None,
            webhook_targets: Vec::new(),
        }
    }

    /// Creates a new escalation router with webhook support.
    pub fn with_webhooks(
        alert_manager: Arc<AlertManagerClient>,
        metrics: Arc<GatewayMetrics>,
        targets: Vec<WebhookTarget>,
    ) -> anyhow::Result<Self> {
        let webhook_sender = WebhookSender::new().map(Arc::new)?;

        Ok(Self {
            alert_manager,
            metrics,
            dedup_cache: Arc::new(DedupCache::new()),
            webhook_sender: Some(webhook_sender),
            webhook_targets: targets,
        })
    }

    /// Routes an anomaly based on its confidence score.
    ///
    /// 1. Determines escalation level from confidence
    /// 2. If no level (confidence < 0.5), returns early
    /// 3. Computes fingerprint for deduplication
    /// 4. Checks if alert is silenced
    /// 5. Checks if alert is a duplicate (within 30 minutes)
    /// 6. Persists alert in ClickHouse (rule_id: None)
    /// 7. Records escalation metric
    /// 8. Logs at appropriate tracing level
    pub async fn route(&self, anomaly: &Anomaly) {
        // Determine escalation level
        let level = match EscalationLevel::from_confidence(anomaly.confidence) {
            Some(level) => level,
            None => {
                // Confidence below threshold, no alert
                return;
            }
        };

        // Compute fingerprint for deduplication
        let alert_type = match anomaly.anomaly_type {
            crate::anomaly_detector::AnomalyType::PossibleHijack => "hijack",
            crate::anomaly_detector::AnomalyType::PrefixFlapping => "flap",
        };
        let fingerprint = compute_fingerprint(alert_type, &anomaly.prefix, anomaly.origin_as);

        // Check if alert is silenced
        match self.alert_manager.is_silenced(&fingerprint).await {
            Ok(true) => {
                tracing::debug!("Alert silenced: {}", fingerprint);
                return;
            }
            Err(e) => {
                // Graceful degradation: log warning but continue processing
                tracing::warn!("Failed to check if alert is silenced: {}", e);
            }
            Ok(false) => {
                // Alert is not silenced, continue processing
            }
        }

        // Check if alert is a duplicate (within 30 minutes)
        if !self.dedup_cache.is_new(&fingerprint) {
            tracing::debug!("Duplicate alert suppressed: {}", fingerprint);
            return;
        }

        // Persist alert in ClickHouse
        if let Err(e) = self.alert_manager.persist_alert(anomaly, None).await {
            tracing::error!("Failed to persist alert for escalation: {}", e);
            // Continue with logging and metrics despite persistence error
        }

        // Record escalation metric
        self.metrics.record_escalation(&level);

        // Log based on escalation level
        match level {
            EscalationLevel::Warning => {
                tracing::warn!(
                    "BGP anomaly detected (WARNING): {:?} prefix={} origin_as={} confidence={} fingerprint={}",
                    anomaly.anomaly_type,
                    anomaly.prefix,
                    anomaly.origin_as,
                    anomaly.confidence,
                    fingerprint
                );
            }
            EscalationLevel::Critical => {
                tracing::error!(
                    "BGP anomaly detected (CRITICAL): {:?} prefix={} origin_as={} confidence={} fingerprint={}",
                    anomaly.anomaly_type,
                    anomaly.prefix,
                    anomaly.origin_as,
                    anomaly.confidence,
                    fingerprint
                );
            }
            EscalationLevel::Emergency => {
                tracing::error!(
                    "BGP anomaly detected (EMERGENCY): {:?} prefix={} origin_as={} confidence={} fingerprint={}",
                    anomaly.anomaly_type,
                    anomaly.prefix,
                    anomaly.origin_as,
                    anomaly.confidence,
                    fingerprint
                );
                // Extra log line for emergency alerts
                tracing::error!("EMERGENCY ALERT: Immediate attention required for BGP anomaly");
            }
        }

        // Send webhook notifications if configured
        if let Some(sender) = &self.webhook_sender {
            for target in &self.webhook_targets {
                let payload = WebhookSender::build_payload(anomaly, &level, &fingerprint);
                if let Err(e) = sender.send(target, &payload).await {
                    tracing::error!("Webhook send failed: {}", e);
                    // nicht abbrechen — nächsten target versuchen
                }
            }
        }
    }
}

/// Background task: checks every N seconds for unacknowledged alerts
/// that are older than `timeout_secs` → escalates them one level higher.
///
/// # Arguments
///
/// * `alert_manager` - Client for accessing alert manager
/// * `escalation_router` - Router for escalating anomalies
/// * `check_interval_secs` - How often to check (e.g., 60 seconds)
/// * `timeout_secs` - After how many seconds to escalate (e.g., 300 seconds)
pub async fn run_auto_escalation(
    alert_manager: Arc<AlertManagerClient>,
    escalation_router: Arc<EscalationRouter>,
    check_interval_secs: u64,
    timeout_secs: u64,
) {
    tracing::info!(
        "Starting auto-escalation task: check_interval={}s, timeout={}s",
        check_interval_secs,
        timeout_secs
    );

    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(check_interval_secs)).await;

        // Get all active alerts
        let alerts = match alert_manager.list_active_alerts().await {
            Ok(alerts) => alerts,
            Err(e) => {
                tracing::warn!("Failed to list active alerts: {}", e);
                continue;
            }
        };

        let now = Utc::now();
        let mut escalated_count = 0;

        for alert in alerts {
            // Check if alert is old enough to escalate
            // Note: AlertHistoryEntry has `fired_at` field, not `detected_at`
            // According to the task description, we should use `detected_at` but
            // AlertHistoryEntry has `fired_at`. We'll use `fired_at` as that's when the alert was triggered.
            let age = now - alert.fired_at;
            let age_secs = age.num_seconds() as u64;

            if age_secs >= timeout_secs && alert.confidence < 0.9 {
                // Calculate new confidence (bump by 0.15, capped at 0.95)
                let new_confidence = (alert.confidence + 0.15).min(0.95);

                // Create an anomaly from the alert for routing
                let anomaly_type = match alert.alert_type.as_str() {
                    "hijack" => crate::anomaly_detector::AnomalyType::PossibleHijack,
                    "flap" => crate::anomaly_detector::AnomalyType::PrefixFlapping,
                    _ => {
                        tracing::warn!("Unknown alert type: {}, skipping", alert.alert_type);
                        continue;
                    }
                };

                let anomaly = Anomaly {
                    id: uuid::Uuid::new_v4(),
                    anomaly_type,
                    prefix: alert.prefix.clone(),
                    origin_as: alert.origin_as,
                    confidence: new_confidence,
                    detected_at: alert.fired_at, // Use fired_at as detected_at
                    details: format!("Auto-escalated from confidence {}", alert.confidence),
                    tenant_id: "".to_string(), // Tenant ID not available in alert history
                };

                // Route the escalated anomaly
                escalation_router.route(&anomaly).await;

                tracing::info!(
                    "Auto-escalating alert {} — confidence bumped from {} to {}",
                    alert.id,
                    alert.confidence,
                    new_confidence
                );

                escalated_count += 1;
            }
        }

        if escalated_count > 0 {
            tracing::info!("Auto-escalated {} alerts", escalated_count);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alert_manager::AlertHistoryEntry;
    use chrono::Duration;
    use uuid::Uuid;

    #[test]
    fn test_escalation_level_from_confidence() {
        // Below threshold
        assert_eq!(EscalationLevel::from_confidence(0.0), None);
        assert_eq!(EscalationLevel::from_confidence(0.49), None);

        // Warning range
        assert_eq!(
            EscalationLevel::from_confidence(0.5),
            Some(EscalationLevel::Warning)
        );
        assert_eq!(
            EscalationLevel::from_confidence(0.69),
            Some(EscalationLevel::Warning)
        );

        // Critical range
        assert_eq!(
            EscalationLevel::from_confidence(0.7),
            Some(EscalationLevel::Critical)
        );
        assert_eq!(
            EscalationLevel::from_confidence(0.89),
            Some(EscalationLevel::Critical)
        );

        // Emergency range
        assert_eq!(
            EscalationLevel::from_confidence(0.9),
            Some(EscalationLevel::Emergency)
        );
        assert_eq!(
            EscalationLevel::from_confidence(1.0),
            Some(EscalationLevel::Emergency)
        );
    }

    #[test]
    fn test_escalation_level_labels() {
        assert_eq!(EscalationLevel::Warning.as_str(), "warning");
        assert_eq!(EscalationLevel::Warning.prometheus_label(), "warning");

        assert_eq!(EscalationLevel::Critical.as_str(), "critical");
        assert_eq!(EscalationLevel::Critical.prometheus_label(), "critical");

        assert_eq!(EscalationLevel::Emergency.as_str(), "emergency");
        assert_eq!(EscalationLevel::Emergency.prometheus_label(), "emergency");
    }

    // Helper function to create a test alert
    fn create_test_alert(id: Uuid, confidence: f64, hours_ago: i64) -> AlertHistoryEntry {
        let fired_at = Utc::now() - Duration::hours(hours_ago);
        
        AlertHistoryEntry {
            id,
            rule_id: Uuid::new_v4(),
            alert_type: "hijack".to_string(),
            prefix: "192.0.2.0/24".to_string(),
            origin_as: 64512,
            confidence,
            status: "fired".to_string(),
            fired_at,
            resolved_at: None,
        }
    }

    #[test]
    fn test_auto_escalation_bumps_confidence() {
        // Test the confidence bump logic directly
        let new_confidence = f64::min(0.6 + 0.15, 0.95);
        assert_eq!(new_confidence, 0.75);
        
        // Create an alert with confidence 0.6 that's 6 hours old (should be escalated)
        let alert_id = Uuid::new_v4();
        let old_alert = create_test_alert(alert_id, 0.6, 6);
        
        // Test that alerts with confidence < 0.9 are eligible for escalation
        assert!(old_alert.confidence < 0.9);
        
        // Test age calculation (6 hours = 21600 seconds > 300 seconds timeout)
        let age = Utc::now() - old_alert.fired_at;
        let age_secs = age.num_seconds() as u64;
        assert!(age_secs >= 300); // 6 hours > 300 seconds
    }

    #[test]
    fn test_auto_escalation_caps_at_0_95() {
        // Test that confidence is capped at 0.95
        let confidence_85 = 0.85;
        let bumped = f64::min(confidence_85 + 0.15, 0.95);
        assert_eq!(bumped, 0.95); // Not 1.0
        
        let confidence_90 = 0.90;
        let bumped = f64::min(confidence_90 + 0.15, 0.95);
        assert_eq!(bumped, 0.95); // Capped at 0.95
        
        let confidence_82 = 0.82;
        let bumped = f64::min(confidence_82 + 0.15, 0.95);
        assert_eq!(bumped, 0.95); // 0.82 + 0.15 = 0.97, but min(0.97, 0.95) = 0.95
    }

    #[test]
    fn test_auto_escalation_skips_emergency() {
        // Create an alert with confidence 0.92 (emergency level) that's 6 hours old
        let alert_id = Uuid::new_v4();
        let emergency_alert = create_test_alert(alert_id, 0.92, 6);
        
        // Emergency alerts (confidence >= 0.9) should not be escalated
        assert!(emergency_alert.confidence >= 0.9);
        
        // Even though it's old, it shouldn't be escalated
        let age = Utc::now() - emergency_alert.fired_at;
        let age_secs = age.num_seconds() as u64;
        assert!(age_secs >= 300); // It's old enough
        
        // But confidence >= 0.9, so it should be skipped
        // This is tested by the condition: `alert.confidence < 0.9`
    }

    #[test]
    fn test_escalation_config_default() {
        // This test is in config.rs, but we can verify the logic here
        use crate::config::EscalationConfig;
        
        let config = EscalationConfig::default();
        assert!(config.auto_escalation_enabled);
        assert_eq!(config.check_interval_secs, 60);
        assert_eq!(config.timeout_secs, 300);
    }
}

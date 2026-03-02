use serde::Serialize;
use std::sync::Arc;
use tracing;

use crate::alert_dedup::{compute_fingerprint, DedupCache};
use crate::alert_manager::AlertManagerClient;
use crate::anomaly_detector::Anomaly;
use crate::metrics::GatewayMetrics;

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
}

impl EscalationRouter {
    /// Creates a new escalation router.
    pub fn new(alert_manager: Arc<AlertManagerClient>, metrics: Arc<GatewayMetrics>) -> Self {
        Self {
            alert_manager,
            metrics,
            dedup_cache: Arc::new(DedupCache::new()),
        }
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

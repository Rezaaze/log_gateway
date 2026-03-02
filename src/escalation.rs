use serde::Serialize;
use std::sync::Arc;
use tracing;

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
}

impl EscalationRouter {
    /// Creates a new escalation router.
    pub fn new(alert_manager: Arc<AlertManagerClient>, metrics: Arc<GatewayMetrics>) -> Self {
        Self {
            alert_manager,
            metrics,
        }
    }

    /// Routes an anomaly based on its confidence score.
    ///
    /// 1. Determines escalation level from confidence
    /// 2. If no level (confidence < 0.5), returns early
    /// 3. Persists alert in ClickHouse (rule_id: None)
    /// 4. Records escalation metric
    /// 5. Logs at appropriate tracing level
    pub async fn route(&self, anomaly: &Anomaly) {
        // Determine escalation level
        let level = match EscalationLevel::from_confidence(anomaly.confidence) {
            Some(level) => level,
            None => {
                // Confidence below threshold, no alert
                return;
            }
        };

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
                    "BGP anomaly detected (WARNING): {:?} prefix={} origin_as={} confidence={}",
                    anomaly.anomaly_type,
                    anomaly.prefix,
                    anomaly.origin_as,
                    anomaly.confidence
                );
            }
            EscalationLevel::Critical => {
                tracing::error!(
                    "BGP anomaly detected (CRITICAL): {:?} prefix={} origin_as={} confidence={}",
                    anomaly.anomaly_type,
                    anomaly.prefix,
                    anomaly.origin_as,
                    anomaly.confidence
                );
            }
            EscalationLevel::Emergency => {
                tracing::error!(
                    "BGP anomaly detected (EMERGENCY): {:?} prefix={} origin_as={} confidence={}",
                    anomaly.anomaly_type,
                    anomaly.prefix,
                    anomaly.origin_as,
                    anomaly.confidence
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

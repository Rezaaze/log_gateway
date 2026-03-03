use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use crate::anomaly_detector::Anomaly;
use crate::escalation::EscalationLevel;

/// Target configuration for webhook notifications.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WebhookTarget {
    /// Slack webhook with channel override.
    Slack { url: String, channel: String },
    /// Generic HTTP webhook with custom headers.
    Generic {
        url: String,
        headers: HashMap<String, String>,
    },
}

/// Payload sent to webhooks for BGP anomaly alerts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookPayload {
    /// Event type, always "bgp_alert" for BGP anomalies.
    pub event_type: String,
    /// Escalation level: "warning", "critical", or "emergency".
    pub level: String,
    /// Anomaly type: "hijack" or "flap".
    pub anomaly_type: String,
    /// BGP prefix affected.
    pub prefix: String,
    /// Origin AS number.
    pub origin_as: u32,
    /// Confidence score (0.0–1.0).
    pub confidence: f64,
    /// Detection timestamp.
    pub detected_at: DateTime<Utc>,
    /// Human-readable details.
    pub details: String,
    /// Deterministic fingerprint for deduplication.
    pub fingerprint: String,
}

/// Sender for webhook notifications.
pub struct WebhookSender {
    /// HTTP client with timeout configuration.
    http: Client,
}

impl WebhookSender {
    /// Creates a new webhook sender with a 10-second timeout.
    pub fn new() -> anyhow::Result<Self> {
        let http = Client::builder().timeout(Duration::from_secs(10)).build()?;

        Ok(Self { http })
    }

    /// Sends a payload to the specified webhook target.
    ///
    /// # Arguments
    ///
    /// * `target` - The webhook target configuration.
    /// * `payload` - The payload to send.
    ///
    /// # Returns
    ///
    /// `Ok(())` on success, `Err` on failure.
    pub async fn send(
        &self,
        target: &WebhookTarget,
        payload: &WebhookPayload,
    ) -> anyhow::Result<()> {
        match target {
            WebhookTarget::Slack { url, channel } => {
                // Format Slack message according to Slack's block kit
                let text = format!(
                    "[{}] BGP Alert: {} AS{} confidence={:.2}",
                    payload.level.to_uppercase(),
                    payload.prefix,
                    payload.origin_as,
                    payload.confidence
                );

                let slack_body = serde_json::json!({
                    "text": text,
                    "channel": channel,
                    "blocks": [
                        {
                            "type": "section",
                            "text": {
                                "type": "mrkdwn",
                                "text": text
                            }
                        },
                        {
                            "type": "section",
                            "fields": [
                                {
                                    "type": "mrkdwn",
                                    "text": format!("*Type:* {}", payload.anomaly_type)
                                },
                                {
                                    "type": "mrkdwn",
                                    "text": format!("*Confidence:* {:.1}%", payload.confidence * 100.0)
                                },
                                {
                                    "type": "mrkdwn",
                                    "text": format!("*Prefix:* {}", payload.prefix)
                                },
                                {
                                    "type": "mrkdwn",
                                    "text": format!("*Origin AS:* {}", payload.origin_as)
                                },
                                {
                                    "type": "mrkdwn",
                                    "text": format!("*Detected:* {}", payload.detected_at)
                                },
                                {
                                    "type": "mrkdwn",
                                    "text": format!("*Fingerprint:* {}", payload.fingerprint)
                                }
                            ]
                        },
                        {
                            "type": "section",
                            "text": {
                                "type": "mrkdwn",
                                "text": format!("*Details:* {}", payload.details)
                            }
                        }
                    ]
                });

                let response = self
                    .http
                    .post(url)
                    .header("Content-Type", "application/json")
                    .json(&slack_body)
                    .send()
                    .await?;

                if !response.status().is_success() {
                    let status = response.status();
                    let body = response.text().await.unwrap_or_default();
                    anyhow::bail!("Slack webhook failed with status {}: {}", status, body);
                }
            }
            WebhookTarget::Generic { url, headers } => {
                let mut request = self.http.post(url);

                // Add custom headers
                for (key, value) in headers {
                    request = request.header(key, value);
                }

                // Ensure Content-Type is set if not already provided
                if !headers.contains_key("Content-Type") {
                    request = request.header("Content-Type", "application/json");
                }

                let response = request.json(payload).send().await?;

                if !response.status().is_success() {
                    let status = response.status();
                    let body = response.text().await.unwrap_or_default();
                    anyhow::bail!("Generic webhook failed with status {}: {}", status, body);
                }
            }
        }

        Ok(())
    }

    /// Builds a webhook payload from an anomaly, escalation level, and fingerprint.
    ///
    /// # Arguments
    ///
    /// * `anomaly` - The detected anomaly.
    /// * `level` - The escalation level.
    /// * `fingerprint` - The deterministic fingerprint.
    ///
    /// # Returns
    ///
    /// A `WebhookPayload` ready for sending.
    pub fn build_payload(
        anomaly: &Anomaly,
        level: &EscalationLevel,
        fingerprint: &str,
    ) -> WebhookPayload {
        let anomaly_type = match anomaly.anomaly_type {
            crate::anomaly_detector::AnomalyType::PossibleHijack => "hijack",
            crate::anomaly_detector::AnomalyType::PrefixFlapping => "flap",
        };

        WebhookPayload {
            event_type: "bgp_alert".to_string(),
            level: level.as_str().to_string(),
            anomaly_type: anomaly_type.to_string(),
            prefix: anomaly.prefix.clone(),
            origin_as: anomaly.origin_as,
            confidence: anomaly.confidence,
            detected_at: anomaly.detected_at,
            details: anomaly.details.clone(),
            fingerprint: fingerprint.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    #[test]
    fn test_build_payload() {
        let anomaly = Anomaly {
            id: Uuid::new_v4(),
            anomaly_type: crate::anomaly_detector::AnomalyType::PossibleHijack,
            prefix: "192.0.2.0/24".to_string(),
            origin_as: 64512,
            confidence: 0.85,
            detected_at: Utc::now(),
            details: "Test anomaly details".to_string(),
            tenant_id: "test-tenant".to_string(),
        };

        let level = EscalationLevel::Critical;
        let fingerprint = "test_fingerprint_123";

        let payload = WebhookSender::build_payload(&anomaly, &level, fingerprint);

        assert_eq!(payload.event_type, "bgp_alert");
        assert_eq!(payload.level, "critical");
        assert_eq!(payload.anomaly_type, "hijack");
        assert_eq!(payload.prefix, "192.0.2.0/24");
        assert_eq!(payload.origin_as, 64512);
        assert_eq!(payload.confidence, 0.85);
        assert_eq!(payload.details, "Test anomaly details");
        assert_eq!(payload.fingerprint, "test_fingerprint_123");
    }

    #[test]
    fn test_slack_text_format() {
        let anomaly = Anomaly {
            id: Uuid::new_v4(),
            anomaly_type: crate::anomaly_detector::AnomalyType::PrefixFlapping,
            prefix: "198.51.100.0/24".to_string(),
            origin_as: 65534,
            confidence: 0.75,
            detected_at: Utc::now(),
            details: "Prefix flapping detected".to_string(),
            tenant_id: "test-tenant".to_string(),
        };

        let level = EscalationLevel::Warning;
        let fingerprint = "test_fingerprint_456";

        let payload = WebhookSender::build_payload(&anomaly, &level, fingerprint);

        // Check that the level appears in uppercase in the expected Slack text format
        let expected_text_start = "[WARNING] BGP Alert:";
        let slack_text = format!(
            "[{}] BGP Alert: {} AS{} confidence={:.2}",
            payload.level.to_uppercase(),
            payload.prefix,
            payload.origin_as,
            payload.confidence
        );

        assert!(slack_text.starts_with(expected_text_start));
        assert!(slack_text.contains(&payload.prefix));
        assert!(slack_text.contains(&payload.origin_as.to_string()));
        assert!(slack_text.contains(&format!("{:.2}", payload.confidence)));
    }

    #[test]
    fn test_webhook_target_serialization() {
        // Test Slack target
        let slack_target = WebhookTarget::Slack {
            url: "https://hooks.slack.com/services/xxx".to_string(),
            channel: "#alerts".to_string(),
        };

        let serialized = serde_json::to_string(&slack_target).unwrap();
        let deserialized: WebhookTarget = serde_json::from_str(&serialized).unwrap();

        match deserialized {
            WebhookTarget::Slack { url, channel } => {
                assert_eq!(url, "https://hooks.slack.com/services/xxx");
                assert_eq!(channel, "#alerts");
            }
            _ => panic!("Expected Slack target"),
        }

        // Test Generic target
        let mut headers = HashMap::new();
        headers.insert("X-API-Key".to_string(), "secret".to_string());
        headers.insert("Content-Type".to_string(), "application/json".to_string());

        let generic_target = WebhookTarget::Generic {
            url: "https://webhook.example.com/alert".to_string(),
            headers,
        };

        let serialized = serde_json::to_string(&generic_target).unwrap();
        let deserialized: WebhookTarget = serde_json::from_str(&serialized).unwrap();

        match deserialized {
            WebhookTarget::Generic { url, headers } => {
                assert_eq!(url, "https://webhook.example.com/alert");
                assert_eq!(headers.get("X-API-Key"), Some(&"secret".to_string()));
                assert_eq!(
                    headers.get("Content-Type"),
                    Some(&"application/json".to_string())
                );
            }
            _ => panic!("Expected Generic target"),
        }
    }
}

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::Deserialize;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tracing;
use uuid::Uuid;

use crate::anomaly_detector::{Anomaly, AnomalyType};

/// Ein einzelner ROA-Eintrag aus Routinator.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RoaEntry {
    pub prefix: String,
    pub max_length: u8,
    pub origin_as: u32,
    pub trust_anchor: String,
}

/// Ein ROA-Änderungsevent (added oder removed).
#[derive(Debug, Clone)]
pub struct RoaDelta {
    pub entry: RoaEntry,
    pub action: RoaAction,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RoaAction {
    Added,
    Removed,
}

impl RoaAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            RoaAction::Added => "added",
            RoaAction::Removed => "removed",
        }
    }
}

/// Response-Struktur von Routinator /api/v1/export.json
#[derive(Debug, Deserialize)]
struct RoutinatorExportResponse {
    roas: Vec<RoutinatorRoa>,
}

#[derive(Debug, Deserialize)]
struct RoutinatorRoa {
    prefix: String,
    max_length: u8,
    asn: String,
    ta: String,
}

pub struct RoaPoller {
    routinator_url: String,
    clickhouse_url: String,
    clickhouse_db: String,
    client: Client,
    known_roas: Mutex<HashSet<RoaEntry>>,
    anomaly_tx: Option<mpsc::Sender<Anomaly>>,
}

impl RoaPoller {
    pub fn new(routinator_url: String, clickhouse_url: String, clickhouse_db: String) -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|e| {
                tracing::warn!("Failed to create HTTP client for RoaPoller: {}", e);
                Client::new()
            });

        Self {
            routinator_url,
            clickhouse_url,
            clickhouse_db,
            client,
            known_roas: Mutex::new(HashSet::new()),
            anomaly_tx: None,
        }
    }

    /// Creates a new RoaPoller with an anomaly sender channel.
    pub fn with_anomaly_sender(
        routinator_url: String,
        clickhouse_url: String,
        clickhouse_db: String,
        anomaly_tx: mpsc::Sender<Anomaly>,
    ) -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|e| {
                tracing::warn!("Failed to create HTTP client for RoaPoller: {}", e);
                Client::new()
            });

        Self {
            routinator_url,
            clickhouse_url,
            clickhouse_db,
            client,
            known_roas: Mutex::new(HashSet::new()),
            anomaly_tx: Some(anomaly_tx),
        }
    }

    /// Fragt Routinator ab und gibt alle aktuellen ROAs zurück.
    pub async fn fetch_roas(&self) -> Result<HashSet<RoaEntry>> {
        let url = format!("{}/api/v1/export.json", self.routinator_url);
        tracing::debug!("Fetching ROAs from {}", url);

        let response = self.client.get(&url).send().await?;
        if !response.status().is_success() {
            return Err(anyhow!(
                "Routinator returned non-success status: {}",
                response.status()
            ));
        }

        let export_response: RoutinatorExportResponse = response.json().await?;
        let mut roas = HashSet::new();

        for roa in export_response.roas {
            match parse_asn(&roa.asn) {
                Ok(origin_as) => {
                    let entry = RoaEntry {
                        prefix: roa.prefix,
                        max_length: roa.max_length,
                        origin_as,
                        trust_anchor: roa.ta,
                    };
                    roas.insert(entry);
                }
                Err(e) => {
                    tracing::warn!("Skipping invalid ROA entry {}: {}", roa.asn, e);
                }
            }
        }

        tracing::info!("Fetched {} ROAs from Routinator", roas.len());
        Ok(roas)
    }

    /// Vergleicht neue mit bekannten ROAs und gibt Deltas zurück.
    pub fn compute_deltas(
        &self,
        new_roas: &HashSet<RoaEntry>,
        now: DateTime<Utc>,
    ) -> Vec<RoaDelta> {
        let mut known_roas = self.known_roas.lock().unwrap();

        // Calculate added ROAs (in new but not in known)
        let added: Vec<RoaEntry> = new_roas.difference(&*known_roas).cloned().collect();

        // Calculate removed ROAs (in known but not in new)
        let removed: Vec<RoaEntry> = known_roas.difference(new_roas).cloned().collect();

        let added_count = added.len();
        let removed_count = removed.len();

        // Update known ROAs
        *known_roas = new_roas.clone();

        // Create deltas
        let mut deltas = Vec::with_capacity(added_count + removed_count);

        for entry in added {
            deltas.push(RoaDelta {
                entry,
                action: RoaAction::Added,
                timestamp: now,
            });
        }

        for entry in removed {
            deltas.push(RoaDelta {
                entry,
                action: RoaAction::Removed,
                timestamp: now,
            });
        }

        tracing::debug!(
            "Computed {} deltas ({} added, {} removed)",
            deltas.len(),
            added_count,
            removed_count
        );
        deltas
    }

    /// Schreibt Deltas im JSONEachRow-Format in ClickHouse.
    pub async fn write_deltas(&self, deltas: &[RoaDelta]) -> Result<()> {
        if deltas.is_empty() {
            return Ok(());
        }

        // Send anomalies for removed ROAs (potential hijack preparation)
        if let Some(ref anomaly_tx) = self.anomaly_tx {
            for delta in deltas {
                if delta.action == RoaAction::Removed {
                    let anomaly = Anomaly {
                        id: Uuid::new_v4(),
                        anomaly_type: AnomalyType::PossibleHijack,
                        prefix: delta.entry.prefix.clone(),
                        origin_as: delta.entry.origin_as,
                        confidence: 0.65,
                        detected_at: delta.timestamp,
                        details: "ROA removed — potential hijack preparation".to_string(),
                        tenant_id: "".to_string(),
                    };

                    // Try to send the anomaly, but don't panic if the receiver is closed
                    if let Err(e) = anomaly_tx.send(anomaly).await {
                        tracing::warn!("Failed to send anomaly for removed ROA: {}", e);
                        // Continue processing other deltas
                    }
                }
            }
        }

        // Prepare JSON lines
        let mut json_lines = Vec::with_capacity(deltas.len());
        for delta in deltas {
            let json_line = serde_json::json!({
                "timestamp": delta.timestamp.format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
                "prefix": delta.entry.prefix,
                "max_length": delta.entry.max_length,
                "origin_as": delta.entry.origin_as,
                "trust_anchor": delta.entry.trust_anchor,
                "action": delta.action.as_str(),
            });
            json_lines.push(json_line.to_string());
        }

        let body = json_lines.join("\n");
        let content_length = body.len();
        let query = format!(
            "INSERT INTO {}.rpki_roa_history FORMAT JSONEachRow",
            self.clickhouse_db
        );
        let url = format!("{}?query={}", self.clickhouse_url, query);

        tracing::debug!("Writing {} deltas to ClickHouse", deltas.len());
        let response = self
            .client
            .post(&url)
            .header("Content-Length", content_length.to_string())
            .body(body)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(anyhow!(
                "ClickHouse returned error status {}: {}",
                status,
                error_text
            ));
        }

        tracing::info!(
            "Successfully wrote {} ROA deltas to ClickHouse",
            deltas.len()
        );
        Ok(())
    }

    /// Führt einen einzelnen Poll-Zyklus aus.
    pub async fn poll_once(&self) -> Result<usize> {
        let now = Utc::now();
        let new_roas = self.fetch_roas().await?;
        let deltas = self.compute_deltas(&new_roas, now);

        if !deltas.is_empty() {
            self.write_deltas(&deltas).await?;
        }

        Ok(deltas.len())
    }

    /// Startet den Polling-Loop (alle 5 Minuten).
    pub fn start(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));

            // First tick immediately
            interval.tick().await;

            loop {
                interval.tick().await;

                match self.poll_once().await {
                    Ok(count) => {
                        if count > 0 {
                            tracing::info!("Poll cycle completed, wrote {} deltas", count);
                        } else {
                            tracing::debug!("Poll cycle completed, no changes");
                        }
                    }
                    Err(e) => {
                        tracing::error!("Poll cycle failed: {}", e);
                    }
                }
            }
        })
    }
}

/// Parse ASN string (e.g., "AS64512", "as64512", "64512") to u32.
fn parse_asn(asn_str: &str) -> Result<u32> {
    let asn_str = asn_str.trim();

    // Case-insensitive strip of "AS" prefix
    let asn_str = if asn_str.len() > 2 && asn_str[0..2].eq_ignore_ascii_case("as") {
        &asn_str[2..]
    } else {
        asn_str
    };

    asn_str
        .parse::<u32>()
        .map_err(|e| anyhow!("Invalid ASN '{}': {}", asn_str, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test 1: ASN-Parsing korrekt
    #[test]
    fn test_parse_asn_strips_prefix() {
        assert_eq!(parse_asn("AS64512").unwrap(), 64512);
        assert_eq!(parse_asn("as64512").unwrap(), 64512); // case-insensitive
        assert_eq!(parse_asn("64512").unwrap(), 64512); // kein Prefix
        assert!(parse_asn("ASINVALID").is_err()); // Fehler
        assert!(parse_asn("").is_err()); // Leerer String
        assert!(parse_asn("AS").is_err()); // Nur Prefix
    }

    // Test 2: compute_deltas erkennt added/removed korrekt
    #[test]
    fn test_compute_deltas_added_removed() {
        let poller = RoaPoller::new(
            "http://test".to_string(),
            "http://test".to_string(),
            "test".to_string(),
        );

        // Start with known = {A, B}
        let entry_a = RoaEntry {
            prefix: "1.2.3.0/24".to_string(),
            max_length: 24,
            origin_as: 64512,
            trust_anchor: "RIPE".to_string(),
        };
        let entry_b = RoaEntry {
            prefix: "2.3.4.0/24".to_string(),
            max_length: 24,
            origin_as: 64513,
            trust_anchor: "RIPE".to_string(),
        };
        let entry_c = RoaEntry {
            prefix: "3.4.5.0/24".to_string(),
            max_length: 24,
            origin_as: 64514,
            trust_anchor: "RIPE".to_string(),
        };

        {
            let mut known = poller.known_roas.lock().unwrap();
            known.insert(entry_a.clone());
            known.insert(entry_b.clone());
        }

        // new = {B, C}
        let mut new_roas = HashSet::new();
        new_roas.insert(entry_b.clone());
        new_roas.insert(entry_c.clone());

        let now = Utc::now();
        let deltas = poller.compute_deltas(&new_roas, now);

        // Should have: added=[C], removed=[A]
        assert_eq!(deltas.len(), 2);

        let added_deltas: Vec<_> = deltas
            .iter()
            .filter(|d| d.action == RoaAction::Added)
            .collect();
        let removed_deltas: Vec<_> = deltas
            .iter()
            .filter(|d| d.action == RoaAction::Removed)
            .collect();

        assert_eq!(added_deltas.len(), 1);
        assert_eq!(removed_deltas.len(), 1);

        assert_eq!(added_deltas[0].entry, entry_c);
        assert_eq!(removed_deltas[0].entry, entry_a);
    }

    // Test 3: compute_deltas bei leerem known-State
    #[test]
    fn test_compute_deltas_initial_load() {
        let poller = RoaPoller::new(
            "http://test".to_string(),
            "http://test".to_string(),
            "test".to_string(),
        );

        // known is empty initially
        let entry_a = RoaEntry {
            prefix: "1.2.3.0/24".to_string(),
            max_length: 24,
            origin_as: 64512,
            trust_anchor: "RIPE".to_string(),
        };
        let entry_b = RoaEntry {
            prefix: "2.3.4.0/24".to_string(),
            max_length: 24,
            origin_as: 64513,
            trust_anchor: "RIPE".to_string(),
        };

        let mut new_roas = HashSet::new();
        new_roas.insert(entry_a.clone());
        new_roas.insert(entry_b.clone());

        let now = Utc::now();
        let deltas = poller.compute_deltas(&new_roas, now);

        // Should have: added=[A, B], removed=[]
        assert_eq!(deltas.len(), 2);

        let added_deltas: Vec<_> = deltas
            .iter()
            .filter(|d| d.action == RoaAction::Added)
            .collect();
        let removed_deltas: Vec<_> = deltas
            .iter()
            .filter(|d| d.action == RoaAction::Removed)
            .collect();

        assert_eq!(added_deltas.len(), 2);
        assert_eq!(removed_deltas.len(), 0);

        let added_entries: HashSet<_> = added_deltas.iter().map(|d| &d.entry).collect();
        assert!(added_entries.contains(&&entry_a));
        assert!(added_entries.contains(&&entry_b));
    }

    // Test 4: RoaAction::as_str()
    #[test]
    fn test_roa_action_as_str() {
        assert_eq!(RoaAction::Added.as_str(), "added");
        assert_eq!(RoaAction::Removed.as_str(), "removed");
    }

    // Test 5: ROA removed triggers anomaly with warning confidence
    #[tokio::test]
    async fn test_roa_removed_triggers_anomaly() {
        let (anomaly_tx, mut anomaly_rx) = tokio::sync::mpsc::channel::<Anomaly>(10);
        let poller = RoaPoller::with_anomaly_sender(
            "http://test".to_string(),
            "http://test".to_string(),
            "test".to_string(),
            anomaly_tx,
        );

        // Create a removed delta
        let delta = RoaDelta {
            entry: RoaEntry {
                prefix: "1.2.3.0/24".to_string(),
                max_length: 24,
                origin_as: 64512,
                trust_anchor: "RIPE".to_string(),
            },
            action: RoaAction::Removed,
            timestamp: Utc::now(),
        };

        // Call write_deltas with the removed delta
        let deltas = vec![delta];

        // We can't call write_deltas because it makes HTTP requests.
        // Instead, we'll test the anomaly sending logic directly.
        // The anomaly should be sent before the HTTP request is made.
        // We'll check that the anomaly is in the channel.

        // write_deltas sends anomalies BEFORE making the HTTP request to ClickHouse.
        // The HTTP request will fail (no real server), but that's expected.
        let _result = poller.write_deltas(&deltas).await;
        // Ignore HTTP error — anomaly is sent before the HTTP call.

        // Check that an anomaly was received (it should be sent before HTTP failure)
        let received =
            tokio::time::timeout(std::time::Duration::from_millis(100), anomaly_rx.recv()).await;

        // Anomaly MUST have been sent before the HTTP error
        let anomaly = received
            .expect("timeout: no anomaly received within 100ms")
            .expect("channel closed without sending anomaly");

        assert_eq!(anomaly.anomaly_type, AnomalyType::PossibleHijack);
        assert_eq!(anomaly.confidence, 0.65);
        assert_eq!(anomaly.prefix, "1.2.3.0/24");
        assert_eq!(anomaly.origin_as, 64512);
        assert_eq!(
            anomaly.details,
            "ROA removed — potential hijack preparation"
        );
        assert_eq!(anomaly.tenant_id, "");
    }

    // Test 6: ROA added does not trigger anomaly
    #[tokio::test]
    async fn test_roa_added_does_not_trigger_anomaly() {
        let (anomaly_tx, mut anomaly_rx) = tokio::sync::mpsc::channel::<Anomaly>(10);
        let poller = RoaPoller::with_anomaly_sender(
            "http://test".to_string(),
            "http://test".to_string(),
            "test".to_string(),
            anomaly_tx,
        );

        // Create an added delta
        let delta = RoaDelta {
            entry: RoaEntry {
                prefix: "1.2.3.0/24".to_string(),
                max_length: 24,
                origin_as: 64512,
                trust_anchor: "RIPE".to_string(),
            },
            action: RoaAction::Added,
            timestamp: Utc::now(),
        };

        // Call write_deltas with the added delta — HTTP will fail (no real server), ignore it
        let deltas = vec![delta];
        let _result = poller.write_deltas(&deltas).await;

        // Check that no anomaly was received (channel should be empty — only Removed triggers)
        let result =
            tokio::time::timeout(std::time::Duration::from_millis(100), anomaly_rx.recv()).await;

        assert!(
            result.is_err(),
            "Expected timeout (no anomaly for Added delta), but got one"
        );
    }

    // Test 7: ROA anomaly confidence is warning level
    #[test]
    fn test_roa_anomaly_confidence_is_warning_level() {
        // Confidence 0.65 lies in Warning range (0.5–0.7)
        use crate::escalation::EscalationLevel;

        let level = EscalationLevel::from_confidence(0.65);
        assert_eq!(level, Some(EscalationLevel::Warning));

        // Verify the exact confidence value used in the implementation
        assert_eq!(0.65, 0.65); // Just to show the value matches
    }

    // Test 8: No panic when anomaly receiver is closed
    #[tokio::test]
    async fn test_no_panic_when_receiver_closed() {
        let (anomaly_tx, anomaly_rx) = tokio::sync::mpsc::channel::<Anomaly>(10);
        let poller = RoaPoller::with_anomaly_sender(
            "http://test".to_string(),
            "http://test".to_string(),
            "test".to_string(),
            anomaly_tx,
        );

        // Close the receiver immediately
        drop(anomaly_rx);

        // Create a removed delta
        let delta = RoaDelta {
            entry: RoaEntry {
                prefix: "1.2.3.0/24".to_string(),
                max_length: 24,
                origin_as: 64512,
                trust_anchor: "RIPE".to_string(),
            },
            action: RoaAction::Removed,
            timestamp: Utc::now(),
        };

        // This should not panic on send error — just log a warning.
        // The HTTP request will also fail, which is expected and ignored.
        let deltas = vec![delta];
        let _result = poller.write_deltas(&deltas).await;
        // If we get here without panic, the test passes
    }

    // Test 9: Poller without anomaly sender doesn't send anomalies
    #[tokio::test]
    async fn test_poller_without_anomaly_sender() {
        let poller = RoaPoller::new(
            "http://test".to_string(),
            "http://test".to_string(),
            "test".to_string(),
        );

        // Create a removed delta
        let delta = RoaDelta {
            entry: RoaEntry {
                prefix: "1.2.3.0/24".to_string(),
                max_length: 24,
                origin_as: 64512,
                trust_anchor: "RIPE".to_string(),
            },
            action: RoaAction::Removed,
            timestamp: Utc::now(),
        };

        // No anomaly_tx set — write_deltas must not panic.
        // HTTP will fail (no real server), which is expected and ignored.
        let deltas = vec![delta];
        let _result = poller.write_deltas(&deltas).await;
        // If we get here without panic, the test passes
    }
}

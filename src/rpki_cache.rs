use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing;

/// RPKI validation status for a BGP announcement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RpkiStatus {
    Valid,
    InvalidAsn,
    InvalidLength,
    NotFound,
    Unavailable,
}

/// A single Validated ROA Payload (VRP) from Routinator.
#[derive(Debug, Clone)]
struct Vrp {
    prefix_network: IpNet,
    max_length: u8,
    asn: u32,
}

/// Caches the complete VRP table from Routinator and validates locally.
/// The table is refreshed every 10 minutes in the background.
#[derive(Debug, Clone)]
pub struct RpkiCache {
    vrp_table: Arc<RwLock<Vec<Vrp>>>,
    client: reqwest::Client,
    routinator_url: String,
}

impl RpkiCache {
    /// Creates a new RPKI cache with the given Routinator URL.
    /// The VRP table starts empty; call `start_refresh_loop()` to begin loading.
    pub fn new(routinator_url: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_else(|e| {
                tracing::warn!("Failed to create HTTP client for RPKI cache: {}", e);
                reqwest::Client::new()
            });

        Self {
            vrp_table: Arc::new(RwLock::new(Vec::new())),
            client,
            routinator_url,
        }
    }

    /// Starts a background task that periodically fetches the VRP dump from Routinator.
    /// The first load happens immediately; subsequent loads occur every 10 minutes.
    /// If a load fails, the previous table is kept and an error is logged.
    pub async fn start_refresh_loop(&self) {
        let vrp_table = Arc::clone(&self.vrp_table);
        let client = self.client.clone();
        let routinator_url = self.routinator_url.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(600)); // 10 minutes

            // First load immediately
            if let Err(e) = Self::load_vrp_table(&vrp_table, &client, &routinator_url).await {
                tracing::error!("Initial VRP table load failed: {}", e);
            } else {
                tracing::info!("Initial VRP table loaded successfully");
            }

            loop {
                interval.tick().await;
                if let Err(e) = Self::load_vrp_table(&vrp_table, &client, &routinator_url).await {
                    tracing::error!("VRP table refresh failed: {}", e);
                } else {
                    tracing::debug!("VRP table refreshed successfully");
                }
            }
        });
    }

    /// Fetches the VRP dump from Routinator and updates the shared table.
    async fn load_vrp_table(
        vrp_table: &Arc<RwLock<Vec<Vrp>>>,
        client: &reqwest::Client,
        routinator_url: &str,
    ) -> Result<(), anyhow::Error> {
        let url = format!("{}/api/v1/vrps", routinator_url);
        let response = client.get(&url).send().await?;

        if !response.status().is_success() {
            anyhow::bail!("Routinator returned non‑2xx status: {}", response.status());
        }

        let json_text = response.text().await?;
        let raw_vrps: Vec<RawVrp> = serde_json::from_str(&json_text)?;

        let mut new_table = Vec::with_capacity(raw_vrps.len());

        for raw in raw_vrps {
            // Parse prefix string into IpNet
            let prefix_network: IpNet = match raw.prefix.parse() {
                Ok(net) => net,
                Err(e) => {
                    tracing::warn!("Skipping invalid prefix '{}': {}", raw.prefix, e);
                    continue;
                }
            };

            // Parse ASN string (format "AS64512") into u32
            let asn = match parse_asn(&raw.asn) {
                Ok(asn) => asn,
                Err(e) => {
                    tracing::warn!("Skipping invalid ASN '{}': {}", raw.asn, e);
                    continue;
                }
            };

            new_table.push(Vrp {
                prefix_network,
                max_length: raw.max_length,
                asn,
            });
        }

        // Sort by prefix length descending, so that more specific prefixes are checked first.
        // This ensures that if a prefix is covered by multiple VRPs, the most specific one is found quickly.
        new_table.sort_by(|a, b| {
            b.prefix_network
                .prefix_len()
                .cmp(&a.prefix_network.prefix_len())
        });

        let mut table = vrp_table.write().await;
        *table = new_table;
        tracing::info!("VRP table updated with {} entries", table.len());

        Ok(())
    }

    /// Validates a BGP announcement against the locally cached VRP table.
    ///
    /// This is a synchronous method that only reads the shared table.
    /// Returns `RpkiStatus::Unavailable` if the table has never been loaded.
    pub fn validate(&self, prefix: &str, origin_as: u32) -> RpkiStatus {
        // Parse the prefix to IpNet; if parsing fails, treat as NotFound.
        let prefix_network: IpNet = match prefix.parse() {
            Ok(net) => net,
            Err(_) => return RpkiStatus::NotFound,
        };

        // Acquire a read lock on the VRP table.
        // We use `try_read()` to avoid blocking if the table is being updated.
        // If we can't get the lock immediately, assume Unavailable.
        let table = match self.vrp_table.try_read() {
            Ok(table) => table,
            Err(_) => return RpkiStatus::Unavailable,
        };

        // If the table is empty, we haven't loaded any data yet.
        if table.is_empty() {
            return RpkiStatus::Unavailable;
        }

        // Find all VRPs that cover the given prefix.
        let covering_vrps: Vec<&Vrp> = table
            .iter()
            .filter(|vrp| vrp.prefix_network.contains(&prefix_network))
            .collect();

        if covering_vrps.is_empty() {
            return RpkiStatus::NotFound;
        }

        // Check each covering VRP for a match.
        for vrp in covering_vrps {
            if vrp.asn == origin_as {
                // ASN matches, now check length.
                let prefix_len = prefix_network.prefix_len();
                if prefix_len <= vrp.max_length {
                    return RpkiStatus::Valid;
                } else {
                    return RpkiStatus::InvalidLength;
                }
            }
        }

        // At least one covering VRP exists, but none matched the ASN.
        RpkiStatus::InvalidAsn
    }
}

/// Raw VRP as returned by Routinator's /api/v1/vrps endpoint.
#[derive(Debug, Deserialize)]
struct RawVrp {
    prefix: String,
    max_length: u8,
    asn: String,
}

/// Parses an ASN string like "AS64512" or "64512" into a u32.
fn parse_asn(asn_str: &str) -> Result<u32, anyhow::Error> {
    let trimmed = asn_str.trim();
    if trimmed.starts_with("AS") || trimmed.starts_with("as") {
        trimmed[2..].parse().map_err(|e| anyhow::anyhow!("{}", e))
    } else {
        trimmed.parse().map_err(|e| anyhow::anyhow!("{}", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_asn() {
        assert_eq!(parse_asn("AS64512").unwrap(), 64512);
        assert_eq!(parse_asn("as12345").unwrap(), 12345);
        assert_eq!(parse_asn("789").unwrap(), 789);
        assert!(parse_asn("ASinvalid").is_err());
        assert!(parse_asn("").is_err());
    }

    #[test]
    fn test_validate_empty_table() {
        let cache = RpkiCache::new("http://dummy".to_string());
        // Table is empty -> Unavailable
        assert_eq!(
            cache.validate("192.0.2.0/24", 64512),
            RpkiStatus::Unavailable
        );
    }

    #[tokio::test]
    async fn test_validate_with_mock_data() {
        let cache = RpkiCache::new("http://dummy".to_string());
        {
            let mut table = cache.vrp_table.write().await;
            *table = vec![
                Vrp {
                    prefix_network: "192.0.2.0/24".parse().unwrap(),
                    max_length: 24,
                    asn: 64512,
                },
                Vrp {
                    prefix_network: "203.0.113.0/24".parse().unwrap(),
                    max_length: 26,
                    asn: 65536,
                },
            ];
        }

        // Exact match
        assert_eq!(cache.validate("192.0.2.0/24", 64512), RpkiStatus::Valid);
        // Longer prefix than allowed
        assert_eq!(
            cache.validate("192.0.2.0/28", 64512),
            RpkiStatus::InvalidLength
        );
        // Wrong ASN
        assert_eq!(
            cache.validate("192.0.2.0/24", 99999),
            RpkiStatus::InvalidAsn
        );
        // Covered by more specific VRP with different ASN, but prefix length exceeds max_length
        assert_eq!(
            cache.validate("203.0.113.0/28", 65536),
            RpkiStatus::InvalidLength
        );
        // Not covered at all
        assert_eq!(cache.validate("10.0.0.0/8", 12345), RpkiStatus::NotFound);
        // Invalid prefix
        assert_eq!(cache.validate("invalid", 12345), RpkiStatus::NotFound);
    }
}

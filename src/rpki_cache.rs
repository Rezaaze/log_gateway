use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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

/// Index type for fast VRP lookup.
/// Key: (prefix_len, network_addr_as_u128)
/// Value: list of (max_length, asn) for VRPs with exactly this network
type VrpIndex = HashMap<(u8, u128), Vec<(u8, u32)>>;

/// Caches the complete VRP table from Routinator and validates locally.
/// The table is refreshed every 10 minutes in the background.
#[derive(Debug, Clone)]
pub struct RpkiCache {
    vrp_index: Arc<RwLock<VrpIndex>>,
    client: reqwest::Client,
    routinator_url: String,
}

impl RpkiCache {
    /// Creates a new RPKI cache with the given Routinator URL.
    /// The VRP index starts empty; call `start_refresh_loop()` to begin loading.
    pub fn new(routinator_url: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_else(|e| {
                tracing::warn!("Failed to create HTTP client for RPKI cache: {}", e);
                reqwest::Client::new()
            });

        Self {
            vrp_index: Arc::new(RwLock::new(HashMap::new())),
            client,
            routinator_url,
        }
    }

    /// Starts a background task that periodically fetches the VRP dump from Routinator.
    /// The first load happens immediately; subsequent loads occur every 10 minutes.
    /// If a load fails, the previous table is kept and an error is logged.
    pub async fn start_refresh_loop(&self) {
        let vrp_index = Arc::clone(&self.vrp_index);
        let client = self.client.clone();
        let routinator_url = self.routinator_url.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(600)); // 10 minutes

            // First load with exponential backoff until successful
            let mut backoff_secs = 5u64;
            loop {
                match Self::load_vrp_index(&vrp_index, &client, &routinator_url).await {
                    Ok(_) => {
                        tracing::info!("Initial VRP index loaded successfully");
                        break; // Exit retry loop
                    }
                    Err(e) => {
                        tracing::warn!("VRP index load failed (retry in {}s): {}", backoff_secs, e);
                        tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                        backoff_secs = (backoff_secs * 2).min(60); // Max 60s
                    }
                }
            }

            // Normal 10-minute refresh loop
            loop {
                interval.tick().await;
                if let Err(e) = Self::load_vrp_index(&vrp_index, &client, &routinator_url).await {
                    tracing::error!("VRP index refresh failed: {}", e);
                } else {
                    tracing::debug!("VRP index refreshed successfully");
                }
            }
        });
    }

    /// Fetches the VRP dump from Routinator and updates the shared index.
    async fn load_vrp_index(
        vrp_index: &Arc<RwLock<VrpIndex>>,
        client: &reqwest::Client,
        routinator_url: &str,
    ) -> Result<(), anyhow::Error> {
        // Routinator 0.15+ uses /json (with metadata+roas wrapper)
        let url = format!("{}/json", routinator_url);
        let response = client.get(&url).send().await?;

        if !response.status().is_success() {
            anyhow::bail!("Routinator returned non‑2xx status: {}", response.status());
        }

        let json_text = response.text().await?;
        let wrapper: RoutinatorResponse = serde_json::from_str(&json_text)?;
        let raw_vrps = wrapper.roas;

        let mut new_index: VrpIndex = HashMap::with_capacity(raw_vrps.len());

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

            let key = (
                prefix_network.prefix_len(),
                addr_to_u128(prefix_network.network()),
            );
            new_index
                .entry(key)
                .or_default()
                .push((raw.max_length, asn));
        }

        let mut index = vrp_index.write().await;
        *index = new_index;
        tracing::info!("VRP index built with {} unique networks", index.len());

        Ok(())
    }

    /// Validates a BGP announcement against the locally cached VRP index.
    ///
    /// This is a synchronous method that only reads the shared index.
    /// Returns `RpkiStatus::Unavailable` if the index has never been loaded.
    pub fn validate(&self, prefix: &str, origin_as: u32) -> RpkiStatus {
        // Parse the prefix to IpNet; if parsing fails, treat as NotFound.
        let prefix_network: IpNet = match prefix.parse() {
            Ok(net) => net,
            Err(_) => return RpkiStatus::NotFound,
        };

        // Acquire a read lock on the VRP index.
        // We use `try_read()` to avoid blocking if the index is being updated.
        // If we can't get the lock immediately, assume Unavailable.
        let index = match self.vrp_index.try_read() {
            Ok(index) => index,
            Err(_) => return RpkiStatus::Unavailable,
        };

        // If the index is empty, we haven't loaded any data yet.
        if index.is_empty() {
            return RpkiStatus::Unavailable;
        }

        let prefix_len = prefix_network.prefix_len();
        let prefix_addr = prefix_network.addr();
        let mut found_covering = false;

        // Iterate candidate prefix lengths from prefix_len down to 0.
        // For each candidate length, compute the truncated network address
        // and look up VRPs with exactly that network.
        for candidate_len in (0..=prefix_len).rev() {
            let candidate_net = match IpNet::new(prefix_addr, candidate_len) {
                Ok(net) => net.trunc(),
                Err(_) => continue, // Should not happen with valid candidate_len
            };
            let key = (candidate_len, addr_to_u128(candidate_net.network()));

            if let Some(vrp_list) = index.get(&key) {
                found_covering = true;
                for &(max_length, asn) in vrp_list {
                    if asn == origin_as {
                        // ASN matches, now check length.
                        if prefix_len <= max_length {
                            return RpkiStatus::Valid;
                        } else {
                            return RpkiStatus::InvalidLength;
                        }
                    }
                }
            }
        }

        if found_covering {
            // At least one covering VRP exists, but none matched the ASN.
            RpkiStatus::InvalidAsn
        } else {
            // No covering VRP found.
            RpkiStatus::NotFound
        }
    }
}

/// Top-level response from Routinator's /json endpoint.
#[derive(Debug, Deserialize)]
struct RoutinatorResponse {
    roas: Vec<RawVrp>,
}

/// Raw VRP as returned by Routinator's /json endpoint.
#[derive(Debug, Deserialize)]
struct RawVrp {
    prefix: String,
    #[serde(rename = "maxLength")]
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

/// Converts an IP address to a u128 representation.
/// IPv4 addresses are mapped to IPv6 (::ffff:a.b.c.d) and then converted to u128.
fn addr_to_u128(addr: std::net::IpAddr) -> u128 {
    match addr {
        std::net::IpAddr::V4(v4) => v4.to_ipv6_mapped().to_bits(),
        std::net::IpAddr::V6(v6) => v6.to_bits(),
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
    fn test_addr_to_u128() {
        // IPv4
        let v4 = "192.0.2.1".parse().unwrap();
        let v4_u128 = addr_to_u128(v4);
        assert_eq!(v4_u128, 0xffffc0000201); // ::ffff:192.0.2.1

        // IPv6
        let v6 = "2001:db8::1".parse().unwrap();
        let v6_u128 = addr_to_u128(v6);
        assert_eq!(v6_u128, 0x20010db8000000000000000000000001);
    }

    #[test]
    fn test_validate_empty_index() {
        let cache = RpkiCache::new("http://dummy".to_string());
        // Index is empty -> Unavailable
        assert_eq!(
            cache.validate("192.0.2.0/24", 64512),
            RpkiStatus::Unavailable
        );
    }

    #[tokio::test]
    async fn test_validate_with_mock_data() {
        let cache = RpkiCache::new("http://dummy".to_string());
        {
            let mut index = cache.vrp_index.write().await;
            *index = HashMap::from([
                (
                    (24, addr_to_u128("192.0.2.0".parse().unwrap())),
                    vec![(24, 64512)],
                ),
                (
                    (24, addr_to_u128("203.0.113.0".parse().unwrap())),
                    vec![(26, 65536)],
                ),
            ]);
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

    #[tokio::test]
    async fn test_validate_with_multiple_vrps_same_network() {
        let cache = RpkiCache::new("http://dummy".to_string());
        {
            let mut index = cache.vrp_index.write().await;
            *index = HashMap::from([(
                (20, addr_to_u128("192.0.0.0".parse().unwrap())),
                vec![(24, 64512), (24, 65536), (20, 64512)],
            )]);
        }

        // Should match the first VRP with matching ASN
        assert_eq!(cache.validate("192.0.2.0/24", 64512), RpkiStatus::Valid);
        // Different ASN but also present
        assert_eq!(cache.validate("192.0.2.0/24", 65536), RpkiStatus::Valid);
        // ASN not in list
        assert_eq!(
            cache.validate("192.0.2.0/24", 99999),
            RpkiStatus::InvalidAsn
        );
        // Prefix length 20 matches max_length 20
        assert_eq!(cache.validate("192.0.0.0/20", 64512), RpkiStatus::Valid);
        // Prefix length 25 exceeds max_length 24 for ASN 64512
        assert_eq!(
            cache.validate("192.0.0.0/25", 64512),
            RpkiStatus::InvalidLength
        );
    }

    #[tokio::test]
    async fn test_validate_ipv6() {
        let cache = RpkiCache::new("http://dummy".to_string());
        {
            let mut index = cache.vrp_index.write().await;
            *index = HashMap::from([(
                (48, addr_to_u128("2001:db8::".parse().unwrap())),
                vec![(64, 64512)],
            )]);
        }

        assert_eq!(cache.validate("2001:db8::/48", 64512), RpkiStatus::Valid);
        assert_eq!(cache.validate("2001:db8::/64", 64512), RpkiStatus::Valid);
        assert_eq!(
            cache.validate("2001:db8::/80", 64512),
            RpkiStatus::InvalidLength
        );
        assert_eq!(
            cache.validate("2001:db8::/48", 99999),
            RpkiStatus::InvalidAsn
        );
    }
}

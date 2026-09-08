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
    refresh_interval: Duration,
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
            refresh_interval: Duration::from_secs(600),
        }
    }

    /// Overrides the refresh interval (default: 10 minutes). Public VRP feeds
    /// are a ~100 MB download each time and are operated by third parties —
    /// poll those hourly or slower.
    pub fn with_refresh_interval(mut self, interval: Duration) -> Self {
        self.refresh_interval = interval;
        self
    }

    /// Starts a background task that periodically fetches the VRP dump from Routinator.
    /// The first load happens immediately; subsequent loads occur every 10 minutes.
    /// If a load fails, the previous table is kept and an error is logged.
    pub async fn start_refresh_loop(&self) {
        let vrp_index = Arc::clone(&self.vrp_index);
        let client = self.client.clone();
        let routinator_url = self.routinator_url.clone();
        let refresh_interval = self.refresh_interval;

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(refresh_interval);

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
        let url = vrp_endpoint_url(routinator_url);
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

            // Accepts both "AS64512"/"64512" (Routinator) and 64512 (rpki-client)
            let asn = match raw.asn.to_u32() {
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

    /// Test/benchmark helper: directly populate the VRP index with mock data.
    /// Not intended for production use.
    pub async fn set_test_data(&self, index: HashMap<(u8, u128), Vec<(u8, u32)>>) {
        let mut write_guard = self.vrp_index.write().await;
        *write_guard = index;
    }
}

/// Top-level response from Routinator's /json endpoint.
#[derive(Debug, Deserialize)]
struct RoutinatorResponse {
    roas: Vec<RawVrp>,
}

/// Raw VRP as returned by a VRP JSON endpoint.
#[derive(Debug, Deserialize)]
struct RawVrp {
    prefix: String,
    #[serde(rename = "maxLength")]
    max_length: u8,
    asn: RawAsn,
}

/// The `asn` field is spelled differently by different VRP publishers:
/// Routinator emits a string (`"AS13335"`, sometimes `"13335"`), while
/// rpki-client-based feeds — including the public one at
/// `https://rpki.cloudflare.com/rpki.json` — emit a bare JSON number
/// (`13335`). Accepting both lets the same code run against a self-hosted
/// validator and a public feed without a second parser.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawAsn {
    Number(u32),
    Text(String),
}

impl RawAsn {
    fn to_u32(&self) -> Result<u32, anyhow::Error> {
        match self {
            RawAsn::Number(n) => Ok(*n),
            RawAsn::Text(s) => parse_asn(s),
        }
    }
}

impl std::fmt::Display for RawAsn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RawAsn::Number(n) => write!(f, "{}", n),
            RawAsn::Text(s) => write!(f, "{}", s),
        }
    }
}

/// Builds the VRP JSON URL from the configured base.
///
/// A self-hosted Routinator is configured by its base address
/// (`http://routinator:8323`) and serves the dump under `/json`. A public
/// feed is configured by its full URL (`https://rpki.cloudflare.com/rpki.json`)
/// and must be used verbatim — appending `/json` to it would 404.
fn vrp_endpoint_url(configured: &str) -> String {
    let trimmed = configured.trim_end_matches('/');
    if trimmed.ends_with(".json") {
        trimmed.to_string()
    } else {
        format!("{}/json", trimmed)
    }
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
pub(crate) fn addr_to_u128(addr: std::net::IpAddr) -> u128 {
    match addr {
        std::net::IpAddr::V4(v4) => v4.to_ipv6_mapped().to_bits(),
        std::net::IpAddr::V6(v6) => v6.to_bits(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A public rpki-client feed (e.g. https://rpki.cloudflare.com/rpki.json)
    /// emits `asn` as a bare number; Routinator emits it as a string. Both
    /// must parse, so the detector can run against a public feed with no
    /// self-hosted validator.
    #[test]
    fn test_parses_numeric_and_string_asn() {
        let cloudflare = r#"{"roas":[{"asn":13335,"prefix":"1.0.0.0/24","maxLength":24,"ta":"apnic","expires":1789481874}]}"#;
        let parsed: RoutinatorResponse = serde_json::from_str(cloudflare).expect("numeric asn");
        assert_eq!(parsed.roas[0].asn.to_u32().unwrap(), 13335);
        assert_eq!(parsed.roas[0].max_length, 24);

        let routinator = r#"{"roas":[{"asn":"AS13335","prefix":"1.0.0.0/24","maxLength":24}]}"#;
        let parsed: RoutinatorResponse = serde_json::from_str(routinator).expect("string asn");
        assert_eq!(parsed.roas[0].asn.to_u32().unwrap(), 13335);

        let plain = r#"{"roas":[{"asn":"13335","prefix":"1.0.0.0/24","maxLength":24}]}"#;
        let parsed: RoutinatorResponse =
            serde_json::from_str(plain).expect("unprefixed string asn");
        assert_eq!(parsed.roas[0].asn.to_u32().unwrap(), 13335);
    }

    #[test]
    fn test_vrp_endpoint_url() {
        // Self-hosted Routinator: base address, dump lives under /json
        assert_eq!(
            vrp_endpoint_url("http://routinator:8323"),
            "http://routinator:8323/json"
        );
        assert_eq!(
            vrp_endpoint_url("http://routinator:8323/"),
            "http://routinator:8323/json"
        );
        // Public feed: full URL, must be used verbatim
        assert_eq!(
            vrp_endpoint_url("https://rpki.cloudflare.com/rpki.json"),
            "https://rpki.cloudflare.com/rpki.json"
        );
    }

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

    #[tokio::test]
    async fn test_validate_hierarchical_supernet_coverage() {
        let cache = RpkiCache::new("http://dummy".to_string());
        {
            let mut index = cache.vrp_index.write().await;
            *index = HashMap::from([(
                (8, addr_to_u128("10.0.0.0".parse().unwrap())),
                vec![(24, 64512)],
            )]);
        }

        // /24 announcement covered by /8 VRP with max_length=24
        assert_eq!(cache.validate("10.1.2.0/24", 64512), RpkiStatus::Valid);
    }

    #[tokio::test]
    async fn test_validate_supernet_too_specific() {
        let cache = RpkiCache::new("http://dummy".to_string());
        {
            let mut index = cache.vrp_index.write().await;
            *index = HashMap::from([(
                (8, addr_to_u128("10.0.0.0".parse().unwrap())),
                vec![(16, 64512)],
            )]);
        }

        // /24 announcement exceeds max_length=16
        assert_eq!(
            cache.validate("10.1.0.0/24", 64512),
            RpkiStatus::InvalidLength
        );
    }

    #[tokio::test]
    async fn test_validate_supernet_wrong_asn() {
        let cache = RpkiCache::new("http://dummy".to_string());
        {
            let mut index = cache.vrp_index.write().await;
            *index = HashMap::from([(
                (8, addr_to_u128("10.0.0.0".parse().unwrap())),
                vec![(24, 64512)],
            )]);
        }

        // /24 announcement covered by /8 VRP but wrong ASN
        assert_eq!(cache.validate("10.1.2.0/24", 99999), RpkiStatus::InvalidAsn);
    }

    #[tokio::test]
    async fn test_validate_multiple_covering_vrps_one_matches() {
        let cache = RpkiCache::new("http://dummy".to_string());
        {
            let mut index = cache.vrp_index.write().await;
            *index = HashMap::from([
                (
                    (16, addr_to_u128("10.1.0.0".parse().unwrap())),
                    vec![(24, 11111)],
                ),
                (
                    (8, addr_to_u128("10.0.0.0".parse().unwrap())),
                    vec![(24, 64512)],
                ),
            ]);
        }

        // /24 announcement: /16 matches length but wrong AS, /8 matches correctly
        assert_eq!(cache.validate("10.1.2.0/24", 64512), RpkiStatus::Valid);
    }

    #[tokio::test]
    async fn test_validate_concurrent_reads_during_write() {
        let cache = RpkiCache::new("http://dummy".to_string());
        // Start with some data
        {
            let mut index = cache.vrp_index.write().await;
            *index = HashMap::from([(
                (24, addr_to_u128("192.0.2.0".parse().unwrap())),
                vec![(24, 64512)],
            )]);
        }

        // Spawn a writer task that will update the index after a short delay
        let cache_clone = cache.clone();
        let writer = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            let mut index = cache_clone.vrp_index.write().await;
            *index = HashMap::from([(
                (24, addr_to_u128("203.0.113.0".parse().unwrap())),
                vec![(24, 65536)],
            )]);
        });

        // Spawn 50 reader tasks that call validate concurrently
        let mut readers = Vec::new();
        for _ in 0..50 {
            let cache_clone = cache.clone();
            readers.push(tokio::spawn(async move {
                // validate may return Unavailable if try_read fails, but should not panic
                let _ = cache_clone.validate("192.0.2.0/24", 64512);
            }));
        }

        // Wait for all readers
        for reader in readers {
            reader.await.expect("reader task panicked");
        }

        // Wait for writer
        writer.await.expect("writer task panicked");

        // After write, new data should be visible
        assert_eq!(cache.validate("203.0.113.0/24", 65536), RpkiStatus::Valid);
    }

    #[tokio::test]
    async fn test_validate_ipv4_classful_supernet() {
        let cache = RpkiCache::new("http://dummy".to_string());
        {
            let mut index = cache.vrp_index.write().await;
            *index = HashMap::from([(
                (16, addr_to_u128("192.168.0.0".parse().unwrap())),
                vec![(24, 1234)],
            )]);
        }

        // Valid: /24 within max_length=24
        assert_eq!(cache.validate("192.168.1.0/24", 1234), RpkiStatus::Valid);
        // InvalidLength: /25 exceeds max_length=24
        assert_eq!(
            cache.validate("192.168.1.0/25", 1234),
            RpkiStatus::InvalidLength
        );
        // InvalidAsn: correct prefix but wrong AS
        assert_eq!(
            cache.validate("192.168.1.0/24", 9999),
            RpkiStatus::InvalidAsn
        );
        // NotFound: different network
        assert_eq!(
            cache.validate("198.51.100.0/24", 1234),
            RpkiStatus::NotFound
        );
    }
}

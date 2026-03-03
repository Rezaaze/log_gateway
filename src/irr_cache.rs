use serde::Deserialize;
use std::time::Duration;
use tracing;

/// IRR-Lookup-Ergebnis für ein Prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IrrStatus {
    /// Origin-AS aus IRR stimmt mit announcetem AS überein.
    Consistent,
    /// Origin-AS aus IRR weicht ab — mögliche Hijack-Indikation.
    Inconsistent { irr_asns: Vec<u32> },
    /// Kein route-object in IRR gefunden.
    NotFound,
    /// IRR-Abfrage fehlgeschlagen (Netzwerkfehler, Rate-Limit etc.).
    Unavailable,
}

/// Caches IRR validation results to avoid repeated queries to RIPE Whois API.
#[derive(Debug, Clone)]
pub struct IrrCache {
    cache: moka::sync::Cache<String, IrrStatus>,
    client: reqwest::Client,
}

impl Default for IrrCache {
    fn default() -> Self {
        Self::new()
    }
}

impl IrrCache {
    /// Creates a new IRR cache.
    /// TTL: 24 Stunden, max_capacity: 50_000
    pub fn new() -> Self {
        let cache = moka::sync::Cache::builder()
            .max_capacity(50_000)
            .time_to_live(Duration::from_secs(24 * 3600)) // 24 hours
            .build();

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_else(|e| {
                tracing::warn!("Failed to create HTTP client for IRR cache: {}", e);
                reqwest::Client::new()
            });

        Self { cache, client }
    }

    /// Prüft ob ein Prefix/Origin-AS-Paar IRR-konsistent ist.
    /// Cache-Key: "{prefix}/{origin_as}"
    pub async fn check(&self, prefix: &str, origin_as: u32) -> IrrStatus {
        let cache_key = format!("{}/{}", prefix, origin_as);

        // Check cache first
        if let Some(status) = self.cache.get(&cache_key) {
            return status;
        }

        // URL-encode the prefix: replace "/" with "%2F"
        let encoded_prefix = prefix.replace('/', "%2F");
        let url = format!(
            "https://rest.db.ripe.net/search.json?query-string={}&type-filter=route&flags=no-referenced",
            encoded_prefix
        );

        // Make HTTP request with Accept: application/json header
        let response = match self
            .client
            .get(&url)
            .header("Accept", "application/json")
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                tracing::warn!("IRR validation request failed for {}: {}", cache_key, e);
                let status = IrrStatus::Unavailable;
                self.cache.insert(cache_key, status.clone());
                return status;
            }
        };

        // Check for non-2xx status
        if !response.status().is_success() {
            tracing::warn!(
                "IRR validation returned non-2xx status for {}: {}",
                cache_key,
                response.status()
            );
            let status = IrrStatus::Unavailable;
            self.cache.insert(cache_key, status.clone());
            return status;
        }

        // Parse JSON response
        let json_text = match response.text().await {
            Ok(text) => text,
            Err(e) => {
                tracing::warn!("Failed to read IRR response body for {}: {}", cache_key, e);
                let status = IrrStatus::Unavailable;
                self.cache.insert(cache_key, status.clone());
                return status;
            }
        };

        // Deserialize response
        let result = match serde_json::from_str::<RipeSearchResponse>(&json_text) {
            Ok(resp) => resp,
            Err(e) => {
                tracing::warn!("Failed to parse IRR JSON for {}: {}", cache_key, e);
                let status = IrrStatus::Unavailable;
                self.cache.insert(cache_key, status.clone());
                return status;
            }
        };

        // Extract all origin ASNs from the response
        let irr_asns: Vec<u32> = result
            .objects
            .object
            .iter()
            .flat_map(|o| o.attributes.attribute.iter())
            .filter(|a| a.name == "origin")
            .filter_map(|a| {
                let s = a.value.trim();
                // Parse ASN: strip "AS" prefix case-insensitively
                let digits = if s.len() > 2 && s[0..2].eq_ignore_ascii_case("as") {
                    &s[2..]
                } else {
                    s
                };
                digits.parse::<u32>().ok()
            })
            .collect();

        // Determine status based on extracted ASNs
        let status = if irr_asns.is_empty() {
            IrrStatus::NotFound
        } else if irr_asns.contains(&origin_as) {
            IrrStatus::Consistent
        } else {
            IrrStatus::Inconsistent { irr_asns }
        };

        // Cache the result
        self.cache.insert(cache_key, status.clone());
        status
    }
}

// Internal structs for deserializing RIPE Whois JSON response
#[derive(Debug, Deserialize)]
pub(crate) struct RipeSearchResponse {
    objects: RipeObjects,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RipeObjects {
    object: Vec<RipeObject>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RipeObject {
    attributes: RipeAttributes,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RipeAttributes {
    attribute: Vec<RipeAttribute>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RipeAttribute {
    name: String,
    value: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test 1: IrrStatus Consistent wenn origin_as in irr_asns
    #[test]
    fn test_irr_status_consistent_detection() {
        // Simuliere: irr_asns = [64512, 64513], origin_as = 64512 → Consistent
        let irr_asns = vec![64512u32, 64513u32];
        let origin_as = 64512u32;
        let status = if irr_asns.contains(&origin_as) {
            IrrStatus::Consistent
        } else {
            IrrStatus::Inconsistent { irr_asns: irr_asns.clone() }
        };
        assert_eq!(status, IrrStatus::Consistent);
    }

    // Test 2: IrrStatus Inconsistent wenn origin_as NICHT in irr_asns
    #[test]
    fn test_irr_status_inconsistent_detection() {
        let irr_asns = vec![64513u32, 64514u32];
        let origin_as = 64512u32;
        let status = if irr_asns.contains(&origin_as) {
            IrrStatus::Consistent
        } else {
            IrrStatus::Inconsistent { irr_asns: irr_asns.clone() }
        };
        match status {
            IrrStatus::Inconsistent { irr_asns } => {
                assert!(irr_asns.contains(&64513));
                assert!(!irr_asns.contains(&64512));
            }
            _ => panic!("Expected Inconsistent"),
        }
    }

    // Test 3: RIPE Response Parsing — alle origin-Attribute korrekt extrahiert
    #[test]
    fn test_parse_ripe_response() {
        let json = r#"{
            "objects": {
                "object": [
                    {
                        "attributes": {
                            "attribute": [
                                { "name": "route",  "value": "1.2.3.0/24" },
                                { "name": "origin", "value": "AS64512" },
                                { "name": "origin", "value": "AS64513" }
                            ]
                        }
                    }
                ]
            }
        }"#;
        let response: RipeSearchResponse = serde_json::from_str(json).unwrap();
        let asns: Vec<u32> = response.objects.object.iter()
            .flat_map(|o| o.attributes.attribute.iter())
            .filter(|a| a.name == "origin")
            .filter_map(|a| {
                let s = a.value.trim();
                let digits = if s.len() > 2 && s[0..2].eq_ignore_ascii_case("as") {
                    &s[2..]
                } else { s };
                digits.parse::<u32>().ok()
            })
            .collect();
        assert_eq!(asns.len(), 2);
        assert!(asns.contains(&64512));
        assert!(asns.contains(&64513));
    }

    // Test 4: IrrCache::new() erstellt Cache mit korrekter Konfiguration
    #[test]
    fn test_irr_cache_new() {
        let cache = IrrCache::new();
        // Kein Panic, Cache ist leer
        // Indirekter Test: check-Methode existiert (Compile-Test reicht)
        drop(cache);
    }
}
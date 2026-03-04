use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
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

/// Caches RPKI validation results to avoid repeated queries to Routinator.
/// Uses a semaphore to limit concurrent requests to Routinator (max 100).
#[derive(Debug, Clone)]
pub struct RpkiCache {
    cache: moka::sync::Cache<String, RpkiStatus>,
    client: reqwest::Client,
    routinator_url: String,
    semaphore: Arc<Semaphore>, // Limit concurrent requests to 100
}

impl RpkiCache {
    /// Creates a new RPKI cache with the given Routinator URL.
    /// Limits concurrent requests to Routinator to prevent 503 errors.
    pub fn new(routinator_url: String) -> Self {
        let cache = moka::sync::Cache::builder()
            .max_capacity(100_000)
            .time_to_live(Duration::from_secs(3600))
            .build();

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap_or_else(|e| {
                tracing::warn!("Failed to create HTTP client for RPKI cache: {}", e);
                reqwest::Client::new()
            });

        // Semaphore with max 100 concurrent requests to Routinator
        let semaphore = Arc::new(Semaphore::new(100));

        Self {
            cache,
            client,
            routinator_url,
            semaphore,
        }
    }

    /// Validates a BGP announcement against RPKI.
    ///
    /// Returns `RpkiStatus::Unavailable` on any network error or non‑2xx response.
    /// Results are cached for 1 hour.
    /// Uses semaphore to limit concurrent requests to 100 (prevents Routinator 503).
    pub async fn validate(&self, prefix: &str, origin_as: u32) -> RpkiStatus {
        let cache_key = format!("{}/{}", prefix, origin_as);

        // Check cache first (no semaphore needed for cache hits)
        if let Some(status) = self.cache.get(&cache_key) {
            return status;
        }

        // Acquire semaphore permit (max 100 concurrent requests)
        let _permit = self.semaphore.acquire().await;
        let permit = match _permit {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("Failed to acquire semaphore for RPKI validation: {}", e);
                return RpkiStatus::Unavailable;
            }
        };

        // Double-check cache after acquiring permit (another request might have filled it)
        if let Some(status) = self.cache.get(&cache_key) {
            drop(permit); // Release permit early
            return status;
        }

        // Build URL with prefix directly (Routinator expects unencoded prefix with "/")
        let url = format!(
            "{}/api/v1/validity/{}/{}",
            self.routinator_url, origin_as, prefix
        );

        // Make HTTP request with timeout
        let response = match self.client.get(&url).send().await {
            Ok(resp) => resp,
            Err(e) => {
                tracing::warn!("RPKI validation request failed for {}: {}", cache_key, e);
                let status = RpkiStatus::Unavailable;
                self.cache.insert(cache_key, status.clone());
                drop(permit); // Release permit
                return status;
            }
        };

        // Check for non-2xx status
        if !response.status().is_success() {
            tracing::warn!(
                "RPKI validation returned non-2xx status for {}: {}",
                cache_key,
                response.status()
            );
            let status = RpkiStatus::Unavailable;
            self.cache.insert(cache_key, status.clone());
            drop(permit); // Release permit
            return status;
        }

        // Parse JSON response
        let json_text = match response.text().await {
            Ok(text) => text,
            Err(e) => {
                tracing::warn!("Failed to read RPKI response body for {}: {}", cache_key, e);
                let status = RpkiStatus::Unavailable;
                self.cache.insert(cache_key, status.clone());
                drop(permit); // Release permit
                return status;
            }
        };

        // Deserialize response
        let result = match serde_json::from_str::<RoutinatorResponse>(&json_text) {
            Ok(resp) => resp,
            Err(e) => {
                tracing::warn!("Failed to parse RPKI JSON for {}: {}", cache_key, e);
                let status = RpkiStatus::Unavailable;
                self.cache.insert(cache_key, status.clone());
                drop(permit); // Release permit
                return status;
            }
        };

        // Map Routinator state to RpkiStatus
        let status = match result.validated_route.validity.state.as_str() {
            "valid" => RpkiStatus::Valid,
            "invalid" => {
                // Check reason to distinguish between ASN and length violations
                if let Some(reason) = &result.validated_route.validity.reason {
                    if reason.to_lowercase().contains("length") {
                        RpkiStatus::InvalidLength
                    } else {
                        // Default to InvalidAsn for any other invalid reason
                        RpkiStatus::InvalidAsn
                    }
                } else {
                    // No reason provided, default to InvalidAsn
                    RpkiStatus::InvalidAsn
                }
            }
            "not-found" => RpkiStatus::NotFound,
            _ => RpkiStatus::Unavailable,
        };

        // Cache the result
        self.cache.insert(cache_key, status.clone());
        drop(permit); // Release permit
        status
    }
}

// Internal structs for deserializing Routinator JSON response
#[derive(Debug, Deserialize)]
struct RoutinatorResponse {
    validated_route: ValidatedRoute,
}

#[derive(Debug, Deserialize)]
struct ValidatedRoute {
    validity: Validity,
}

#[derive(Debug, Deserialize)]
struct Validity {
    state: String,
    reason: Option<String>,
}

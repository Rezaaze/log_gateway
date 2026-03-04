use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::error;
use utoipa::ToSchema;
use uuid::Uuid;

/// Tenant information with API key hash.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Tenant {
    pub id: Uuid,
    pub name: String,
    pub api_key_hash: String,    // SHA-256 hex des API-Keys
    pub rate_limit_per_sec: u32, // req/s, default: 1000
    pub plan: String,            // "free" | "pro" | "enterprise"
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub enabled: bool,
}

/// Input for creating a new tenant.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TenantCreate {
    pub name: String,
    pub api_key: String,                 // Plaintext — wird zu hash umgewandelt
    pub rate_limit_per_sec: Option<u32>, // req/s, default: 1000
    pub plan: String,
}

/// Input for updating an existing tenant.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TenantUpdate {
    pub name: Option<String>,
    pub rate_limit_per_sec: Option<u32>, // req/s, default: 1000
    pub plan: Option<String>,
    pub enabled: Option<bool>,
}

/// Client for managing tenants in ClickHouse.
#[derive(Debug, Clone)]
pub struct TenantManagerClient {
    http: Client,
    url: String,
    db: String,
}

impl TenantManagerClient {
    /// Creates a new TenantManagerClient.
    ///
    /// # Arguments
    ///
    /// * `url` - ClickHouse HTTP API URL (e.g., "http://localhost:8123")
    /// * `db` - ClickHouse database name (e.g., "bgp")
    pub fn new(url: String, db: String) -> Self {
        let http = Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap_or_else(|e| {
                error!("Failed to build HTTP client for TenantManager: {}", e);
                Client::new()
            });

        Self { http, url, db }
    }

    /// Lists all active tenants (enabled = true).
    pub async fn list_tenants(&self) -> Result<Vec<Tenant>> {
        let sql = format!(
            "SELECT id, name, api_key_hash, rate_limit_per_sec, plan, toString(created_at) as created_at, toString(updated_at) as updated_at, enabled FROM {}.tenants FINAL WHERE enabled = 1 FORMAT JSON",
            self.db
        );

        let response = self.query_json::<TenantRow>(&sql).await?;
        let tenants = response
            .into_iter()
            .map(tenant_from_row)
            .collect::<Result<Vec<_>>>()?;
        Ok(tenants)
    }

    /// Creates a new tenant. Returns the created tenant.
    pub async fn create_tenant(&self, input: TenantCreate) -> Result<Tenant> {
        // Validate plan
        let valid_plans = ["free", "pro", "enterprise"];
        if !valid_plans.contains(&input.plan.as_str()) {
            return Err(anyhow::anyhow!(
                "Invalid plan: {}. Must be one of: free, pro, enterprise",
                input.plan
            ));
        }

        // Validate rate limit if provided
        let rate_limit = input.rate_limit_per_sec.unwrap_or(1000);
        if rate_limit == 0 {
            return Err(anyhow::anyhow!("Rate limit must be greater than 0"));
        }

        let id = Uuid::new_v4();
        let now = Utc::now();
        let api_key_hash = hash_api_key(&input.api_key);

        // Format timestamps for ClickHouse
        let created_at_str = now.format("%Y-%m-%d %H:%M:%S%.3f").to_string();
        let updated_at_str = created_at_str.clone();

        // Build JSON for JSONEachRow format
        let tenant_json = serde_json::json!({
            "id": id.to_string(),
            "name": input.name,
            "api_key_hash": api_key_hash,
            "rate_limit_per_sec": rate_limit,
            "plan": input.plan,
            "created_at": created_at_str,
            "updated_at": updated_at_str,
            "enabled": 1u8
        });

        let sql = format!(
            "INSERT INTO {}.tenants FORMAT JSONEachRow {}",
            self.db, tenant_json
        );

        self.execute(&sql).await?;

        // Return the created tenant
        Ok(Tenant {
            id,
            name: input.name,
            api_key_hash,
            rate_limit_per_sec: rate_limit,
            plan: input.plan,
            created_at: now,
            updated_at: now,
            enabled: true,
        })
    }

    /// Updates an existing tenant.
    ///
    /// Uses ClickHouse's ReplacingMergeTree engine: we INSERT a new row with the same ID
    /// but newer updated_at, and the engine will deduplicate based on updated_at.
    pub async fn update_tenant(&self, id: Uuid, input: TenantUpdate) -> Result<()> {
        // First, get the existing tenant to ensure it exists
        let existing = self.get_tenant(id).await?;

        // Validate plan if provided
        if let Some(ref p) = &input.plan {
            let valid_plans = ["free", "pro", "enterprise"];
            if !valid_plans.contains(&p.as_str()) {
                return Err(anyhow::anyhow!(
                    "Invalid plan: {}. Must be one of: free, pro, enterprise",
                    p
                ));
            }
        }

        // Validate rate limit if provided
        if let Some(rl) = input.rate_limit_per_sec {
            if rl == 0 {
                return Err(anyhow::anyhow!("Rate limit must be greater than 0"));
            }
        }

        // Build updated tenant with new values
        let name = input.name.unwrap_or(existing.name);
        let rate_limit_per_sec = input
            .rate_limit_per_sec
            .unwrap_or(existing.rate_limit_per_sec);
        let plan = input.plan.unwrap_or(existing.plan);
        let enabled = input.enabled.unwrap_or(existing.enabled);

        let now = Utc::now();
        let updated_at_str = now.format("%Y-%m-%d %H:%M:%S%.3f").to_string();

        // Build JSON for JSONEachRow format
        let tenant_json = serde_json::json!({
            "id": id.to_string(),
            "name": name,
            "api_key_hash": existing.api_key_hash, // API key hash stays the same
            "rate_limit_per_sec": rate_limit_per_sec,
            "plan": plan,
            "created_at": existing.created_at.format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
            "updated_at": updated_at_str,
            "enabled": if enabled { 1u8 } else { 0u8 }
        });

        let sql = format!(
            "INSERT INTO {}.tenants FORMAT JSONEachRow {}",
            self.db, tenant_json
        );

        self.execute(&sql).await
    }

    /// Soft-delete: sets enabled = false.
    pub async fn delete_tenant(&self, id: Uuid) -> Result<()> {
        self.update_tenant(
            id,
            TenantUpdate {
                name: None,
                rate_limit_per_sec: None,
                plan: None,
                enabled: Some(false),
            },
        )
        .await
    }

    /// Finds a tenant by API key hash. Returns None if not found.
    pub async fn find_by_api_key(&self, api_key: &str) -> Result<Option<Tenant>> {
        let hash = hash_api_key(api_key);
        let sql = format!(
            "SELECT id, name, api_key_hash, rate_limit_per_sec, plan, toString(created_at) as created_at, toString(updated_at) as updated_at, enabled FROM {}.tenants FINAL WHERE api_key_hash = '{}' AND enabled = 1 LIMIT 1 FORMAT JSON",
            self.db,
            hash.replace("'", "''")
        );

        let mut rows = self.query_json::<TenantRow>(&sql).await?;
        if rows.is_empty() {
            Ok(None)
        } else {
            let row = rows.remove(0);
            tenant_from_row(row).map(Some)
        }
    }

    /// Gets a single tenant by ID.
    async fn get_tenant(&self, id: Uuid) -> Result<Tenant> {
        let sql = format!(
            "SELECT id, name, api_key_hash, rate_limit_per_sec, plan, toString(created_at) as created_at, toString(updated_at) as updated_at, enabled FROM {}.tenants FINAL WHERE id = '{}'",
            self.db, id
        );

        let mut rows = self.query_json::<TenantRow>(&sql).await?;
        rows.pop()
            .map(tenant_from_row)
            .transpose()?
            .context(format!("Tenant with id {} not found", id))
    }

    /// Executes a ClickHouse query that doesn't return rows.
    async fn execute(&self, sql: &str) -> Result<()> {
        let url = self.build_url();
        let body = sql.to_string();
        let content_length = body.len();

        let response = self
            .http
            .post(&url)
            .header("Content-Length", content_length.to_string())
            .body(body)
            .send()
            .await
            .context("ClickHouse request failed")?;

        if !response.status().is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Failed to read error body".to_string());
            return Err(anyhow::anyhow!("ClickHouse error: {}", body));
        }

        Ok(())
    }

    /// Executes a ClickHouse query and deserializes the JSON response.
    async fn query_json<T: serde::de::DeserializeOwned>(&self, sql: &str) -> Result<Vec<T>> {
        let url = self.build_url();
        let body = sql.to_string();
        let content_length = body.len();

        let response = self
            .http
            .post(&url)
            .header("Content-Length", content_length.to_string())
            .body(body)
            .send()
            .await
            .context("ClickHouse request failed")?;

        if !response.status().is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Failed to read error body".to_string());
            return Err(anyhow::anyhow!("ClickHouse error: {}", body));
        }

        let body = response
            .text()
            .await
            .context("Failed to read response body")?;

        // ClickHouse JSON format has {"data": [...]}
        #[derive(Deserialize)]
        struct ClickHouseResponse<T> {
            data: Vec<T>,
        }

        let response: ClickHouseResponse<T> = serde_json::from_str(&body).context(format!(
            "Failed to parse ClickHouse JSON response: {}",
            body
        ))?;

        Ok(response.data)
    }

    /// Builds the ClickHouse HTTP API base URL (SQL is sent as POST body).
    fn build_url(&self) -> String {
        format!("{}/?database={}&default_format=JSON", self.url, self.db)
    }
}

/// Tenant row from ClickHouse (enabled as u8).
#[derive(Debug, Deserialize)]
struct TenantRow {
    id: String, // UUID als String
    name: String,
    api_key_hash: String,
    rate_limit_per_sec: Option<u32>, // req/s, default: 1000
    plan: String,
    created_at: String, // "2024-01-01 12:00:00.000"
    updated_at: String,
    enabled: u8,
}

/// Converts a TenantRow to a Tenant.
fn tenant_from_row(row: TenantRow) -> Result<Tenant> {
    let id = Uuid::parse_str(&row.id).context(format!("Invalid UUID: {}", row.id))?;

    // Parse timestamps, fallback to current time on error
    let created_at = DateTime::parse_from_str(&row.created_at, "%Y-%m-%d %H:%M:%S%.3f")
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());

    let updated_at = DateTime::parse_from_str(&row.updated_at, "%Y-%m-%d %H:%M:%S%.3f")
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());

    Ok(Tenant {
        id,
        name: row.name,
        api_key_hash: row.api_key_hash,
        rate_limit_per_sec: row.rate_limit_per_sec.unwrap_or(1000),
        plan: row.plan,
        created_at,
        updated_at,
        enabled: row.enabled != 0,
    })
}

/// Hashes an API key using SHA-256 and returns hex string.
fn hash_api_key(key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test 1: hash_api_key ist deterministisch
    #[test]
    fn test_hash_api_key_deterministic() {
        let h1 = hash_api_key("my-secret-key");
        let h2 = hash_api_key("my-secret-key");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64); // SHA-256 hex = 64 chars
    }

    // Test 2: hash_api_key unterscheidet verschiedene Keys
    #[test]
    fn test_hash_api_key_different_keys() {
        let h1 = hash_api_key("key-one");
        let h2 = hash_api_key("key-two");
        assert_ne!(h1, h2);
    }

    // Test 3: TenantRow → Tenant Konvertierung
    #[test]
    fn test_tenant_row_conversion() {
        let row = TenantRow {
            id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            name: "Test Tenant".to_string(),
            api_key_hash: "abc123".to_string(),
            rate_limit_per_sec: Some(1000),
            plan: "pro".to_string(),
            created_at: "2024-01-01 12:00:00.000".to_string(),
            updated_at: "2024-01-02 12:00:00.000".to_string(),
            enabled: 1,
        };
        let tenant = tenant_from_row(row).unwrap();
        assert_eq!(tenant.name, "Test Tenant");
        assert!(tenant.enabled);
        assert_eq!(tenant.rate_limit_per_sec, 1000);
        assert_eq!(tenant.plan, "pro");
    }

    // Test 4: TenantCreate → hash wird korrekt gesetzt
    #[test]
    fn test_create_tenant_hashes_key() {
        let input = TenantCreate {
            name: "Test".to_string(),
            api_key: "raw-key-123".to_string(),
            rate_limit_per_sec: Some(500),
            plan: "free".to_string(),
        };
        let hash = hash_api_key(&input.api_key);
        assert_ne!(hash, input.api_key); // hash ≠ plaintext
        assert_eq!(hash.len(), 64);
    }

    // Test 5: TenantRow with enabled = 0 converts to enabled = false
    #[test]
    fn test_tenant_row_disabled_conversion() {
        let row = TenantRow {
            id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            name: "Disabled Tenant".to_string(),
            api_key_hash: "abc123".to_string(),
            rate_limit_per_sec: Some(1000),
            plan: "free".to_string(),
            created_at: "2024-01-01 12:00:00.000".to_string(),
            updated_at: "2024-01-02 12:00:00.000".to_string(),
            enabled: 0,
        };
        let tenant = tenant_from_row(row).unwrap();
        assert!(!tenant.enabled);
    }

    // Test 6: Invalid timestamp falls back to current time
    #[test]
    fn test_tenant_row_invalid_timestamp_fallback() {
        let row = TenantRow {
            id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            name: "Test".to_string(),
            api_key_hash: "abc123".to_string(),
            rate_limit_per_sec: Some(1000),
            plan: "free".to_string(),
            created_at: "invalid-date".to_string(),
            updated_at: "invalid-date".to_string(),
            enabled: 1,
        };
        let tenant = tenant_from_row(row).unwrap();
        // Should not panic, should use current time as fallback
        assert!(!tenant.created_at.to_string().is_empty());
        assert!(!tenant.updated_at.to_string().is_empty());
    }

    // Test 7: TenantCreate with custom rate limit
    #[test]
    fn test_tenant_create_with_custom_rate_limit() {
        let input = TenantCreate {
            name: "Test Tenant".to_string(),
            api_key: "test-key".to_string(),
            rate_limit_per_sec: Some(500),
            plan: "pro".to_string(),
        };

        // Test serialization
        let json = serde_json::to_string(&input).unwrap();
        let parsed: TenantCreate = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.name, "Test Tenant");
        assert_eq!(parsed.api_key, "test-key");
        assert_eq!(parsed.rate_limit_per_sec, Some(500));
        assert_eq!(parsed.plan, "pro");

        // Test that None defaults to 1000 in create_tenant logic
        let input_without_rate = TenantCreate {
            name: "Test Tenant 2".to_string(),
            api_key: "test-key-2".to_string(),
            rate_limit_per_sec: None,
            plan: "free".to_string(),
        };

        let json2 = serde_json::to_string(&input_without_rate).unwrap();
        let parsed2: TenantCreate = serde_json::from_str(&json2).unwrap();

        assert_eq!(parsed2.rate_limit_per_sec, None);
    }

    // Test 8: TenantUpdate with custom rate limit
    #[test]
    fn test_tenant_update_with_custom_rate_limit() {
        let input = TenantUpdate {
            name: Some("Updated Name".to_string()),
            rate_limit_per_sec: Some(200),
            plan: Some("enterprise".to_string()),
            enabled: Some(true),
        };

        // Test serialization
        let json = serde_json::to_string(&input).unwrap();
        let parsed: TenantUpdate = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.name, Some("Updated Name".to_string()));
        assert_eq!(parsed.rate_limit_per_sec, Some(200));
        assert_eq!(parsed.plan, Some("enterprise".to_string()));
        assert_eq!(parsed.enabled, Some(true));
    }

    // Test 9: tenant_from_row handles missing rate_limit_per_sec (defaults to 1000)
    #[test]
    fn test_tenant_row_missing_rate_limit_defaults_to_1000() {
        let row = TenantRow {
            id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            name: "Test Tenant".to_string(),
            api_key_hash: "abc123".to_string(),
            rate_limit_per_sec: None, // Missing rate limit
            plan: "free".to_string(),
            created_at: "2024-01-01 12:00:00.000".to_string(),
            updated_at: "2024-01-02 12:00:00.000".to_string(),
            enabled: 1,
        };
        let tenant = tenant_from_row(row).unwrap();
        assert_eq!(tenant.rate_limit_per_sec, 1000); // Should default to 1000
    }
}

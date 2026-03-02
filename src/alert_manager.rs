use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::error;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::anomaly_detector::Anomaly;

/// Alert rule for BGP anomaly detection.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AlertRule {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub rule_type: String, // "hijack" | "flap" | "leak" | "custom"
    pub threshold: f64,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Input for creating a new alert rule.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AlertRuleCreate {
    pub name: String,
    pub description: String,
    pub rule_type: String,
    pub threshold: f64,
}

/// Input for updating an existing alert rule.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AlertRuleUpdate {
    pub name: Option<String>,
    pub description: Option<String>,
    pub threshold: Option<f64>,
    pub enabled: Option<bool>,
}

/// Alert history entry representing a fired alert.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AlertHistoryEntry {
    pub id: Uuid,
    pub rule_id: Uuid,
    pub alert_type: String,
    pub prefix: String,
    pub origin_as: u32,
    pub confidence: f64,
    pub status: String, // "fired" | "resolved"
    pub fired_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

/// Client for managing alert rules and alert history in ClickHouse.
#[derive(Debug, Clone)]
pub struct AlertManagerClient {
    http: Client,
    url: String,
    database: String,
}

impl AlertManagerClient {
    /// Creates a new AlertManagerClient.
    ///
    /// # Arguments
    ///
    /// * `url` - ClickHouse HTTP API URL (e.g., "http://localhost:8123")
    /// * `database` - ClickHouse database name (e.g., "bgp")
    pub fn new(url: String, database: String) -> Self {
        let http = Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .pool_max_idle_per_host(5)
            .build()
            .unwrap_or_else(|e| {
                error!("Failed to build HTTP client for AlertManager: {}", e);
                Client::new()
            });

        Self {
            http,
            url,
            database,
        }
    }

    /// Lists all enabled alert rules.
    pub async fn list_rules(&self) -> Result<Vec<AlertRule>> {
        let sql = format!(
            "SELECT * FROM {}.alert_rules WHERE enabled = true FINAL ORDER BY created_at DESC",
            self.database
        );

        self.query_json::<AlertRule>(&sql).await
    }

    /// Creates a new alert rule.
    pub async fn create_rule(&self, input: AlertRuleCreate) -> Result<AlertRule> {
        // Validate rule_type
        let valid_types = ["hijack", "flap", "leak", "custom"];
        if !valid_types.contains(&input.rule_type.as_str()) {
            return Err(anyhow::anyhow!(
                "Invalid rule_type: {}. Must be one of: hijack, flap, leak, custom",
                input.rule_type
            ));
        }

        // Validate threshold
        if input.threshold < 0.0 || input.threshold > 1.0 {
            return Err(anyhow::anyhow!(
                "Threshold must be between 0.0 and 1.0, got {}",
                input.threshold
            ));
        }

        let sql = format!(
            "INSERT INTO {}.alert_rules (name, description, rule_type, threshold) \
             VALUES ('{}', '{}', '{}', {})",
            self.database,
            input.name.replace("'", "''"),
            input.description.replace("'", "''"),
            input.rule_type.replace("'", "''"),
            input.threshold
        );

        self.execute(&sql).await?;

        // Get the newly created rule by selecting the latest with this name
        let sql = format!(
            "SELECT * FROM {}.alert_rules WHERE name = '{}' FINAL ORDER BY updated_at DESC LIMIT 1",
            self.database,
            input.name.replace("'", "''")
        );

        let mut rules = self.query_json::<AlertRule>(&sql).await?;
        rules.pop().context("Failed to retrieve created alert rule")
    }

    /// Updates an existing alert rule.
    ///
    /// Uses ClickHouse's ReplacingMergeTree engine: we INSERT a new row with the same ID
    /// but newer updated_at, and the engine will deduplicate based on updated_at.
    pub async fn update_rule(&self, id: Uuid, input: AlertRuleUpdate) -> Result<AlertRule> {
        // First, get the existing rule to ensure it exists
        let existing = self.get_rule(id).await?;

        // Build UPDATE query as INSERT with new values
        let name = input.name.unwrap_or(existing.name);
        let description = input.description.unwrap_or(existing.description);
        let threshold = input.threshold.unwrap_or(existing.threshold);
        let enabled = input.enabled.unwrap_or(existing.enabled);

        // Validate threshold if provided
        if let Some(t) = input.threshold {
            if t < 0.0 || t > 1.0 {
                return Err(anyhow::anyhow!(
                    "Threshold must be between 0.0 and 1.0, got {}",
                    t
                ));
            }
        }

        let sql = format!(
            "INSERT INTO {}.alert_rules (id, name, description, rule_type, threshold, enabled) \
             VALUES ('{}', '{}', '{}', '{}', {}, {})",
            self.database,
            id,
            name.replace("'", "''"),
            description.replace("'", "''"),
            existing.rule_type.replace("'", "''"),
            threshold,
            enabled
        );

        self.execute(&sql).await?;

        // Get the updated rule
        self.get_rule(id).await
    }

    /// "Deletes" an alert rule by setting enabled = false.
    ///
    /// We don't physically delete rows in ClickHouse; we mark them as disabled.
    pub async fn delete_rule(&self, id: Uuid) -> Result<()> {
        let sql = format!(
            "INSERT INTO {}.alert_rules (id, name, description, rule_type, threshold, enabled) \
             SELECT id, name, description, rule_type, threshold, false \
             FROM {}.alert_rules FINAL WHERE id = '{}'",
            self.database, self.database, id
        );

        self.execute(&sql).await
    }

    /// Lists currently active alerts (status = 'fired' and not resolved).
    pub async fn list_active_alerts(&self) -> Result<Vec<AlertHistoryEntry>> {
        let sql = format!(
            "SELECT * FROM {}.alert_history WHERE status = 'fired' AND resolved_at IS NULL ORDER BY fired_at DESC",
            self.database
        );

        self.query_json::<AlertHistoryEntry>(&sql).await
    }

    /// Persists an anomaly as an alert in the history table.
    ///
    /// # Arguments
    ///
    /// * `anomaly` - The detected anomaly
    /// * `rule_id` - Optional rule ID that triggered this alert
    pub async fn persist_alert(&self, anomaly: &Anomaly, rule_id: Option<Uuid>) -> Result<()> {
        let alert_type = match anomaly.anomaly_type {
            crate::anomaly_detector::AnomalyType::PossibleHijack => "hijack",
            crate::anomaly_detector::AnomalyType::PrefixFlapping => "flap",
        };

        let sql = format!(
            "INSERT INTO {}.alert_history (rule_id, alert_type, prefix, origin_as, confidence) \
             VALUES ({}, '{}', '{}', {}, {})",
            self.database,
            match rule_id {
                Some(id) => format!("'{}'", id),
                None => "NULL".to_string(),
            },
            alert_type,
            anomaly.prefix.replace("'", "''"),
            anomaly.origin_as,
            anomaly.confidence
        );

        self.execute(&sql).await
    }

    /// Resolves an alert by updating its status and setting resolved_at.
    ///
    /// Uses ClickHouse's ALTER TABLE UPDATE mutation.
    pub async fn resolve_alert(&self, alert_id: Uuid) -> Result<()> {
        let sql = format!(
            "ALTER TABLE {}.alert_history UPDATE status = 'resolved', resolved_at = now64(3) \
             WHERE id = '{}'",
            self.database, alert_id
        );

        self.execute(&sql).await
    }

    /// Gets a single alert rule by ID.
    async fn get_rule(&self, id: Uuid) -> Result<AlertRule> {
        let sql = format!(
            "SELECT * FROM {}.alert_rules WHERE id = '{}' FINAL",
            self.database, id
        );

        let mut rules = self.query_json::<AlertRule>(&sql).await?;
        rules
            .pop()
            .context(format!("Alert rule with id {} not found", id))
    }

    /// Executes a ClickHouse query that doesn't return rows.
    async fn execute(&self, sql: &str) -> Result<()> {
        let url = self.build_url(sql);

        let response = self
            .http
            .post(&url)
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

    /// Executes a ClickHouse query and deserializes the JSONEachRow response.
    async fn query_json<T: serde::de::DeserializeOwned>(&self, sql: &str) -> Result<Vec<T>> {
        let url = self.build_url(sql);

        let response = self
            .http
            .post(&url)
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

        // Parse newline-delimited JSON (JSONEachRow format)
        let mut results = Vec::new();
        for line in body.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let value: T = serde_json::from_str(line)
                .context(format!("Failed to parse JSON line: {}", line))?;
            results.push(value);
        }

        Ok(results)
    }

    /// Builds the ClickHouse HTTP API URL with URL-encoded query.
    fn build_url(&self, sql: &str) -> String {
        let encoded_query = sql
            .replace('%', "%25") // must be first
            .replace(' ', "%20")
            .replace('\n', "%0A")
            .replace('\t', "%09")
            .replace('\'', "%27")
            .replace('(', "%28")
            .replace(')', "%29")
            .replace('=', "%3D")
            .replace(',', "%2C");
        format!(
            "{}/?database={}&default_format=JSONEachRow&query={}",
            self.url, self.database, encoded_query
        )
    }
}

/// Input for creating a new silence.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SilenceCreate {
    pub fingerprint: String,
    pub reason: String,
    pub silenced_by: String,
    pub duration_hours: u32, // Wie lange schweigen (1–168 Stunden)
}

/// A silence entry for suppressing alerts.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Silence {
    pub id: Uuid,
    pub fingerprint: String,
    pub reason: String,
    pub silenced_by: String,
    pub silenced_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub active: bool,
}

impl AlertManagerClient {
    /// Creates a new silence for suppressing alerts.
    ///
    /// # Arguments
    ///
    /// * `input` - The silence creation parameters
    ///
    /// # Returns
    ///
    /// The created silence entry
    pub async fn create_silence(&self, input: SilenceCreate) -> Result<Silence> {
        // Validierung: duration_hours zwischen 1 und 168
        if input.duration_hours < 1 || input.duration_hours > 168 {
            return Err(anyhow::anyhow!(
                "Duration must be between 1 and 168 hours, got {}",
                input.duration_hours
            ));
        }

        // Validierung: reason nicht leer
        if input.reason.trim().is_empty() {
            return Err(anyhow::anyhow!("Reason cannot be empty"));
        }

        // Berechne expires_at
        let expires_at = Utc::now() + chrono::Duration::hours(input.duration_hours as i64);

        let sql = format!(
            "INSERT INTO {}.alert_silences (fingerprint, reason, silenced_by, expires_at) \
             VALUES ('{}', '{}', '{}', '{}')",
            self.database,
            input.fingerprint.replace("'", "''"),
            input.reason.replace("'", "''"),
            input.silenced_by.replace("'", "''"),
            expires_at.format("%Y-%m-%d %H:%M:%S")
        );

        self.execute(&sql).await?;

        // Get the newly created silence by selecting the latest with this fingerprint
        let sql = format!(
            "SELECT * FROM {}.alert_silences WHERE fingerprint = '{}' FINAL ORDER BY silenced_at DESC LIMIT 1",
            self.database,
            input.fingerprint.replace("'", "''")
        );

        let mut silences = self.query_json::<Silence>(&sql).await?;
        silences.pop().context("Failed to retrieve created silence")
    }

    /// Lists all active silences (active = true and expires_at > now()).
    pub async fn list_silences(&self) -> Result<Vec<Silence>> {
        let sql = format!(
            "SELECT * FROM {}.alert_silences WHERE active = true AND expires_at > now() FINAL ORDER BY silenced_at DESC",
            self.database
        );

        self.query_json::<Silence>(&sql).await
    }

    /// Checks if a fingerprint is currently silenced.
    ///
    /// # Arguments
    ///
    /// * `fingerprint` - The fingerprint to check
    ///
    /// # Returns
    ///
    /// `true` if the fingerprint is silenced, `false` otherwise
    pub async fn is_silenced(&self, fingerprint: &str) -> Result<bool> {
        let sql = format!(
            "SELECT count() as cnt FROM {}.alert_silences WHERE fingerprint = '{}' \
             AND active = true AND expires_at > now() FINAL",
            self.database,
            fingerprint.replace("'", "''")
        );

        let url = self.build_url(&sql);
        let response = self
            .http
            .post(&url)
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

        // Parse the count result
        for line in body.lines() {
            if line.trim().is_empty() {
                continue;
            }
            // ClickHouse returns JSON like {"cnt": 0} or {"cnt": 1}
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
                if let Some(cnt) = value.get("cnt").and_then(|v| v.as_u64()) {
                    return Ok(cnt > 0);
                }
            }
        }

        Ok(false)
    }

    /// Expires a silence by setting active = false.
    ///
    /// Uses ClickHouse's ReplacingMergeTree engine: we INSERT a new row with the same ID
    /// but newer silenced_at and active = false.
    ///
    /// # Arguments
    ///
    /// * `id` - The silence ID to expire
    pub async fn expire_silence(&self, id: Uuid) -> Result<()> {
        // First, get the existing silence to ensure it exists
        let sql = format!(
            "SELECT * FROM {}.alert_silences WHERE id = '{}' FINAL",
            self.database, id
        );

        let mut silences = self.query_json::<Silence>(&sql).await?;
        let existing = silences
            .pop()
            .context(format!("Silence with id {} not found", id))?;

        // Insert new row with active = false
        let sql = format!(
            "INSERT INTO {}.alert_silences (id, fingerprint, reason, silenced_by, silenced_at, expires_at, active) \
             VALUES ('{}', '{}', '{}', '{}', '{}', '{}', false)",
            self.database,
            id,
            existing.fingerprint.replace("'", "''"),
            existing.reason.replace("'", "''"),
            existing.silenced_by.replace("'", "''"),
            existing.silenced_at.format("%Y-%m-%d %H:%M:%S"),
            existing.expires_at.format("%Y-%m-%d %H:%M:%S")
        );

        self.execute(&sql).await
    }
}

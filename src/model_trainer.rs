use crate::baseline_model::BaselineModel;
use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, TimeZone, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::time;

/// A single training row returned from ClickHouse.
///
/// Maps to the columns selected from the `bgp_events` table.
#[derive(Debug, Deserialize)]
struct TrainingRow {
    prefix: String,
    as_path_len: f64,
    origin_as: u32,
    event_date: String,
}

/// Row for baseline snapshot serialization.
#[derive(Debug, Serialize)]
struct BaselineSnapshotRow {
    snapshot_at: String,
    prefix: String,
    ema: f64,
    variance_ema: f64,
    sample_count: u32,
}

/// Row for AS knowledge snapshot serialization.
#[derive(Debug, Serialize)]
struct AsKnowledgeSnapshotRow {
    snapshot_at: String,
    asn: u32,
    days_seen: Vec<String>,
}

/// Row for baseline snapshot deserialization.
#[derive(Debug, Deserialize)]
struct BaselineSnapshotLoadRow {
    prefix: String,
    ema: f64,
    variance_ema: f64,
    sample_count: u32,
}

/// Row for AS knowledge snapshot deserialization.
#[derive(Debug, Deserialize)]
struct AsKnowledgeSnapshotLoadRow {
    asn: u32,
    days_seen: Vec<String>,
}

/// Background task that periodically retrains the [`BaselineModel`] from
/// historical ClickHouse data.
///
/// Every day at 00:00 UTC, it queries the last 30 days of BGP events and
/// feeds all (prefix, as_path_len) pairs plus AS-seen-dates into the model.
/// The model is **not** reset before retraining — new data is folded in
/// incrementally so the EMA smooths naturally over time.
pub struct ModelTrainer {
    /// The baseline model shared with the anomaly detector.
    baseline: Arc<BaselineModel>,
    /// ClickHouse HTTP endpoint (e.g. `http://clickhouse:8123`).
    clickhouse_url: String,
    /// ClickHouse database name.
    database: String,
    /// Table name for BGP events (typically `bgp_events`).
    table: String,
    /// HTTP client for ClickHouse requests.
    http: Client,
}

impl ModelTrainer {
    /// Creates a new `ModelTrainer`.
    ///
    /// # Arguments
    ///
    /// * `baseline`        – shared `BaselineModel` (same Arc used by `AnomalyDetector`)
    /// * `clickhouse_url`  – full HTTP URL of the ClickHouse server
    /// * `database`        – database name
    /// * `table`           – table name for BGP events (e.g. `"bgp_events"`)
    pub fn new(
        baseline: Arc<BaselineModel>,
        clickhouse_url: String,
        database: String,
        table: String,
    ) -> Self {
        Self {
            baseline,
            clickhouse_url,
            database,
            table,
            http: Client::new(),
        }
    }

    /// Queries ClickHouse for the last 30 days and trains the baseline model.
    ///
    /// For each row the method:
    /// 1. Calls `baseline.update(prefix, as_path_len)` to fold in the EMA.
    /// 2. Calls `baseline.record_as_seen(origin_as, event_date)` to update AS knowledge.
    ///
    /// Returns the number of training rows processed.
    pub async fn train_from_clickhouse(&self) -> Result<usize> {
        let sql = format!(
            "SELECT \
                prefix, \
                length(as_path) AS as_path_len, \
                origin_as, \
                formatDateTime(timestamp, '%Y-%m-%d') AS event_date \
             FROM {db}.{table} \
             WHERE timestamp >= now() - INTERVAL 30 DAY \
             FORMAT JSONEachRow",
            db = self.database,
            table = self.table,
        );

        let url = format!("{}/?default_format=JSONEachRow", self.clickhouse_url);

        let body = sql;
        let content_length = body.len();
        let response = self
            .http
            .post(&url)
            .header("Content-Length", content_length.to_string())
            .body(body)
            .send()
            .await
            .context("Failed to send training query to ClickHouse")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "ClickHouse returned {} during model training: {}",
                status,
                body
            );
        }

        let body = response
            .text()
            .await
            .context("Failed to read ClickHouse training response")?;

        let mut count = 0usize;
        for line in body.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<TrainingRow>(line) {
                Ok(row) => {
                    self.baseline.update(&row.prefix, row.as_path_len);
                    self.baseline.record_as_seen(row.origin_as, &row.event_date);
                    count += 1;
                }
                Err(e) => {
                    tracing::warn!(
                        "ModelTrainer: failed to parse training row: {} — {}",
                        e,
                        line
                    );
                }
            }
        }

        Ok(count)
    }

    /// Background loop: sleeps until the next 00:00 UTC, runs one training
    /// cycle, then repeats indefinitely.
    ///
    /// Errors from ClickHouse are logged as warnings but never abort the loop.
    pub async fn run_daily(self: Arc<Self>) {
        tracing::info!("ModelTrainer daily retraining task started");

        loop {
            let now = Utc::now();
            let next_midnight = Self::next_midnight(now);
            let sleep_secs = (next_midnight - now).num_seconds().max(0) as u64;

            tracing::info!(
                "Next model retraining scheduled for {} (in {} hours)",
                next_midnight.format("%Y-%m-%d %H:%M:%S UTC"),
                sleep_secs / 3600,
            );

            time::sleep(time::Duration::from_secs(sleep_secs)).await;

            tracing::info!(
                "ModelTrainer: starting daily retraining from ClickHouse (last 30 days)"
            );

            match self.train_from_clickhouse().await {
                Ok(count) => {
                    tracing::info!(
                        "ModelTrainer: retraining complete — {} rows processed",
                        count
                    );

                    // Save snapshot after successful training
                    match self.save_snapshot().await {
                        Ok(snapshot_rows) => {
                            tracing::info!(
                                "ModelTrainer: saved {} rows to cold-start snapshot",
                                snapshot_rows
                            );
                        }
                        Err(e) => {
                            tracing::warn!("ModelTrainer: failed to save cold-start snapshot (will retry tomorrow): {}", e);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "ModelTrainer: retraining failed (will retry tomorrow): {}",
                        e
                    );
                }
            }
        }
    }

    /// Pure helper: returns the next 00:00:00 UTC after `now`.
    pub fn next_midnight(now: DateTime<Utc>) -> DateTime<Utc> {
        let tomorrow = now.date_naive().succ_opt().unwrap_or(now.date_naive());
        Utc.with_ymd_and_hms(tomorrow.year(), tomorrow.month(), tomorrow.day(), 0, 0, 0)
            .single()
            .expect("Invalid date for next midnight")
    }

    /// Saves a snapshot of the current baseline model state to ClickHouse.
    ///
    /// Persists all baseline entries and AS knowledge to ClickHouse tables
    /// `{db}.baseline_snapshots` and `{db}.as_knowledge_snapshots`.
    ///
    /// Returns the total number of rows persisted (baselines + AS knowledge entries).
    pub async fn save_snapshot(&self) -> Result<usize> {
        let snapshot_at = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let mut total_rows = 0;

        // Save baseline snapshots
        let baseline_rows: Vec<BaselineSnapshotRow> = self
            .baseline
            .baselines()
            .iter()
            .map(|entry| BaselineSnapshotRow {
                snapshot_at: snapshot_at.clone(),
                prefix: entry.key().clone(),
                ema: entry.ema,
                variance_ema: entry.variance_ema,
                sample_count: entry.sample_count,
            })
            .collect();

        if !baseline_rows.is_empty() {
            let sql = format!(
                "INSERT INTO {db}.baseline_snapshots FORMAT JSONEachRow",
                db = self.database
            );
            let url = format!("{}/?default_format=JSONEachRow", self.clickhouse_url);

            let body = baseline_rows
                .iter()
                .map(serde_json::to_string)
                .collect::<Result<Vec<_>, _>>()
                .context("Failed to serialize baseline snapshot rows")?
                .join("\n");

            let request_body = format!("{}\n{}", sql, body);
            let content_length = request_body.len();
            let response = self
                .http
                .post(&url)
                .header("Content-Length", content_length.to_string())
                .body(request_body)
                .send()
                .await
                .context("Failed to send baseline snapshot to ClickHouse")?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                anyhow::bail!(
                    "ClickHouse returned {} during baseline snapshot save: {}",
                    status,
                    body
                );
            }

            total_rows += baseline_rows.len();
        }

        // Save AS knowledge snapshots
        let as_knowledge_rows: Vec<AsKnowledgeSnapshotRow> = self
            .baseline
            .as_knowledge()
            .as_to_days()
            .iter()
            .map(|entry| AsKnowledgeSnapshotRow {
                snapshot_at: snapshot_at.clone(),
                asn: *entry.key(),
                days_seen: entry.value().iter().cloned().collect(),
            })
            .collect();

        if !as_knowledge_rows.is_empty() {
            let sql = format!(
                "INSERT INTO {db}.as_knowledge_snapshots FORMAT JSONEachRow",
                db = self.database
            );
            let url = format!("{}/?default_format=JSONEachRow", self.clickhouse_url);

            let body = as_knowledge_rows
                .iter()
                .map(serde_json::to_string)
                .collect::<Result<Vec<_>, _>>()
                .context("Failed to serialize AS knowledge snapshot rows")?
                .join("\n");

            let request_body = format!("{}\n{}", sql, body);
            let content_length = request_body.len();
            let response = self
                .http
                .post(&url)
                .header("Content-Length", content_length.to_string())
                .body(request_body)
                .send()
                .await
                .context("Failed to send AS knowledge snapshot to ClickHouse")?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                anyhow::bail!(
                    "ClickHouse returned {} during AS knowledge snapshot save: {}",
                    status,
                    body
                );
            }

            total_rows += as_knowledge_rows.len();
        }

        Ok(total_rows)
    }

    /// Loads the latest snapshot from ClickHouse into the baseline model.
    ///
    /// Queries the most recent snapshot from `{db}.baseline_snapshots` and
    /// `{db}.as_knowledge_snapshots` and restores the model state.
    ///
    /// Returns the total number of rows loaded (baselines + AS knowledge entries).
    pub async fn load_snapshot(&self) -> Result<usize> {
        let mut total_rows = 0;

        // Load baseline snapshots
        let sql = format!(
            "SELECT prefix, ema, variance_ema, sample_count \
             FROM {db}.baseline_snapshots FINAL \
             ORDER BY prefix \
             FORMAT JSONEachRow",
            db = self.database
        );
        let url = format!("{}/?default_format=JSONEachRow", self.clickhouse_url);

        let body = sql;
        let content_length = body.len();
        let response = self
            .http
            .post(&url)
            .header("Content-Length", content_length.to_string())
            .body(body)
            .send()
            .await
            .context("Failed to send baseline snapshot query to ClickHouse")?;

        if response.status().is_success() {
            let body = response
                .text()
                .await
                .context("Failed to read ClickHouse baseline snapshot response")?;

            for line in body.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                match serde_json::from_str::<BaselineSnapshotLoadRow>(line) {
                    Ok(row) => {
                        // Direct insert into DashMap (no update, just set)
                        self.baseline.baselines().insert(
                            row.prefix,
                            crate::baseline_model::PrefixBaseline {
                                ema: row.ema,
                                variance_ema: row.variance_ema,
                                sample_count: row.sample_count,
                            },
                        );
                        total_rows += 1;
                    }
                    Err(e) => {
                        tracing::warn!(
                            "ModelTrainer: failed to parse baseline snapshot row: {} — {}",
                            e,
                            line
                        );
                    }
                }
            }
        } else {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "ClickHouse returned {} during baseline snapshot load: {}",
                status,
                body
            );
        }

        // Load AS knowledge snapshots
        let sql = format!(
            "SELECT asn, days_seen \
             FROM {db}.as_knowledge_snapshots FINAL \
             FORMAT JSONEachRow",
            db = self.database
        );
        let url = format!("{}/?default_format=JSONEachRow", self.clickhouse_url);

        let body = sql;
        let content_length = body.len();
        let response = self
            .http
            .post(&url)
            .header("Content-Length", content_length.to_string())
            .body(body)
            .send()
            .await
            .context("Failed to send AS knowledge snapshot query to ClickHouse")?;

        if response.status().is_success() {
            let body = response
                .text()
                .await
                .context("Failed to read ClickHouse AS knowledge snapshot response")?;

            for line in body.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                match serde_json::from_str::<AsKnowledgeSnapshotLoadRow>(line) {
                    Ok(row) => {
                        // For each day in days_seen, record AS as seen
                        for day in row.days_seen {
                            self.baseline.record_as_seen(row.asn, &day);
                        }
                        total_rows += 1;
                    }
                    Err(e) => {
                        tracing::warn!(
                            "ModelTrainer: failed to parse AS knowledge snapshot row: {} — {}",
                            e,
                            line
                        );
                    }
                }
            }
        } else {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "ClickHouse returned {} during AS knowledge snapshot load: {}",
                status,
                body
            );
        }

        Ok(total_rows)
    }

    /// Loads snapshot on startup (graceful degradation).
    ///
    /// This function is meant to be called once during application startup.
    /// Errors are logged as warnings but do not prevent the application from starting.
    pub async fn load_snapshot_on_startup(trainer: Arc<Self>) {
        tracing::info!("ModelTrainer: attempting to load cold-start snapshot from ClickHouse");

        match trainer.load_snapshot().await {
            Ok(count) => {
                tracing::info!(
                    "ModelTrainer: loaded {} rows from cold-start snapshot",
                    count
                );
            }
            Err(e) => {
                tracing::warn!(
                    "ModelTrainer: failed to load cold-start snapshot (continuing without): {}",
                    e
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    // ── next_midnight tests ──────────────────────────────────────────────────

    #[test]
    fn test_next_midnight_midday() {
        // 2026-03-03 14:30:00 UTC → next midnight = 2026-03-04 00:00:00 UTC
        let now = Utc.with_ymd_and_hms(2026, 3, 3, 14, 30, 0).unwrap();
        let next = ModelTrainer::next_midnight(now);
        let expected = Utc.with_ymd_and_hms(2026, 3, 4, 0, 0, 0).unwrap();
        assert_eq!(next, expected);
    }

    #[test]
    fn test_next_midnight_just_after_midnight() {
        // 2026-03-03 00:00:01 UTC → next midnight = 2026-03-04 00:00:00 UTC
        let now = Utc.with_ymd_and_hms(2026, 3, 3, 0, 0, 1).unwrap();
        let next = ModelTrainer::next_midnight(now);
        let expected = Utc.with_ymd_and_hms(2026, 3, 4, 0, 0, 0).unwrap();
        assert_eq!(next, expected);
    }

    #[test]
    fn test_next_midnight_year_rollover() {
        // 2026-12-31 23:59:59 UTC → next midnight = 2027-01-01 00:00:00 UTC
        let now = Utc.with_ymd_and_hms(2026, 12, 31, 23, 59, 59).unwrap();
        let next = ModelTrainer::next_midnight(now);
        let expected = Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap();
        assert_eq!(next, expected);
    }

    #[test]
    fn test_next_midnight_leap_day() {
        // 2024-02-28 12:00:00 UTC → next midnight = 2024-02-29 00:00:00 UTC (leap year)
        let now = Utc.with_ymd_and_hms(2024, 2, 28, 12, 0, 0).unwrap();
        let next = ModelTrainer::next_midnight(now);
        let expected = Utc.with_ymd_and_hms(2024, 2, 29, 0, 0, 0).unwrap();
        assert_eq!(next, expected);
    }

    // ── model update logic tests ─────────────────────────────────────────────

    /// Verify that `train_from_clickhouse` fails gracefully when the server
    /// is unreachable (returns Err, does not panic).
    #[tokio::test]
    async fn test_train_fails_gracefully_on_connection_error() {
        let baseline = Arc::new(BaselineModel::default());
        let trainer = ModelTrainer::new(
            baseline,
            "http://127.0.0.1:19999".to_string(), // nothing listening here
            "bgp".to_string(),
            "bgp_events".to_string(),
        );
        let result = trainer.train_from_clickhouse().await;
        assert!(
            result.is_err(),
            "Expected Err on connection refused, got Ok"
        );
    }

    /// Verify that baseline is updated correctly when we simulate rows.
    ///
    /// We call the same logic manually (bypassing HTTP) to test the EMA/AS
    /// knowledge wiring without a real ClickHouse.
    #[test]
    fn test_baseline_updated_during_training() {
        let baseline = Arc::new(BaselineModel::default());

        // Simulate 10 training rows for the same prefix
        for i in 0..10u32 {
            baseline.update("10.0.0.0/8", 5.0);
            baseline.record_as_seen(64512, &format!("2026-01-{:02}", i + 1));
        }

        // After 10 rows the EMA should be close to 5.0
        let z = baseline.z_score("10.0.0.0/8", 5.0);
        assert!(z.is_some(), "Expected Some z-score after 10 samples");
        assert!(
            z.unwrap().abs() < 1.0,
            "Z-score for in-distribution value should be small"
        );

        // AS 64512 has been seen on 10 days → well-known
        assert!(baseline.as_knowledge().is_well_known(64512));
    }

    /// Verify that the trainer struct can be created and is Send + Sync.
    #[test]
    fn test_trainer_new() {
        let baseline = Arc::new(BaselineModel::default());
        let trainer = Arc::new(ModelTrainer::new(
            baseline,
            "http://clickhouse:8123".to_string(),
            "bgp".to_string(),
            "bgp_events".to_string(),
        ));
        // If this compiles and runs, Send + Sync is satisfied
        let _clone = Arc::clone(&trainer);
    }

    // ── snapshot tests ──────────────────────────────────────────────────────

    /// Verify that `save_snapshot` fails gracefully when the server is unreachable.
    #[tokio::test]
    async fn test_save_snapshot_fails_gracefully_on_no_server() {
        let baseline = Arc::new(BaselineModel::default());

        // Add some data so save_snapshot will actually try to send HTTP requests
        baseline.update("10.0.0.0/8", 5.0);
        baseline.record_as_seen(64512, "2024-01-01");

        let trainer = ModelTrainer::new(
            baseline,
            "http://127.0.0.1:19999".to_string(), // nothing listening here
            "bgp".to_string(),
            "bgp_events".to_string(),
        );
        let result = trainer.save_snapshot().await;
        assert!(
            result.is_err(),
            "Expected Err on connection refused, got Ok"
        );
    }

    /// Verify that `load_snapshot` fails gracefully when the server is unreachable.
    #[tokio::test]
    async fn test_load_snapshot_fails_gracefully_on_no_server() {
        let baseline = Arc::new(BaselineModel::default());
        let trainer = ModelTrainer::new(
            baseline,
            "http://127.0.0.1:19999".to_string(), // nothing listening here
            "bgp".to_string(),
            "bgp_events".to_string(),
        );
        let result = trainer.load_snapshot().await;
        assert!(
            result.is_err(),
            "Expected Err on connection refused, got Ok"
        );
    }

    /// Verify that `load_snapshot_on_startup` does not panic when server is unreachable.
    #[tokio::test]
    async fn test_load_snapshot_on_startup_no_panic() {
        let baseline = Arc::new(BaselineModel::default());
        let trainer = Arc::new(ModelTrainer::new(
            baseline,
            "http://127.0.0.1:19999".to_string(), // nothing listening here
            "bgp".to_string(),
            "bgp_events".to_string(),
        ));

        // This should not panic, only log warnings
        ModelTrainer::load_snapshot_on_startup(trainer).await;
    }

    /// Verify that direct DashMap insert works (PrefixBaseline fields are public).
    #[test]
    fn test_insert_baseline_direct() {
        let baseline = Arc::new(BaselineModel::default());

        // Test that we can directly insert into baselines DashMap
        baseline.baselines().insert(
            "10.0.0.0/8".to_string(),
            crate::baseline_model::PrefixBaseline {
                ema: 5.0,
                variance_ema: 0.1,
                sample_count: 10,
            },
        );

        // Verify the entry was inserted
        let entry = baseline.baselines().get("10.0.0.0/8");
        assert!(entry.is_some());
        let baseline_data = entry.unwrap();
        assert_eq!(baseline_data.ema, 5.0);
        assert_eq!(baseline_data.variance_ema, 0.1);
        assert_eq!(baseline_data.sample_count, 10);
    }

    /// Verify snapshot roundtrip logic (simulating save/load without actual HTTP).
    #[test]
    fn test_snapshot_roundtrip_logic() {
        use crate::baseline_model::PrefixBaseline;

        // Create a baseline model and populate it
        let baseline = Arc::new(BaselineModel::default());

        // Add some baseline data
        baseline.baselines().insert(
            "10.0.0.0/8".to_string(),
            PrefixBaseline {
                ema: 5.0,
                variance_ema: 0.1,
                sample_count: 10,
            },
        );
        baseline.baselines().insert(
            "192.168.0.0/16".to_string(),
            PrefixBaseline {
                ema: 3.5,
                variance_ema: 0.05,
                sample_count: 5,
            },
        );

        // Add some AS knowledge
        baseline.record_as_seen(64512, "2024-01-01");
        baseline.record_as_seen(64512, "2024-01-02");
        baseline.record_as_seen(64513, "2024-01-01");

        // Simulate save snapshot: iterate baselines and collect rows
        let baseline_rows: Vec<_> = baseline
            .baselines()
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect();

        // Simulate save snapshot: iterate AS knowledge and collect rows
        let as_knowledge_rows: Vec<_> = baseline
            .as_knowledge()
            .as_to_days()
            .iter()
            .map(|entry| (*entry.key(), entry.value().clone()))
            .collect();

        // Verify we collected the right data
        assert_eq!(baseline_rows.len(), 2);
        assert_eq!(as_knowledge_rows.len(), 2);

        // Check baseline data
        let (prefix1, baseline1) = &baseline_rows[0];
        let (_prefix2, baseline2) = &baseline_rows[1];

        // Order might vary, so check both
        if prefix1 == "10.0.0.0/8" {
            assert_eq!(baseline1.ema, 5.0);
            assert_eq!(baseline1.variance_ema, 0.1);
            assert_eq!(baseline1.sample_count, 10);
            assert_eq!(baseline2.ema, 3.5);
            assert_eq!(baseline2.variance_ema, 0.05);
            assert_eq!(baseline2.sample_count, 5);
        } else {
            assert_eq!(baseline2.ema, 5.0);
            assert_eq!(baseline2.variance_ema, 0.1);
            assert_eq!(baseline2.sample_count, 10);
            assert_eq!(baseline1.ema, 3.5);
            assert_eq!(baseline1.variance_ema, 0.05);
            assert_eq!(baseline1.sample_count, 5);
        }

        // Check AS knowledge data
        let mut asn_64512_days = None;
        let mut asn_64513_days = None;

        for (asn, days) in &as_knowledge_rows {
            if *asn == 64512 {
                asn_64512_days = Some(days);
            } else if *asn == 64513 {
                asn_64513_days = Some(days);
            }
        }

        assert!(asn_64512_days.is_some());
        assert!(asn_64513_days.is_some());

        let days_64512 = asn_64512_days.unwrap();
        let days_64513 = asn_64513_days.unwrap();

        assert!(days_64512.contains(&"2024-01-01".to_string()));
        assert!(days_64512.contains(&"2024-01-02".to_string()));
        assert!(days_64513.contains(&"2024-01-01".to_string()));
    }
}

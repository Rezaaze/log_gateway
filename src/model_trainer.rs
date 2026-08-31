use crate::baseline_model::BaselineModel;
use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs;
use tokio::time;

/// JSON snapshot format for baseline model persistence.
#[derive(Debug, Serialize, Deserialize)]
struct Snapshot {
    saved_at: String,
    version: u32,
    prefix_states: Vec<PrefixState>,
    as_states: Vec<AsState>,
}

/// Prefix state in snapshot.
#[derive(Debug, Serialize, Deserialize)]
struct PrefixState {
    prefix: String,
    ema: f64,
    variance_ema: f64,
    sample_count: u32,
}

/// AS state in snapshot.
#[derive(Debug, Serialize, Deserialize)]
struct AsState {
    asn: u32,
    days_seen: Vec<String>,
}

/// Background task that periodically saves the [`BaselineModel`] state to disk.
///
/// Every day at 00:00 UTC, it saves the current model state as a JSON snapshot.
/// The model learns incrementally from the NATS stream; the trainer only handles persistence.
pub struct ModelTrainer {
    /// The baseline model shared with the anomaly detector.
    baseline: Arc<BaselineModel>,
    /// Directory where snapshots are stored.
    snapshot_dir: PathBuf,
    /// Number of days to keep snapshots (older files are deleted).
    retention_days: u32,
}

impl ModelTrainer {
    /// Creates a new `ModelTrainer`.
    ///
    /// # Arguments
    ///
    /// * `baseline`        – shared `BaselineModel` (same Arc used by `AnomalyDetector`)
    /// * `snapshot_dir`    – directory where snapshots will be stored
    /// * `retention_days`  – number of days to keep snapshots (default: 7)
    pub fn new(baseline: Arc<BaselineModel>, snapshot_dir: PathBuf, retention_days: u32) -> Self {
        Self {
            baseline,
            snapshot_dir,
            retention_days,
        }
    }

    /// Loads the latest snapshot from disk into the baseline model.
    ///
    /// Searches for the newest file matching `baseline_*.json` in the snapshot directory.
    /// If no snapshot exists, returns `Ok(0)` (cold start, not an error).
    ///
    /// Returns the total number of entries loaded (prefix states + AS states).
    pub async fn load_snapshot(&self) -> Result<usize> {
        // Find the newest snapshot file
        let pattern = self.snapshot_dir.join("baseline_*.json");
        let pattern_str = pattern
            .to_str()
            .context("Invalid snapshot directory path")?;

        let mut entries = Vec::new();
        for entry in glob::glob(pattern_str).context("Failed to glob snapshot files")? {
            match entry {
                Ok(path) => {
                    if let Ok(metadata) = fs::metadata(&path).await {
                        if metadata.is_file() {
                            entries.push((path, metadata.modified().ok()));
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("ModelTrainer: glob error: {}", e);
                }
            }
        }

        // Sort by modification time (newest first)
        entries.sort_by_key(|a| std::cmp::Reverse(a.1));
        let newest_path = entries.first().map(|(path, _)| path);

        let Some(path) = newest_path else {
            tracing::info!("ModelTrainer: no snapshot found, cold start");
            return Ok(0);
        };

        // Read and parse the snapshot
        let content = fs::read_to_string(path)
            .await
            .with_context(|| format!("Failed to read snapshot file: {}", path.display()))?;

        let snapshot: Snapshot = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse snapshot JSON: {}", path.display()))?;

        tracing::info!(
            "ModelTrainer: loading snapshot from {} (saved at {}, version {})",
            path.display(),
            snapshot.saved_at,
            snapshot.version
        );

        let mut total_loaded = 0;

        // Load prefix states
        for prefix_state in snapshot.prefix_states {
            self.baseline.baselines().insert(
                prefix_state.prefix.clone(),
                crate::baseline_model::PrefixBaseline {
                    ema: prefix_state.ema,
                    variance_ema: prefix_state.variance_ema,
                    sample_count: prefix_state.sample_count,
                },
            );
            total_loaded += 1;
        }

        // Load AS states
        for as_state in snapshot.as_states {
            for day in as_state.days_seen {
                self.baseline.record_as_seen(as_state.asn, &day);
            }
            total_loaded += 1;
        }

        tracing::info!(
            "ModelTrainer: loaded {} entries from snapshot",
            total_loaded
        );

        Ok(total_loaded)
    }

    /// Saves the current baseline model state as a JSON snapshot.
    ///
    /// Creates the snapshot directory if it doesn't exist, writes the snapshot
    /// atomically (via a temporary file), and deletes snapshots older than
    /// `retention_days`.
    ///
    /// Returns the total number of entries saved (prefix states + AS states).
    pub async fn save_snapshot(&self) -> Result<usize> {
        // Ensure snapshot directory exists
        fs::create_dir_all(&self.snapshot_dir)
            .await
            .with_context(|| {
                format!(
                    "Failed to create snapshot directory: {}",
                    self.snapshot_dir.display()
                )
            })?;

        // Collect prefix states
        let prefix_states: Vec<PrefixState> = self
            .baseline
            .baselines()
            .iter()
            .map(|entry| PrefixState {
                prefix: entry.key().clone(),
                ema: entry.ema,
                variance_ema: entry.variance_ema,
                sample_count: entry.sample_count,
            })
            .collect();

        // Collect AS states
        let as_states: Vec<AsState> = self
            .baseline
            .as_knowledge()
            .as_to_days()
            .iter()
            .map(|entry| AsState {
                asn: *entry.key(),
                days_seen: entry.value().iter().cloned().collect(),
            })
            .collect();

        let total_entries = prefix_states.len() + as_states.len();

        // Create snapshot
        let snapshot = Snapshot {
            saved_at: Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            version: 1,
            prefix_states,
            as_states,
        };

        // Generate filename with current date
        let date_str = Utc::now().format("%Y-%m-%d").to_string();
        let filename = format!("baseline_{}.json", date_str);
        let file_path = self.snapshot_dir.join(&filename);
        let temp_path = self.snapshot_dir.join(format!("{}.tmp", filename));

        // Write to temporary file first
        let json = serde_json::to_string_pretty(&snapshot)
            .context("Failed to serialize snapshot to JSON")?;
        fs::write(&temp_path, &json).await.with_context(|| {
            format!(
                "Failed to write temporary snapshot: {}",
                temp_path.display()
            )
        })?;

        // Atomically rename to final file
        fs::rename(&temp_path, &file_path).await.with_context(|| {
            format!(
                "Failed to rename snapshot: {} -> {}",
                temp_path.display(),
                file_path.display()
            )
        })?;

        tracing::info!(
            "ModelTrainer: saved snapshot with {} entries to {}",
            total_entries,
            file_path.display()
        );

        // Clean up old snapshots
        self.cleanup_old_snapshots().await?;

        Ok(total_entries)
    }

    /// Deletes snapshot files older than `retention_days`.
    async fn cleanup_old_snapshots(&self) -> Result<()> {
        let pattern = self.snapshot_dir.join("baseline_*.json");
        let pattern_str = pattern
            .to_str()
            .context("Invalid snapshot directory path")?;

        let cutoff_time = Utc::now() - chrono::Duration::days(self.retention_days as i64);

        for entry in glob::glob(pattern_str).context("Failed to glob snapshot files")? {
            match entry {
                Ok(path) => {
                    // Extract date from filename
                    let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if let Some(date_str) = filename
                        .strip_prefix("baseline_")
                        .and_then(|s| s.strip_suffix(".json"))
                    {
                        if let Ok(file_date) =
                            chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d")
                        {
                            // Create DateTime from NaiveDate at midnight UTC
                            let file_datetime = Utc
                                .from_local_datetime(&file_date.and_hms_opt(0, 0, 0).unwrap())
                                .single()
                                .unwrap_or(Utc::now());

                            if file_datetime < cutoff_time {
                                match fs::remove_file(&path).await {
                                    Ok(_) => {
                                        tracing::info!(
                                            "ModelTrainer: deleted old snapshot: {}",
                                            path.display()
                                        );
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            "ModelTrainer: failed to delete old snapshot {}: {}",
                                            path.display(),
                                            e
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("ModelTrainer: glob error during cleanup: {}", e);
                }
            }
        }

        Ok(())
    }

    /// Background loop: sleeps until the next 00:00 UTC, saves a snapshot,
    /// then repeats indefinitely.
    ///
    /// Errors from snapshot saving are logged as warnings but never abort the loop.
    pub async fn run_daily(self: Arc<Self>) {
        tracing::info!("ModelTrainer daily snapshot task started");

        loop {
            let now = Utc::now();
            let next_midnight = Self::next_midnight(now);
            let sleep_secs = (next_midnight - now).num_seconds().max(0) as u64;

            tracing::info!(
                "Next snapshot scheduled for {} (in {} hours)",
                next_midnight.format("%Y-%m-%d %H:%M:%S UTC"),
                sleep_secs / 3600,
            );

            time::sleep(time::Duration::from_secs(sleep_secs)).await;

            tracing::info!("ModelTrainer: starting daily snapshot");

            match self.save_snapshot().await {
                Ok(count) => {
                    tracing::info!("ModelTrainer: snapshot saved — {} entries", count);
                }
                Err(e) => {
                    tracing::warn!(
                        "ModelTrainer: snapshot save failed (will retry tomorrow): {}",
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

    /// Loads snapshot on startup (graceful degradation).
    ///
    /// This function is meant to be called once during application startup.
    /// Errors are logged as warnings but do not prevent the application from starting.
    pub async fn load_snapshot_on_startup(trainer: Arc<Self>) {
        tracing::info!("ModelTrainer: attempting to load snapshot from disk");

        match trainer.load_snapshot().await {
            Ok(count) => {
                tracing::info!("ModelTrainer: loaded {} entries from snapshot", count);
            }
            Err(e) => {
                tracing::warn!(
                    "ModelTrainer: failed to load snapshot (continuing without): {}",
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
    use tempfile::TempDir;

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

    // ── snapshot roundtrip tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_save_and_load_snapshot() {
        let temp_dir = TempDir::new().unwrap();
        let snapshot_dir = temp_dir.path().to_path_buf();

        let baseline = Arc::new(BaselineModel::default());
        let trainer = ModelTrainer::new(baseline.clone(), snapshot_dir.clone(), 7);

        // Add some data to the baseline - need at least 5 samples for z-score
        for i in 0..5 {
            baseline.update("10.0.0.0/8", 5.0 + (i as f64 * 0.1));
        }
        baseline.record_as_seen(64512, "2026-01-01");
        baseline.record_as_seen(64512, "2026-01-02");
        baseline.record_as_seen(64513, "2026-01-01");

        // Save snapshot
        let saved_count = trainer.save_snapshot().await.unwrap();
        assert_eq!(saved_count, 3); // 1 prefix + 2 AS entries

        // Create a new baseline and trainer to test loading
        let new_baseline = Arc::new(BaselineModel::default());
        let new_trainer = ModelTrainer::new(new_baseline.clone(), snapshot_dir, 7);

        // Load snapshot
        let loaded_count = new_trainer.load_snapshot().await.unwrap();
        assert_eq!(loaded_count, 3);

        // Verify the data was restored
        let z_score = new_baseline.z_score("10.0.0.0/8", 5.5);
        assert!(z_score.is_some()); // Should have learned the distribution

        assert_eq!(new_baseline.as_knowledge().days_seen(64512), 2);
        assert_eq!(new_baseline.as_knowledge().days_seen(64513), 1);
    }

    #[tokio::test]
    async fn test_load_snapshot_no_files() {
        let temp_dir = TempDir::new().unwrap();
        let snapshot_dir = temp_dir.path().to_path_buf();

        let baseline = Arc::new(BaselineModel::default());
        let trainer = ModelTrainer::new(baseline, snapshot_dir, 7);

        // Should return Ok(0) when no snapshot exists
        let result = trainer.load_snapshot().await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0);
    }

    #[tokio::test]
    async fn test_save_snapshot_creates_directory() {
        let temp_dir = TempDir::new().unwrap();
        let snapshot_dir = temp_dir.path().join("nonexistent").join("subdir");

        let baseline = Arc::new(BaselineModel::default());
        let trainer = ModelTrainer::new(baseline, snapshot_dir.clone(), 7);

        // Directory shouldn't exist yet
        assert!(!snapshot_dir.exists());

        // Save should create the directory
        let result = trainer.save_snapshot().await;
        assert!(result.is_ok());
        assert!(snapshot_dir.exists());
    }

    #[tokio::test]
    async fn test_cleanup_old_snapshots() {
        let temp_dir = TempDir::new().unwrap();
        let snapshot_dir = temp_dir.path().to_path_buf();

        let baseline = Arc::new(BaselineModel::default());
        let trainer = ModelTrainer::new(baseline, snapshot_dir.clone(), 1); // 1 day retention

        // Create some old snapshot files
        let old_date = Utc::now() - chrono::Duration::days(2);
        let old_filename = format!("baseline_{}.json", old_date.format("%Y-%m-%d"));
        let old_path = snapshot_dir.join(old_filename);
        fs::create_dir_all(&snapshot_dir).await.unwrap();
        fs::write(&old_path, "{}").await.unwrap();

        // Create a recent snapshot
        let recent_date = Utc::now();
        let recent_filename = format!("baseline_{}.json", recent_date.format("%Y-%m-%d"));
        let recent_path = snapshot_dir.join(recent_filename);
        fs::write(&recent_path, "{}").await.unwrap();

        // Run cleanup
        trainer.cleanup_old_snapshots().await.unwrap();

        // Old file should be deleted, recent file should remain
        assert!(!old_path.exists());
        assert!(recent_path.exists());
    }

    #[tokio::test]
    async fn test_save_snapshot_fails_gracefully_on_permission_error() {
        // Try to save to a root directory (should fail due to permissions)
        let snapshot_dir = PathBuf::from("/root/snapshots"); // Usually not writable

        let baseline = Arc::new(BaselineModel::default());
        let trainer = ModelTrainer::new(baseline, snapshot_dir, 7);

        // This should return an error, not panic
        let result = trainer.save_snapshot().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_load_snapshot_fails_gracefully_on_corrupted_file() {
        let temp_dir = TempDir::new().unwrap();
        let snapshot_dir = temp_dir.path().to_path_buf();

        // Create a corrupted JSON file
        let corrupted_path = snapshot_dir.join("baseline_2026-01-01.json");
        fs::create_dir_all(&snapshot_dir).await.unwrap();
        fs::write(&corrupted_path, "not valid json").await.unwrap();

        let baseline = Arc::new(BaselineModel::default());
        let trainer = ModelTrainer::new(baseline, snapshot_dir, 7);

        // This should return an error, not panic
        let result = trainer.load_snapshot().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_load_snapshot_on_startup_no_panic() {
        let temp_dir = TempDir::new().unwrap();
        let snapshot_dir = temp_dir.path().to_path_buf();

        let baseline = Arc::new(BaselineModel::default());
        let trainer = Arc::new(ModelTrainer::new(baseline, snapshot_dir, 7));

        // This should not panic, even with no snapshot
        ModelTrainer::load_snapshot_on_startup(trainer).await;
    }

    /// Verify that the trainer struct can be created and is Send + Sync.
    #[test]
    fn test_trainer_new() {
        let baseline = Arc::new(BaselineModel::default());
        let trainer = Arc::new(ModelTrainer::new(
            baseline,
            PathBuf::from("/data/snapshots"),
            7,
        ));
        // If this compiles and runs, Send + Sync is satisfied
        let _clone = Arc::clone(&trainer);
    }
}

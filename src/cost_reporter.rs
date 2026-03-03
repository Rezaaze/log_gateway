use crate::config::SmtpConfig;
use crate::cost_tracker::{CostTracker, TenantStats};
use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, TimeZone, Utc};
use reqwest::Client;
use std::sync::Arc;
use tokio::time;

/// Cost reporter for generating and sending monthly cost reports via email
pub struct CostReporter {
    /// Cost tracker to get tenant statistics
    pub cost_tracker: Arc<CostTracker>,
    /// SMTP configuration for email sending
    pub smtp_config: SmtpConfig,
    /// HTTP client for sending requests to SMTP gateway
    pub http: Client,
}

impl CostReporter {
    /// Create a new CostReporter instance
    pub fn new(cost_tracker: Arc<CostTracker>, smtp_config: SmtpConfig) -> Self {
        Self {
            cost_tracker,
            smtp_config,
            http: Client::new(),
        }
    }

    /// Generate a plaintext report from tenant cost summaries
    pub fn generate_report(summary: &[TenantStats]) -> String {
        let now = Utc::now();
        let mut report = String::new();

        // Header
        report.push_str("=== Monthly Cost Report ===\n");
        report.push_str(&format!(
            "Generated: {}\n\n",
            now.format("%Y-%m-%d %H:%M:%S UTC")
        ));

        // Tenant details
        for tenant in summary {
            let bytes_mb = tenant.total_bytes_ingested as f64 / (1024.0 * 1024.0);
            report.push_str(&format!("Tenant: {}\n", tenant.tenant_id));
            report.push_str(&format!("  Requests: {}\n", tenant.total_requests));
            report.push_str(&format!("  Bytes:    {:.1} MB\n", bytes_mb));
            report.push_str(&format!("  PII Hits: {}\n", tenant.total_pii_hits));
            report.push('\n');
        }

        // Footer
        report.push_str(&format!("Total tenants: {}\n", summary.len()));

        report
    }

    /// Send report via HTTP POST to an SMTP gateway (e.g., MailHog / smtp2http)
    pub async fn send_report(&self, report: &str) -> Result<()> {
        if !self.smtp_config.enabled {
            tracing::info!("SMTP reporting is disabled, skipping email send");
            return Ok(());
        }

        if self.smtp_config.to.is_empty() {
            tracing::warn!("No recipients configured for SMTP reporting");
            return Ok(());
        }

        // Prepare email payload for SMTP gateway
        let payload = serde_json::json!({
            "from": self.smtp_config.from,
            "to": self.smtp_config.to,
            "subject": format!("Monthly Cost Report - {}", Utc::now().format("%Y-%m")),
            "text": report,
            "host": self.smtp_config.host,
            "port": self.smtp_config.port,
            "username": self.smtp_config.username,
            "password": self.smtp_config.password,
        });

        // Send HTTP POST to SMTP gateway
        // Typically this would be something like http://localhost:8025/api/v2/send for MailHog
        // or a custom smtp2http endpoint
        let gateway_url = format!("http://{}/api/v2/send", self.smtp_config.host);

        tracing::info!(
            "Sending monthly cost report to {} recipients via {}",
            self.smtp_config.to.len(),
            gateway_url
        );

        let response = self
            .http
            .post(&gateway_url)
            .json(&payload)
            .send()
            .await
            .context("Failed to send HTTP request to SMTP gateway")?;

        if response.status().is_success() {
            tracing::info!("Monthly cost report sent successfully");
            Ok(())
        } else {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            Err(anyhow::anyhow!(
                "SMTP gateway returned error status {}: {}",
                status,
                body
            ))
        }
    }

    /// Background task: sleeps until the next month start (UTC 00:00), sends report, sleeps again
    pub async fn run_monthly(self: Arc<Self>) {
        tracing::info!("Monthly cost reporter task started");

        loop {
            // Calculate sleep duration until next month start
            let now = Utc::now();
            let next_month_start = Self::next_month_start(now);
            let sleep_duration = next_month_start - now;

            if sleep_duration.num_seconds() > 0 {
                tracing::info!(
                    "Next cost report scheduled for {} (in {} hours)",
                    next_month_start.format("%Y-%m-%d %H:%M:%S UTC"),
                    sleep_duration.num_hours()
                );

                // Sleep until next month start
                time::sleep(time::Duration::from_secs(
                    sleep_duration.num_seconds() as u64
                ))
                .await;
            }

            // Generate and send report
            match self.generate_and_send_report().await {
                Ok(_) => tracing::info!("Monthly cost report completed successfully"),
                Err(e) => tracing::error!("Failed to send monthly cost report: {}", e),
            }
        }
    }

    /// Generate report from current cost tracker data and send it
    async fn generate_and_send_report(&self) -> Result<()> {
        let summary = self.cost_tracker.summary();
        let report = Self::generate_report(&summary.tenants);

        self.send_report(&report).await
    }

    /// Pure function to calculate the next month start (UTC 00:00)
    pub fn next_month_start(now: DateTime<Utc>) -> DateTime<Utc> {
        let mut year = now.year();
        let mut month = now.month() + 1;

        // Handle December -> January year rollover
        if month > 12 {
            month = 1;
            year += 1;
        }

        // Create datetime for the first day of next month at 00:00:00 UTC
        Utc.with_ymd_and_hms(year, month, 1, 0, 0, 0)
            .single()
            .expect("Invalid date for next month start")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_report_empty() {
        let summary: Vec<TenantStats> = Vec::new();
        let report = CostReporter::generate_report(&summary);

        assert!(report.contains("Total tenants: 0"));
        assert!(report.contains("=== Monthly Cost Report ==="));
    }

    #[test]
    fn test_generate_report_single_tenant() {
        let summary = vec![TenantStats {
            tenant_id: "acme-corp".to_string(),
            total_requests: 125000,
            total_bytes_ingested: 47_381_811, // 45.2 MB in bytes (45.2 * 1024 * 1024 ≈ 47,381,811)
            total_pii_hits: 12,
            cache_hits: 0,
            cache_misses: 0,
        }];

        let report = CostReporter::generate_report(&summary);

        assert!(report.contains("Tenant: acme-corp"));
        assert!(report.contains("Requests: 125000"));
        // Check for "Bytes:" followed by "45.2 MB" (allow flexible spacing)
        assert!(report.contains("Bytes:"));
        assert!(report.contains("45.2 MB"));
        assert!(report.contains("PII Hits: 12"));
        assert!(report.contains("Total tenants: 1"));
    }

    #[test]
    fn test_next_month_start_from_jan() {
        // 2026-01-15 14:30:00 UTC
        let jan_15 = Utc.with_ymd_and_hms(2026, 1, 15, 14, 30, 0).unwrap();
        let next = CostReporter::next_month_start(jan_15);

        // Should be 2026-02-01 00:00:00 UTC
        let expected = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap();
        assert_eq!(next, expected);
    }

    #[test]
    fn test_next_month_start_from_dec() {
        // 2026-12-20 23:59:59 UTC
        let dec_20 = Utc.with_ymd_and_hms(2026, 12, 20, 23, 59, 59).unwrap();
        let next = CostReporter::next_month_start(dec_20);

        // Should be 2027-01-01 00:00:00 UTC (year rollover)
        let expected = Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap();
        assert_eq!(next, expected);
    }

    #[test]
    fn test_next_month_start_end_of_month() {
        // 2026-02-28 00:00:00 UTC (non-leap year)
        let feb_28 = Utc.with_ymd_and_hms(2026, 2, 28, 0, 0, 0).unwrap();
        let next = CostReporter::next_month_start(feb_28);

        // Should be 2026-03-01 00:00:00 UTC
        let expected = Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap();
        assert_eq!(next, expected);
    }

    #[test]
    fn test_next_month_start_leap_year() {
        // 2024-02-29 12:00:00 UTC (leap year)
        let feb_29 = Utc.with_ymd_and_hms(2024, 2, 29, 12, 0, 0).unwrap();
        let next = CostReporter::next_month_start(feb_29);

        // Should be 2024-03-01 00:00:00 UTC
        let expected = Utc.with_ymd_and_hms(2024, 3, 1, 0, 0, 0).unwrap();
        assert_eq!(next, expected);
    }
}

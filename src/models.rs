use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, PartialEq, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct LogEntry {
    #[serde(default = "Uuid::new_v4")]
    pub id: Uuid,
    #[serde(default = "Utc::now")]
    pub timestamp: DateTime<Utc>,
    pub level: LogLevel,
    pub source: String,
    pub message: String,
    pub metadata: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct IngestResponse {
    pub id: Uuid,
    pub status: String,
    pub processed_at: DateTime<Utc>,
    pub pii_hits: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_level_deserialize_lowercase() {
        let json = "\"info\"";
        let level: LogLevel = serde_json::from_str(json).unwrap();
        assert!(matches!(level, LogLevel::Info));
    }

    #[test]
    fn test_log_level_deserialize_all_variants() {
        // Jede Variante explizit prüfen — matches! mit Variable ist tautologisch
        let debug: LogLevel = serde_json::from_str("\"debug\"").unwrap();
        assert!(matches!(debug, LogLevel::Debug));

        let info: LogLevel = serde_json::from_str("\"info\"").unwrap();
        assert!(matches!(info, LogLevel::Info));

        let warn: LogLevel = serde_json::from_str("\"warn\"").unwrap();
        assert!(matches!(warn, LogLevel::Warn));

        let error: LogLevel = serde_json::from_str("\"error\"").unwrap();
        assert!(matches!(error, LogLevel::Error));
    }

    #[test]
    fn test_log_entry_deserialize() {
        let json = r#"{
            "id": "123e4567-e89b-12d3-a456-426614174000",
            "timestamp": "2024-01-01T00:00:00Z",
            "level": "info",
            "source": "test-service",
            "message": "test message",
            "metadata": {"key": "value"}
        }"#;

        let entry: LogEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.source, "test-service");
        assert_eq!(entry.message, "test message");
        assert!(matches!(entry.level, LogLevel::Info));
        assert!(entry.metadata.is_some());
    }

    #[test]
    fn test_ingest_response_serialize() {
        use chrono::TimeZone;

        let response = IngestResponse {
            id: Uuid::parse_str("123e4567-e89b-12d3-a456-426614174000").unwrap(),
            status: "accepted".to_string(),
            processed_at: Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
            pii_hits: 5,
        };

        let json = serde_json::to_string(&response).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["id"], "123e4567-e89b-12d3-a456-426614174000");
        assert_eq!(parsed["status"], "accepted");
        assert_eq!(parsed["pii_hits"], 5);
        assert!(parsed.get("processed_at").is_some());
    }
}

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::Utc;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;
use uuid::Uuid;

use crate::cache::{CacheEntry, CacheStats, SemanticCache};
use crate::cost_tracker::{CostTracker, GatewayCostSummary};
use crate::metrics::GatewayMetrics;
use crate::models::{IngestResponse, LogEntry, LogLevel};
use crate::redactor::Redactor;
use crate::s3_exporter::S3Exporter;
use crate::sink::{SinkRecord, StorageSink};
use serde::{Deserialize, Serialize};
use utoipa::{OpenApi, ToSchema};

#[derive(Serialize, Deserialize, ToSchema)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub uptime_seconds: u64,
    pub cache_size: u64,
    pub checks: HealthChecks,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct HealthChecks {
    pub cache: String,
    pub sink: String,
}

#[derive(Debug, Clone)]
pub struct AppState {
    pub redactor: Redactor,
    pub cache: SemanticCache,
    pub cost_tracker: CostTracker,
    pub metrics: GatewayMetrics,
    pub sink: Option<StorageSink>,
    pub s3_exporter: Option<Arc<S3Exporter>>,
    pub sink_output_dir: PathBuf,
    pub started_at: std::time::Instant,
    /// API key cached at startup — avoids per-request disk reads of /run/secrets/
    pub api_key: Option<Arc<String>>,
    /// JWT secret cached at startup — avoids per-request disk reads of /run/secrets/
    pub jwt_secret: Option<Arc<String>>,
}

#[utoipa::path(
    post,
    path = "/api/v1/logs",
    request_body = LogEntry,
    responses(
        (status = 200, description = "Log accepted", body = IngestResponse),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Unauthorized"),
        (status = 429, description = "Rate limit exceeded"),
    ),
    security(("api_key" = []), ("bearer_auth" = []))
)]
pub async fn ingest_log(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    // Start latency measurement
    let start = std::time::Instant::now();

    // Generate ID at the beginning for all responses
    let id = Uuid::new_v4();

    // Helper function to create headers with x-request-id
    fn create_headers(id: &Uuid) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("x-request-id", id.to_string().parse().unwrap());
        headers
    }

    // 1. Deserialize bytes → Value (single JSON parse, reused for schema validation)
    let raw: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            let duration_ms = start.elapsed().as_secs_f64() * 1000.0;
            state.metrics.record_duration(duration_ms);
            return (
                StatusCode::BAD_REQUEST,
                create_headers(&id),
                Json(serde_json::json!({ "error": format!("invalid log entry: {}", e) })),
            )
                .into_response();
        }
    };

    // 2. Schema validation against the already-parsed Value (no re-serialization)
    if let Err(e) = crate::schema_validator::SchemaValidator::validate(&raw) {
        let duration_ms = start.elapsed().as_secs_f64() * 1000.0;
        state.metrics.record_duration(duration_ms);
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            create_headers(&id),
            Json(serde_json::json!({ "error": "schema validation failed", "details": e })),
        )
            .into_response();
    }

    // 3. Value → LogEntry (reuses the already-parsed Value, no second parse from bytes)
    let entry: LogEntry = match serde_json::from_value(raw) {
        Ok(e) => e,
        Err(e) => {
            let duration_ms = start.elapsed().as_secs_f64() * 1000.0;
            state.metrics.record_duration(duration_ms);
            return (
                StatusCode::BAD_REQUEST,
                create_headers(&id),
                Json(serde_json::json!({ "error": format!("invalid log entry: {}", e) })),
            )
                .into_response();
        }
    };

    // 4. Extract and validate tenant ID from X-Tenant-ID header
    let tenant_id = headers
        .get("X-Tenant-ID")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("anonymous");

    // Validate tenant ID using same logic as rate_limiter.rs
    fn is_valid_tenant_id(id: &str) -> bool {
        id.len() <= 64
            && id
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    }

    if !is_valid_tenant_id(tenant_id) {
        let duration_ms = start.elapsed().as_secs_f64() * 1000.0;
        state.metrics.record_duration(duration_ms);
        return (
            StatusCode::BAD_REQUEST,
            create_headers(&id),
            Json(serde_json::json!({
                "error": "invalid_tenant_id",
                "hint": "tenant ID must be at most 64 characters and contain only alphanumeric characters, hyphens, and underscores"
            })),
        )
            .into_response();
    }

    // 4. Rest des Handlers ab hier unverändert weiterführen
    // (PII redaction, cache lookup, cost tracking, metrics, sink write, response)

    // Generate cache key
    let key = SemanticCache::make_key(&entry.message);

    // Check cache
    if let Some(cached) = state.cache.get(&key) {
        // Cache HIT
        info!("Cache HIT: id={}, key={}", id, key);

        // Use inspect method to satisfy compiler dead code analysis
        let _ = cached.inspect();

        // Calculate bytes and record cost
        let bytes = entry.message.len() as u64;
        state
            .cost_tracker
            .record(tenant_id, bytes, cached.pii_hits, true);
        state.metrics.record_request(bytes, cached.pii_hits, true);

        let response = IngestResponse {
            id,
            status: "accepted".to_string(),
            processed_at: Utc::now(),
            pii_hits: cached.pii_hits,
        };

        // Write to storage sink if enabled
        if let Some(sink) = &state.sink {
            let record = SinkRecord {
                id,
                timestamp: Utc::now(),
                source: entry.source.clone(),
                level: format!("{:?}", entry.level),
                redacted_message: cached.redacted_message.clone(),
                pii_hits: cached.pii_hits,
                bytes,
                cache_hit: true,
            };
            sink.write(record);
        }

        let duration_ms = start.elapsed().as_secs_f64() * 1000.0;
        state.metrics.record_duration(duration_ms);
        return (StatusCode::ACCEPTED, create_headers(&id), Json(response)).into_response();
    }

    // Cache MISS - run redaction
    let result = state.redactor.redact(&entry.message);

    // Build cache entry
    let cache_entry = CacheEntry {
        redacted_message: result.redacted_text.clone(),
        pii_hits: result.hit_count,
        created_at: Utc::now(),
    };

    // Insert into cache
    state.cache.insert(key.clone(), cache_entry);

    // Calculate bytes and record cost
    let bytes = entry.message.len() as u64;
    state
        .cost_tracker
        .record(tenant_id, bytes, result.hit_count, false);
    state.metrics.record_request(bytes, result.hit_count, false);

    // Log with tracing::info!
    info!("Cache MISS: id={}, pii_hits={}", id, result.hit_count);

    let response = IngestResponse {
        id,
        status: "accepted".to_string(),
        processed_at: Utc::now(),
        pii_hits: result.hit_count,
    };

    // Write to storage sink if enabled
    if let Some(sink) = &state.sink {
        let record = SinkRecord {
            id,
            timestamp: Utc::now(),
            source: entry.source.clone(),
            level: format!("{:?}", entry.level),
            redacted_message: result.redacted_text.clone(),
            pii_hits: result.hit_count,
            bytes,
            cache_hit: false,
        };
        sink.write(record);
    }

    let duration_ms = start.elapsed().as_secs_f64() * 1000.0;
    state.metrics.record_duration(duration_ms);
    (StatusCode::ACCEPTED, create_headers(&id), Json(response)).into_response()
}

#[utoipa::path(
    get,
    path = "/health",
    responses(
        (status = 200, description = "Gateway healthy", body = HealthResponse),
    )
)]
pub async fn health_check(State(state): State<AppState>) -> Json<HealthResponse> {
    let uptime_seconds = state.started_at.elapsed().as_secs();
    let cache_size = state.cache.len();
    let sink_status = if state.sink.is_some() {
        "ok"
    } else {
        "disabled"
    };

    Json(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_seconds,
        cache_size,
        checks: HealthChecks {
            cache: "ok".to_string(),
            sink: sink_status.to_string(),
        },
    })
}

#[utoipa::path(
    get,
    path = "/api/v1/cache/stats",
    responses(
        (status = 200, description = "Cache statistics"),
        (status = 401, description = "Unauthorized"),
    ),
    security(("api_key" = []), ("bearer_auth" = []))
)]
pub async fn cache_stats(State(state): State<AppState>) -> Json<CacheStats> {
    Json(state.cache.stats())
}

#[utoipa::path(
    get,
    path = "/api/v1/costs",
    responses(
        (status = 200, description = "Cost summary"),
        (status = 401, description = "Unauthorized"),
    ),
    security(("api_key" = []), ("bearer_auth" = []))
)]
pub async fn cost_summary(State(state): State<AppState>) -> Json<GatewayCostSummary> {
    Json(state.cost_tracker.summary())
}

pub async fn tenant_cost(
    State(state): State<AppState>,
    axum::extract::Path(tenant_id): axum::extract::Path<String>,
) -> Response {
    // Validate tenant_id: max 64 chars, alphanumeric + dash + underscore only
    if tenant_id.len() > 64
        || !tenant_id
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "invalid tenant_id format"})),
        )
            .into_response();
    }

    if let Some(stats) = state.cost_tracker.tenant_stats(&tenant_id) {
        (StatusCode::OK, Json(stats)).into_response()
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "tenant not found"})),
        )
            .into_response()
    }
}

pub async fn metrics(State(state): State<AppState>) -> impl axum::response::IntoResponse {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4",
        )],
        state.metrics.render(),
    )
}

#[derive(Serialize)]
pub struct ExportResponse {
    pub files_uploaded: usize,
    pub status: &'static str,
}

#[utoipa::path(
    post,
    path = "/api/v1/export/s3",
    responses(
        (status = 200, description = "S3 export triggered"),
        (status = 401, description = "Unauthorized"),
    ),
    security(("api_key" = []), ("bearer_auth" = []))
)]
pub async fn trigger_s3_export(
    State(state): State<AppState>,
) -> (StatusCode, Json<ExportResponse>) {
    match &state.s3_exporter {
        Some(exporter) => match exporter.export_pending_files(&state.sink_output_dir).await {
            Ok(n) => (
                StatusCode::OK,
                Json(ExportResponse {
                    files_uploaded: n,
                    status: "ok",
                }),
            ),
            Err(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ExportResponse {
                    files_uploaded: 0,
                    status: "error",
                }),
            ),
        },
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ExportResponse {
                files_uploaded: 0,
                status: "s3_disabled",
            }),
        ),
    }
}

#[derive(OpenApi)]
#[openapi(
    paths(ingest_log, health_check, cache_stats, cost_summary, trigger_s3_export),
    components(schemas(LogEntry, LogLevel, IngestResponse, HealthResponse, HealthChecks)),
    modifiers(&SecurityAddon),
    info(
        title = "Log Gateway API",
        version = "1.0.0",
        description = "Production-grade Rust Log Processing Gateway"
    ),
)]
pub struct ApiDoc;

struct SecurityAddon;

impl utoipa::Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        use utoipa::openapi::security::{
            ApiKey, ApiKeyValue, Http, HttpAuthScheme, SecurityScheme,
        };
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "api_key",
                SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::new("X-API-Key"))),
            );
            components.add_security_scheme(
                "bearer_auth",
                SecurityScheme::Http(Http::new(HttpAuthScheme::Bearer)),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum_test::TestServer;

    #[tokio::test]
    async fn test_health_check_response() {
        // Create app state
        let app_state = AppState {
            redactor: Redactor::new(),
            cache: SemanticCache::new(100, 60),
            cost_tracker: CostTracker::new(),
            metrics: GatewayMetrics::new(),
            sink: None,
            s3_exporter: None,
            sink_output_dir: PathBuf::from("data/logs"),
            started_at: std::time::Instant::now(),
            api_key: None,
            jwt_secret: None,
        };

        // Build application
        let app = axum::Router::new()
            .route("/health", axum::routing::get(health_check))
            .with_state(app_state);

        let server = TestServer::new(app).unwrap();

        // Make request
        let response = server.get("/health").await;

        assert_eq!(response.status_code(), StatusCode::OK);

        let health_response: HealthResponse = response.json();

        assert_eq!(health_response.status, "ok");
        assert_eq!(health_response.version, "0.1.0");
        // uptime_seconds is u64, so it's always >= 0
        assert_eq!(health_response.cache_size, 0);
        assert_eq!(health_response.checks.cache, "ok");
        assert_eq!(health_response.checks.sink, "disabled");
    }

    #[tokio::test]
    async fn test_ingest_log_headers() {
        // Create app state
        let app_state = AppState {
            redactor: Redactor::new(),
            cache: SemanticCache::new(100, 60),
            cost_tracker: CostTracker::new(),
            metrics: GatewayMetrics::new(),
            sink: None,
            s3_exporter: None,
            sink_output_dir: PathBuf::from("data/logs"),
            started_at: std::time::Instant::now(),
            api_key: None,
            jwt_secret: None,
        };

        // Build application
        let app = axum::Router::new()
            .route("/api/v1/logs", axum::routing::post(ingest_log))
            .with_state(app_state);

        let server = TestServer::new(app).unwrap();

        // Make request
        let response = server
            .post("/api/v1/logs")
            .json(&serde_json::json!({
                "source": "test-service",
                "level": "info",
                "message": "test message"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::ACCEPTED);

        // Check for x-request-id header
        let headers = response.headers();
        assert!(headers.contains_key("x-request-id"));

        let request_id = headers.get("x-request-id").unwrap().to_str().unwrap();
        assert!(!request_id.is_empty());

        // Verify the response body contains the same ID
        let ingest_response: IngestResponse = response.json();
        assert_eq!(request_id, ingest_response.id.to_string());
    }

    #[tokio::test]
    async fn test_metrics_contains_histogram() {
        // Create app state
        let app_state = AppState {
            redactor: Redactor::new(),
            cache: SemanticCache::new(100, 60),
            cost_tracker: CostTracker::new(),
            metrics: GatewayMetrics::new(),
            sink: None,
            s3_exporter: None,
            sink_output_dir: PathBuf::from("data/logs"),
            started_at: std::time::Instant::now(),
            api_key: None,
            jwt_secret: None,
        };

        // Build application
        let app = axum::Router::new()
            .route("/metrics", axum::routing::get(metrics))
            .with_state(app_state);

        let server = TestServer::new(app).unwrap();

        // Make request
        let response = server.get("/metrics").await;

        assert_eq!(response.status_code(), StatusCode::OK);

        let body = response.text();

        // Check that histogram appears in metrics
        assert!(body.contains("gateway_request_duration_ms"));
        assert!(body.contains("Request processing duration in milliseconds."));
    }

    #[tokio::test]
    async fn test_ingest_uses_tenant_id_header_for_cost() {
        // Create app state
        let app_state = AppState {
            redactor: Redactor::new(),
            cache: SemanticCache::new(100, 60),
            cost_tracker: CostTracker::new(),
            metrics: GatewayMetrics::new(),
            sink: None,
            s3_exporter: None,
            sink_output_dir: PathBuf::from("data/logs"),
            started_at: std::time::Instant::now(),
            api_key: None,
            jwt_secret: None,
        };

        let cost_tracker = app_state.cost_tracker.clone();

        let app = axum::Router::new()
            .route("/api/v1/logs", axum::routing::post(ingest_log))
            .with_state(app_state);

        let server = TestServer::new(app).unwrap();

        let response = server
            .post("/api/v1/logs")
            .add_header(
                axum::http::HeaderName::from_static("x-tenant-id"),
                axum::http::HeaderValue::from_static("my-tenant"),
            )
            .json(&serde_json::json!({
                "source": "test-service",
                "level": "info",
                "message": "hello world"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::ACCEPTED);

        // Cost should be tracked under "my-tenant", not "test-service"
        let summary = cost_tracker.summary();
        assert!(
            summary.tenants.iter().any(|t| t.tenant_id == "my-tenant"),
            "Expected cost tracked under 'my-tenant'"
        );
    }

    #[tokio::test]
    async fn test_ingest_falls_back_to_anonymous_without_header() {
        let app_state = AppState {
            redactor: Redactor::new(),
            cache: SemanticCache::new(100, 60),
            cost_tracker: CostTracker::new(),
            metrics: GatewayMetrics::new(),
            sink: None,
            s3_exporter: None,
            sink_output_dir: PathBuf::from("data/logs"),
            started_at: std::time::Instant::now(),
            api_key: None,
            jwt_secret: None,
        };

        let cost_tracker = app_state.cost_tracker.clone();

        let app = axum::Router::new()
            .route("/api/v1/logs", axum::routing::post(ingest_log))
            .with_state(app_state);

        let server = TestServer::new(app).unwrap();

        let response = server
            .post("/api/v1/logs")
            .json(&serde_json::json!({
                "source": "test-service",
                "level": "info",
                "message": "hello world"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::ACCEPTED);

        let summary = cost_tracker.summary();
        assert!(
            summary.tenants.iter().any(|t| t.tenant_id == "anonymous"),
            "Expected cost tracked under 'anonymous'"
        );
    }

    #[tokio::test]
    async fn test_ingest_rejects_invalid_tenant_id() {
        let app_state = AppState {
            redactor: Redactor::new(),
            cache: SemanticCache::new(100, 60),
            cost_tracker: CostTracker::new(),
            metrics: GatewayMetrics::new(),
            sink: None,
            s3_exporter: None,
            sink_output_dir: PathBuf::from("data/logs"),
            started_at: std::time::Instant::now(),
            api_key: None,
            jwt_secret: None,
        };

        let app = axum::Router::new()
            .route("/api/v1/logs", axum::routing::post(ingest_log))
            .with_state(app_state);

        let server = TestServer::new(app).unwrap();

        let response = server
            .post("/api/v1/logs")
            .add_header(
                axum::http::HeaderName::from_static("x-tenant-id"),
                axum::http::HeaderValue::from_static("invalid tenant!"),
            )
            .json(&serde_json::json!({
                "source": "test-service",
                "level": "info",
                "message": "hello world"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
    }
}

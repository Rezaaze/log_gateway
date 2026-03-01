use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

/// ClickHouse query client for BGP data
#[derive(Debug)]
pub struct ClickHouseQueryClient {
    client: Client,
    url: String,
    database: String,
}

impl ClickHouseQueryClient {
    /// Create a new ClickHouseQueryClient
    pub fn new(url: String, database: String) -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .pool_max_idle_per_host(5)
            .build()
            .expect("Failed to build reqwest client");

        Self {
            client,
            url,
            database,
        }
    }

    /// Execute a query against ClickHouse and deserialize the response
    pub async fn query<T: serde::de::DeserializeOwned>(
        &self,
        sql: &str,
    ) -> Result<Vec<T>, String> {
        let url = format!("{}/?database={}&default_format=JSONEachRow", self.url, self.database);

        let response = self
            .client
            .post(&url)
            .body(sql.to_string())
            .send()
            .await
            .map_err(|e| format!("ClickHouse request failed: {}", e))?;

        if !response.status().is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Failed to read error body".to_string());
            return Err(format!("ClickHouse error: {}", body));
        }

        let body = response
            .text()
            .await
            .map_err(|e| format!("Failed to read response body: {}", e))?;

        // Parse newline-delimited JSON (JSONEachRow format)
        let mut results = Vec::new();
        for line in body.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let value: T = serde_json::from_str(line)
                .map_err(|e| format!("Failed to parse JSON line: {} - {}", e, line))?;
            results.push(value);
        }

        Ok(results)
    }
}

/// Response struct for prefix history endpoint
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PrefixHistoryEntry {
    pub timestamp: String,
    pub event_type: String,
    pub origin_as: u32,
    pub as_path: Vec<u32>,
    pub peer_asn: u32,
}

/// Response struct for ASN prefixes endpoint
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AsnPrefixEntry {
    pub prefix: String,
    pub last_seen: String,
    pub event_count: u64,
}

/// Response struct for BGP events endpoint
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct BgpEventEntry {
    pub timestamp: String,
    pub event_type: String,
    pub prefix: String,
    pub origin_as: u32,
    pub as_path: Vec<u32>,
    pub peer_ip: String,
}

/// Response struct for top AS endpoint
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct TopAsEntry {
    pub origin_as: u32,
    pub event_count: u64,
    pub prefix_count: u64,
}

/// Query parameters for prefix history endpoint
#[derive(Debug, Deserialize, IntoParams, ToSchema)]
pub struct PrefixHistoryParams {
    pub from: Option<String>,
    pub to: Option<String>,
    #[param(minimum = 1, maximum = 1000)]
    pub limit: Option<u32>,
}

/// Query parameters for BGP events endpoint
#[derive(Debug, Deserialize, IntoParams, ToSchema)]
pub struct BgpEventsParams {
    pub from: Option<String>,
    pub to: Option<String>,
    pub event_type: Option<String>,
    #[param(minimum = 1, maximum = 1000)]
    pub limit: Option<u32>,
    #[param(minimum = 0)]
    pub offset: Option<u32>,
}

/// Query parameters for top AS endpoint
#[derive(Debug, Deserialize, IntoParams, ToSchema)]
pub struct TopAsParams {
    #[param(minimum = 1, maximum = 100)]
    pub limit: Option<u32>,  // default 10, max 100
}

/// Validate and sanitize prefix parameter
fn validate_prefix(prefix: &str) -> Result<String, (StatusCode, String)> {
    // Manual percent-decode: replace %2F → / and %3A → : (no new dependency)
    let decoded = prefix
        .replace("%2F", "/")
        .replace("%2f", "/")
        .replace("%3A", ":")
        .replace("%3a", ":");

    // Validate prefix format: only [0-9a-fA-F.:\/] allowed
    let is_valid = decoded
        .chars()
        .all(|c: char| c.is_ascii_digit() || c.is_ascii_hexdigit() || c == '.' || c == ':' || c == '/');

    if !is_valid {
        return Err((
            StatusCode::BAD_REQUEST,
            "Invalid prefix format. Only alphanumeric characters, dots, colons, and slash are allowed".to_string(),
        ));
    }

    Ok(decoded)
}

/// Build SQL WHERE clause with optional filters
fn build_where_clause(
    from: Option<&str>,
    to: Option<&str>,
    event_type: Option<&str>,
    prefix: Option<&str>,
    origin_as: Option<u32>,
) -> String {
    let mut conditions = Vec::new();

    if let Some(p) = prefix {
        conditions.push(format!("prefix = '{}'", p.replace("'", "''")));
    }

    if let Some(asn) = origin_as {
        conditions.push(format!("origin_as = {}", asn));
    }

    if let Some(f) = from {
        conditions.push(format!("timestamp >= '{}'", f.replace("'", "''")));
    }

    if let Some(t) = to {
        conditions.push(format!("timestamp <= '{}'", t.replace("'", "''")));
    }

    if let Some(et) = event_type {
        conditions.push(format!("event_type = '{}'", et.replace("'", "''")));
    }

    if conditions.is_empty() {
        "1=1".to_string()
    } else {
        conditions.join(" AND ")
    }
}

/// GET /api/v1/bgp/prefixes/:prefix/history
#[utoipa::path(
    get,
    path = "/api/v1/bgp/prefixes/{prefix}/history",
    params(
        ("prefix" = String, Path, description = "BGP prefix (e.g., 1.2.3.0/24)"),
        PrefixHistoryParams
    ),
    responses(
        (status = 200, description = "Prefix history retrieved", body = [PrefixHistoryEntry]),
        (status = 400, description = "Invalid prefix format"),
        (status = 502, description = "ClickHouse error"),
    ),
    security(("api_key" = []), ("bearer_auth" = []))
)]
pub async fn bgp_prefix_history(
    State(state): State<crate::handlers::AppState>,
    Path(prefix): Path<String>,
    Query(params): Query<PrefixHistoryParams>,
) -> impl IntoResponse {
    // Validate and sanitize prefix
    let prefix = match validate_prefix(&prefix) {
        Ok(p) => p,
        Err((status, error)) => {
            return (status, Json(serde_json::json!({ "error": error }))).into_response();
        }
    };

    // Get ClickHouse client
    let client = match &state.bgp_query_client {
        Some(client) => client,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": "BGP query service is disabled" })),
            )
                .into_response();
        }
    };

    // Build SQL query
    let limit = params.limit.unwrap_or(100).min(1000);
    let where_clause = build_where_clause(
        params.from.as_deref(),
        params.to.as_deref(),
        None,
        Some(&prefix),
        None,
    );

    let sql = format!(
        "SELECT
            formatDateTime(timestamp, '%Y-%m-%dT%H:%i:%SZ') AS timestamp,
            event_type,
            origin_as,
            as_path,
            peer_asn
        FROM bgp_events
        WHERE {}
        ORDER BY timestamp DESC
        LIMIT {}",
        where_clause, limit
    );

    // Execute query
    match client.query::<PrefixHistoryEntry>(&sql).await {
        Ok(results) => (StatusCode::OK, Json(results)).into_response(),
        Err(error) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "error": error })),
        )
            .into_response(),
    }
}

/// GET /api/v1/bgp/asn/:asn/prefixes
#[utoipa::path(
    get,
    path = "/api/v1/bgp/asn/{asn}/prefixes",
    params(
        ("asn" = u32, Path, description = "Autonomous System Number"),
        PrefixHistoryParams
    ),
    responses(
        (status = 200, description = "ASN prefixes retrieved", body = [AsnPrefixEntry]),
        (status = 502, description = "ClickHouse error"),
    ),
    security(("api_key" = []), ("bearer_auth" = []))
)]
pub async fn bgp_asn_prefixes(
    State(state): State<crate::handlers::AppState>,
    Path(asn): Path<u32>,
    Query(params): Query<PrefixHistoryParams>,
) -> impl IntoResponse {
    // Get ClickHouse client
    let client = match &state.bgp_query_client {
        Some(client) => client,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": "BGP query service is disabled" })),
            )
                .into_response();
        }
    };

    // Build SQL query
    let limit = params.limit.unwrap_or(100).min(1000);
    let where_clause = build_where_clause(
        params.from.as_deref(),
        params.to.as_deref(),
        None,
        None,
        Some(asn),
    );

    let sql = format!(
        "SELECT
            prefix,
            formatDateTime(max(timestamp), '%Y-%m-%dT%H:%i:%SZ') AS last_seen,
            count() AS event_count
        FROM bgp_events
        WHERE {}
        GROUP BY prefix
        ORDER BY event_count DESC
        LIMIT {}",
        where_clause, limit
    );

    // Execute query
    match client.query::<AsnPrefixEntry>(&sql).await {
        Ok(results) => (StatusCode::OK, Json(results)).into_response(),
        Err(error) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "error": error })),
        )
            .into_response(),
    }
}

/// GET /api/v1/bgp/events
#[utoipa::path(
    get,
    path = "/api/v1/bgp/events",
    params(BgpEventsParams),
    responses(
        (status = 200, description = "BGP events retrieved", body = [BgpEventEntry]),
        (status = 400, description = "Invalid event_type"),
        (status = 502, description = "ClickHouse error"),
    ),
    security(("api_key" = []), ("bearer_auth" = []))
)]
pub async fn bgp_events(
    State(state): State<crate::handlers::AppState>,
    Query(params): Query<BgpEventsParams>,
) -> impl IntoResponse {
    // Validate event_type if provided
    if let Some(event_type) = &params.event_type {
        if event_type != "announce" && event_type != "withdraw" {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "Invalid event_type. Must be 'announce' or 'withdraw'"
                })),
            )
                .into_response();
        }
    }

    // Get ClickHouse client
    let client = match &state.bgp_query_client {
        Some(client) => client,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": "BGP query service is disabled" })),
            )
                .into_response();
        }
    };

    // Build SQL query
    let limit = params.limit.unwrap_or(100).min(1000);
    let offset = params.offset.unwrap_or(0);
    let where_clause = build_where_clause(
        params.from.as_deref(),
        params.to.as_deref(),
        params.event_type.as_deref(),
        None,
        None,
    );

    let sql = format!(
        "SELECT
            formatDateTime(timestamp, '%Y-%m-%dT%H:%i:%SZ') AS timestamp,
            event_type,
            prefix,
            origin_as,
            as_path,
            peer_ip
        FROM bgp_events
        WHERE {}
        ORDER BY timestamp DESC
        LIMIT {} OFFSET {}",
        where_clause, limit, offset
    );

    // Execute query
    match client.query::<BgpEventEntry>(&sql).await {
        Ok(results) => (StatusCode::OK, Json(results)).into_response(),
        Err(error) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "error": error })),
        )
            .into_response(),
    }
}

/// GET /api/v1/bgp/stats/top-as
#[utoipa::path(
    get,
    path = "/api/v1/bgp/stats/top-as",
    params(TopAsParams),
    responses(
        (status = 200, description = "Top AS statistics retrieved", body = [TopAsEntry]),
        (status = 502, description = "ClickHouse error"),
    ),
    security(("api_key" = []), ("bearer_auth" = []))
)]
pub async fn bgp_top_as(
    State(state): State<crate::handlers::AppState>,
    Query(params): Query<TopAsParams>,
) -> impl IntoResponse {
    // Get ClickHouse client
    let client = match &state.bgp_query_client {
        Some(client) => client,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": "BGP query service is disabled" })),
            )
                .into_response();
        }
    };

    // Build SQL query
    let limit = params.limit.unwrap_or(10).min(100);
    
    let sql = format!(
        "SELECT
            origin_as,
            count() AS event_count,
            uniqExact(prefix) AS prefix_count
        FROM bgp_events
        GROUP BY origin_as
        ORDER BY event_count DESC
        LIMIT {}",
        limit
    );

    // Execute query
    match client.query::<TopAsEntry>(&sql).await {
        Ok(results) => (StatusCode::OK, Json(results)).into_response(),
        Err(error) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "error": error })),
        )
            .into_response(),
    }
}

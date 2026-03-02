use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use tracing::{error, info};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::handlers::AppState;

/// Response wrapper for alert API endpoints.
#[derive(Debug, Serialize, ToSchema)]
pub struct AlertApiResponse<T> {
    pub data: T,
    pub status: &'static str,
}

/// Error response for alert API endpoints.
#[derive(Debug, Serialize, ToSchema)]
pub struct AlertApiError {
    pub error: String,
    pub status: &'static str,
}

/// Parameters for listing alert rules.
#[derive(Debug, Deserialize, IntoParams, ToSchema)]
pub struct ListRulesParams {
    pub enabled: Option<bool>,
}

/// GET /api/v1/alerts/rules
#[utoipa::path(
    get,
    path = "/api/v1/alerts/rules",
    params(ListRulesParams),
    responses(
        (status = 200, description = "List of alert rules", body = [AlertRule]),
        (status = 503, description = "Alert manager disabled"),
        (status = 500, description = "Internal server error"),
    ),
    security(("api_key" = []), ("bearer_auth" = []))
)]
pub async fn list_rules_handler(State(state): State<AppState>) -> impl IntoResponse {
    let client = match &state.alert_manager_client {
        Some(client) => client,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(AlertApiError {
                    error: "alert_manager_disabled".to_string(),
                    status: "error",
                }),
            )
                .into_response();
        }
    };

    match client.list_rules().await {
        Ok(rules) => (
            StatusCode::OK,
            Json(AlertApiResponse {
                data: rules,
                status: "ok",
            }),
        )
            .into_response(),
        Err(e) => {
            error!("Failed to list alert rules: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(AlertApiError {
                    error: format!("ClickHouse error: {}", e),
                    status: "error",
                }),
            )
                .into_response()
        }
    }
}

/// POST /api/v1/alerts/:id/silence
#[utoipa::path(
    post,
    path = "/api/v1/alerts/{id}/silence",
    params(
        ("id" = Uuid, Path, description = "Alert ID")
    ),
    request_body = SilenceCreate,
    responses(
        (status = 200, description = "Alert silenced", body = Silence),
        (status = 400, description = "Invalid input or UUID"),
        (status = 404, description = "Alert not found"),
        (status = 503, description = "Alert manager disabled"),
        (status = 500, description = "Internal server error"),
    ),
    security(("api_key" = []), ("bearer_auth" = []))
)]
pub async fn silence_alert_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<crate::alert_manager::SilenceCreate>,
) -> impl IntoResponse {
    // Parse UUID
    let alert_id = match Uuid::parse_str(&id) {
        Ok(id) => id,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(AlertApiError {
                    error: format!("Invalid UUID: {}", e),
                    status: "error",
                }),
            )
                .into_response();
        }
    };

    // Validierung: duration_hours 1–168
    if input.duration_hours < 1 || input.duration_hours > 168 {
        return (
            StatusCode::BAD_REQUEST,
            Json(AlertApiError {
                error: format!(
                    "Duration must be between 1 and 168 hours, got {}",
                    input.duration_hours
                ),
                status: "error",
            }),
        )
            .into_response();
    }

    // Validierung: reason nicht leer
    if input.reason.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(AlertApiError {
                error: "Reason cannot be empty".to_string(),
                status: "error",
            }),
        )
            .into_response();
    }

    let client = match &state.alert_manager_client {
        Some(client) => client,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(AlertApiError {
                    error: "alert_manager_disabled".to_string(),
                    status: "error",
                }),
            )
                .into_response();
        }
    };

    match client.create_silence(input).await {
        Ok(silence) => {
            info!("Created silence for alert {}: {}", alert_id, silence.id);
            (
                StatusCode::OK,
                Json(AlertApiResponse {
                    data: silence,
                    status: "ok",
                }),
            )
                .into_response()
        }
        Err(e) => {
            error!("Failed to create silence for alert {}: {}", alert_id, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(AlertApiError {
                    error: format!("ClickHouse error: {}", e),
                    status: "error",
                }),
            )
                .into_response()
        }
    }
}

/// GET /api/v1/alerts/silences
#[utoipa::path(
    get,
    path = "/api/v1/alerts/silences",
    responses(
        (status = 200, description = "List of active silences", body = [Silence]),
        (status = 503, description = "Alert manager disabled"),
        (status = 500, description = "Internal server error"),
    ),
    security(("api_key" = []), ("bearer_auth" = []))
)]
pub async fn list_silences_handler(State(state): State<AppState>) -> impl IntoResponse {
    let client = match &state.alert_manager_client {
        Some(client) => client,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(AlertApiError {
                    error: "alert_manager_disabled".to_string(),
                    status: "error",
                }),
            )
                .into_response();
        }
    };

    match client.list_silences().await {
        Ok(silences) => (
            StatusCode::OK,
            Json(AlertApiResponse {
                data: silences,
                status: "ok",
            }),
        )
            .into_response(),
        Err(e) => {
            error!("Failed to list silences: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(AlertApiError {
                    error: format!("ClickHouse error: {}", e),
                    status: "error",
                }),
            )
                .into_response()
        }
    }
}

/// DELETE /api/v1/alerts/silences/:id
#[utoipa::path(
    delete,
    path = "/api/v1/alerts/silences/{id}",
    params(
        ("id" = Uuid, Path, description = "Silence ID")
    ),
    responses(
        (status = 204, description = "Silence expired"),
        (status = 400, description = "Invalid UUID"),
        (status = 404, description = "Silence not found"),
        (status = 503, description = "Alert manager disabled"),
        (status = 500, description = "Internal server error"),
    ),
    security(("api_key" = []), ("bearer_auth" = []))
)]
pub async fn expire_silence_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    // Parse UUID
    let silence_id = match Uuid::parse_str(&id) {
        Ok(id) => id,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(AlertApiError {
                    error: format!("Invalid UUID: {}", e),
                    status: "error",
                }),
            )
                .into_response();
        }
    };

    let client = match &state.alert_manager_client {
        Some(client) => client,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(AlertApiError {
                    error: "alert_manager_disabled".to_string(),
                    status: "error",
                }),
            )
                .into_response();
        }
    };

    match client.expire_silence(silence_id).await {
        Ok(()) => {
            info!("Expired silence: {}", silence_id);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            if e.to_string().contains("not found") {
                (
                    StatusCode::NOT_FOUND,
                    Json(AlertApiError {
                        error: format!("Silence not found: {}", e),
                        status: "error",
                    }),
                )
                    .into_response()
            } else {
                error!("Failed to expire silence {}: {}", silence_id, e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(AlertApiError {
                        error: format!("ClickHouse error: {}", e),
                        status: "error",
                    }),
                )
                    .into_response()
            }
        }
    }
}

/// POST /api/v1/alerts/rules
#[utoipa::path(
    post,
    path = "/api/v1/alerts/rules",
    request_body = AlertRuleCreate,
    responses(
        (status = 201, description = "Alert rule created", body = AlertRule),
        (status = 400, description = "Invalid input"),
        (status = 503, description = "Alert manager disabled"),
        (status = 500, description = "Internal server error"),
    ),
    security(("api_key" = []), ("bearer_auth" = []))
)]
pub async fn create_rule_handler(
    State(state): State<AppState>,
    Json(input): Json<crate::alert_manager::AlertRuleCreate>,
) -> impl IntoResponse {
    // Validate rule_type
    let valid_types = ["hijack", "flap", "leak", "custom"];
    if !valid_types.contains(&input.rule_type.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(AlertApiError {
                error: format!(
                    "Invalid rule_type: {}. Must be one of: hijack, flap, leak, custom",
                    input.rule_type
                ),
                status: "error",
            }),
        )
            .into_response();
    }

    // Validate threshold
    if input.threshold < 0.0 || input.threshold > 1.0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(AlertApiError {
                error: format!(
                    "Threshold must be between 0.0 and 1.0, got {}",
                    input.threshold
                ),
                status: "error",
            }),
        )
            .into_response();
    }

    let client = match &state.alert_manager_client {
        Some(client) => client,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(AlertApiError {
                    error: "alert_manager_disabled".to_string(),
                    status: "error",
                }),
            )
                .into_response();
        }
    };

    match client.create_rule(input).await {
        Ok(rule) => {
            info!("Created alert rule: {} ({})", rule.name, rule.id);
            (
                StatusCode::CREATED,
                Json(AlertApiResponse {
                    data: rule,
                    status: "ok",
                }),
            )
                .into_response()
        }
        Err(e) => {
            error!("Failed to create alert rule: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(AlertApiError {
                    error: format!("ClickHouse error: {}", e),
                    status: "error",
                }),
            )
                .into_response()
        }
    }
}

/// PUT /api/v1/alerts/rules/:id
#[utoipa::path(
    put,
    path = "/api/v1/alerts/rules/{id}",
    params(
        ("id" = Uuid, Path, description = "Alert rule ID")
    ),
    request_body = AlertRuleUpdate,
    responses(
        (status = 200, description = "Alert rule updated", body = AlertRule),
        (status = 400, description = "Invalid input or UUID"),
        (status = 404, description = "Alert rule not found"),
        (status = 503, description = "Alert manager disabled"),
        (status = 500, description = "Internal server error"),
    ),
    security(("api_key" = []), ("bearer_auth" = []))
)]
pub async fn update_rule_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<crate::alert_manager::AlertRuleUpdate>,
) -> impl IntoResponse {
    // Parse UUID
    let id = match Uuid::parse_str(&id) {
        Ok(id) => id,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(AlertApiError {
                    error: format!("Invalid UUID: {}", e),
                    status: "error",
                }),
            )
                .into_response();
        }
    };

    // Validate threshold if provided
    if let Some(threshold) = input.threshold {
        if threshold < 0.0 || threshold > 1.0 {
            return (
                StatusCode::BAD_REQUEST,
                Json(AlertApiError {
                    error: format!("Threshold must be between 0.0 and 1.0, got {}", threshold),
                    status: "error",
                }),
            )
                .into_response();
        }
    }

    let client = match &state.alert_manager_client {
        Some(client) => client,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(AlertApiError {
                    error: "alert_manager_disabled".to_string(),
                    status: "error",
                }),
            )
                .into_response();
        }
    };

    match client.update_rule(id, input).await {
        Ok(rule) => {
            info!("Updated alert rule: {} ({})", rule.name, rule.id);
            (
                StatusCode::OK,
                Json(AlertApiResponse {
                    data: rule,
                    status: "ok",
                }),
            )
                .into_response()
        }
        Err(e) => {
            if e.to_string().contains("not found") {
                (
                    StatusCode::NOT_FOUND,
                    Json(AlertApiError {
                        error: format!("Alert rule not found: {}", e),
                        status: "error",
                    }),
                )
                    .into_response()
            } else {
                error!("Failed to update alert rule {}: {}", id, e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(AlertApiError {
                        error: format!("ClickHouse error: {}", e),
                        status: "error",
                    }),
                )
                    .into_response()
            }
        }
    }
}

/// DELETE /api/v1/alerts/rules/:id
#[utoipa::path(
    delete,
    path = "/api/v1/alerts/rules/{id}",
    params(
        ("id" = Uuid, Path, description = "Alert rule ID")
    ),
    responses(
        (status = 204, description = "Alert rule deleted"),
        (status = 400, description = "Invalid UUID"),
        (status = 404, description = "Alert rule not found"),
        (status = 503, description = "Alert manager disabled"),
        (status = 500, description = "Internal server error"),
    ),
    security(("api_key" = []), ("bearer_auth" = []))
)]
pub async fn delete_rule_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    // Parse UUID
    let id = match Uuid::parse_str(&id) {
        Ok(id) => id,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(AlertApiError {
                    error: format!("Invalid UUID: {}", e),
                    status: "error",
                }),
            )
                .into_response();
        }
    };

    let client = match &state.alert_manager_client {
        Some(client) => client,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(AlertApiError {
                    error: "alert_manager_disabled".to_string(),
                    status: "error",
                }),
            )
                .into_response();
        }
    };

    match client.delete_rule(id).await {
        Ok(()) => {
            info!("Deleted alert rule: {}", id);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            if e.to_string().contains("not found") {
                (
                    StatusCode::NOT_FOUND,
                    Json(AlertApiError {
                        error: format!("Alert rule not found: {}", e),
                        status: "error",
                    }),
                )
                    .into_response()
            } else {
                error!("Failed to delete alert rule {}: {}", id, e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(AlertApiError {
                        error: format!("ClickHouse error: {}", e),
                        status: "error",
                    }),
                )
                    .into_response()
            }
        }
    }
}

/// GET /api/v1/alerts/active
#[utoipa::path(
    get,
    path = "/api/v1/alerts/active",
    responses(
        (status = 200, description = "List of active alerts", body = [AlertHistoryEntry]),
        (status = 503, description = "Alert manager disabled"),
        (status = 500, description = "Internal server error"),
    ),
    security(("api_key" = []), ("bearer_auth" = []))
)]
pub async fn list_active_alerts_handler(State(state): State<AppState>) -> impl IntoResponse {
    let client = match &state.alert_manager_client {
        Some(client) => client,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(AlertApiError {
                    error: "alert_manager_disabled".to_string(),
                    status: "error",
                }),
            )
                .into_response();
        }
    };

    match client.list_active_alerts().await {
        Ok(alerts) => (
            StatusCode::OK,
            Json(AlertApiResponse {
                data: alerts,
                status: "ok",
            }),
        )
            .into_response(),
        Err(e) => {
            error!("Failed to list active alerts: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(AlertApiError {
                    error: format!("ClickHouse error: {}", e),
                    status: "error",
                }),
            )
                .into_response()
        }
    }
}

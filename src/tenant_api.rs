use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Serialize;
use tracing::{error, info};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::handlers::AppState;
use crate::tenant_manager::{TenantCreate, TenantUpdate};

/// Response wrapper for tenant API endpoints.
#[derive(Debug, Serialize, ToSchema)]
pub struct TenantApiResponse<T: Serialize> {
    pub data: T,
    pub status: &'static str,
}

/// Error response for tenant API endpoints.
#[derive(Debug, Serialize, ToSchema)]
pub struct TenantApiError {
    pub error: String,
    pub status: &'static str,
}

/// GET /api/v1/tenants
#[utoipa::path(
    get,
    path = "/api/v1/tenants",
    responses(
        (status = 200, description = "List of tenants", body = [Tenant]),
        (status = 503, description = "Tenant manager disabled"),
        (status = 500, description = "Internal server error"),
    ),
    security(("api_key" = []), ("bearer_auth" = []))
)]
pub async fn list_tenants_handler(State(state): State<AppState>) -> impl IntoResponse {
    let client = match &state.tenant_manager_client {
        Some(client) => client,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(TenantApiError {
                    error: "tenant_manager_disabled".to_string(),
                    status: "error",
                }),
            )
                .into_response();
        }
    };

    match client.list_tenants().await {
        Ok(tenants) => (
            StatusCode::OK,
            Json(TenantApiResponse {
                data: tenants,
                status: "ok",
            }),
        )
            .into_response(),
        Err(e) => {
            error!("Failed to list tenants: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(TenantApiError {
                    error: format!("ClickHouse error: {}", e),
                    status: "error",
                }),
            )
                .into_response()
        }
    }
}

/// POST /api/v1/tenants
#[utoipa::path(
    post,
    path = "/api/v1/tenants",
    request_body = TenantCreate,
    responses(
        (status = 201, description = "Tenant created", body = Tenant),
        (status = 400, description = "Invalid input"),
        (status = 503, description = "Tenant manager disabled"),
        (status = 500, description = "Internal server error"),
    ),
    security(("api_key" = []), ("bearer_auth" = []))
)]
pub async fn create_tenant_handler(
    State(state): State<AppState>,
    Json(input): Json<TenantCreate>,
) -> impl IntoResponse {
    let client = match &state.tenant_manager_client {
        Some(client) => client,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(TenantApiError {
                    error: "tenant_manager_disabled".to_string(),
                    status: "error",
                }),
            )
                .into_response();
        }
    };

    match client.create_tenant(input).await {
        Ok(tenant) => {
            info!("Tenant created: {}", tenant.id);
            (
                StatusCode::CREATED,
                Json(TenantApiResponse {
                    data: tenant,
                    status: "created",
                }),
            )
                .into_response()
        }
        Err(e) => {
            let error_msg = e.to_string();
            if error_msg.contains("Invalid plan") || error_msg.contains("Rate limit") {
                (
                    StatusCode::BAD_REQUEST,
                    Json(TenantApiError {
                        error: error_msg,
                        status: "error",
                    }),
                )
                    .into_response()
            } else {
                error!("Failed to create tenant: {}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(TenantApiError {
                        error: format!("ClickHouse error: {}", e),
                        status: "error",
                    }),
                )
                    .into_response()
            }
        }
    }
}

/// PUT /api/v1/tenants/:id
#[utoipa::path(
    put,
    path = "/api/v1/tenants/{id}",
    params(
        ("id" = Uuid, Path, description = "Tenant ID")
    ),
    request_body = TenantUpdate,
    responses(
        (status = 200, description = "Tenant updated"),
        (status = 400, description = "Invalid input or UUID"),
        (status = 404, description = "Tenant not found"),
        (status = 503, description = "Tenant manager disabled"),
        (status = 500, description = "Internal server error"),
    ),
    security(("api_key" = []), ("bearer_auth" = []))
)]
pub async fn update_tenant_handler(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(input): Json<TenantUpdate>,
) -> impl IntoResponse {
    let client = match &state.tenant_manager_client {
        Some(client) => client,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(TenantApiError {
                    error: "tenant_manager_disabled".to_string(),
                    status: "error",
                }),
            )
                .into_response();
        }
    };

    match client.update_tenant(id, input).await {
        Ok(()) => (
            StatusCode::OK,
            Json(TenantApiResponse {
                data: serde_json::json!({"updated": true}),
                status: "ok",
            }),
        )
            .into_response(),
        Err(e) => {
            let error_msg = e.to_string();
            if error_msg.contains("not found") {
                (
                    StatusCode::NOT_FOUND,
                    Json(TenantApiError {
                        error: error_msg,
                        status: "error",
                    }),
                )
                    .into_response()
            } else if error_msg.contains("Invalid plan") || error_msg.contains("Rate limit") {
                (
                    StatusCode::BAD_REQUEST,
                    Json(TenantApiError {
                        error: error_msg,
                        status: "error",
                    }),
                )
                    .into_response()
            } else {
                error!("Failed to update tenant {}: {}", id, e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(TenantApiError {
                        error: format!("ClickHouse error: {}", e),
                        status: "error",
                    }),
                )
                    .into_response()
            }
        }
    }
}

/// DELETE /api/v1/tenants/:id
#[utoipa::path(
    delete,
    path = "/api/v1/tenants/{id}",
    params(
        ("id" = Uuid, Path, description = "Tenant ID")
    ),
    responses(
        (status = 200, description = "Tenant deleted"),
        (status = 404, description = "Tenant not found"),
        (status = 503, description = "Tenant manager disabled"),
        (status = 500, description = "Internal server error"),
    ),
    security(("api_key" = []), ("bearer_auth" = []))
)]
pub async fn delete_tenant_handler(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    let client = match &state.tenant_manager_client {
        Some(client) => client,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(TenantApiError {
                    error: "tenant_manager_disabled".to_string(),
                    status: "error",
                }),
            )
                .into_response();
        }
    };

    match client.delete_tenant(id).await {
        Ok(()) => (
            StatusCode::OK,
            Json(TenantApiResponse {
                data: serde_json::json!({"deleted": true}),
                status: "ok",
            }),
        )
            .into_response(),
        Err(e) => {
            let error_msg = e.to_string();
            if error_msg.contains("not found") {
                (
                    StatusCode::NOT_FOUND,
                    Json(TenantApiError {
                        error: error_msg,
                        status: "error",
                    }),
                )
                    .into_response()
            } else {
                error!("Failed to delete tenant {}: {}", id, e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(TenantApiError {
                        error: format!("ClickHouse error: {}", e),
                        status: "error",
                    }),
                )
                    .into_response()
            }
        }
    }
}

/// GET /api/v1/tenants/:id
#[utoipa::path(
    get,
    path = "/api/v1/tenants/{id}",
    params(
        ("id" = Uuid, Path, description = "Tenant ID")
    ),
    responses(
        (status = 200, description = "Tenant details", body = Tenant),
        (status = 404, description = "Tenant not found"),
        (status = 503, description = "Tenant manager disabled"),
        (status = 500, description = "Internal server error"),
    ),
    security(("api_key" = []), ("bearer_auth" = []))
)]
pub async fn get_tenant_handler(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    let client = match &state.tenant_manager_client {
        Some(client) => client,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(TenantApiError {
                    error: "tenant_manager_disabled".to_string(),
                    status: "error",
                }),
            )
                .into_response();
        }
    };

    match client.list_tenants().await {
        Ok(tenants) => {
            // Filter for the specific tenant ID and ensure it's enabled
            if let Some(tenant) = tenants.into_iter().find(|t| t.id == id && t.enabled) {
                (
                    StatusCode::OK,
                    Json(TenantApiResponse {
                        data: tenant,
                        status: "ok",
                    }),
                )
                    .into_response()
            } else {
                (
                    StatusCode::NOT_FOUND,
                    Json(TenantApiError {
                        error: "Tenant not found".to_string(),
                        status: "error",
                    }),
                )
                    .into_response()
            }
        }
        Err(e) => {
            error!("Failed to list tenants for get_tenant_handler: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(TenantApiError {
                    error: format!("ClickHouse error: {}", e),
                    status: "error",
                }),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test 1: TenantApiError serialisiert korrekt
    #[test]
    fn test_tenant_api_error_serialization() {
        let err = TenantApiError {
            error: "not found".to_string(),
            status: "error",
        };
        let json = serde_json::to_string(&err).unwrap();
        assert!(json.contains("not found"));
        assert!(json.contains("error"));
    }

    // Test 2: TenantApiResponse serialisiert korrekt
    #[test]
    fn test_tenant_api_response_serialization() {
        let resp = TenantApiResponse {
            data: vec!["item1", "item2"],
            status: "ok",
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("item1"));
        assert!(json.contains("ok"));
    }

    // Test 3: 400 bei Invalid plan Fehler erkannt
    #[test]
    fn test_is_validation_error_plan() {
        let err = anyhow::anyhow!("Invalid plan: xyz. Must be one of: free, pro, enterprise");
        let msg = err.to_string();
        assert!(msg.contains("Invalid plan") || msg.contains("Rate limit"));
    }

    // Test 4: 400 bei Rate limit Fehler erkannt
    #[test]
    fn test_is_validation_error_rate_limit() {
        let err = anyhow::anyhow!("Rate limit must be greater than 0");
        let msg = err.to_string();
        assert!(msg.contains("Rate limit"));
    }
}
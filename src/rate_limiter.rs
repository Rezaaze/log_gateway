use governor::{
    clock::DefaultClock, middleware::NoOpMiddleware, state::keyed::DashMapStateStore, Quota,
    RateLimiter,
};
use std::num::NonZeroU32;
use std::sync::Arc;

// Per-tenant rate limiter
pub type TenantLimiter =
    Arc<RateLimiter<String, DashMapStateStore<String>, DefaultClock, NoOpMiddleware>>;

pub fn new_tenant_limiter(requests_per_second: u32) -> TenantLimiter {
    let quota = Quota::per_second(NonZeroU32::new(requests_per_second).expect("rps must be > 0"));
    Arc::new(RateLimiter::dashmap(quota))
}

use crate::metrics::GatewayMetrics;
use axum::{
    extract::Request, http::StatusCode, middleware::Next, response::Response, Extension, Json,
};
use once_cell::sync::Lazy;
use regex::Regex;

static TENANT_ID_REGEX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[a-zA-Z0-9_\-]+$").expect("Failed to compile tenant ID regex"));

/// Validates tenant ID from X-Tenant-ID header
/// Returns validated tenant ID or error response
fn validate_tenant_id(tenant_id: &str) -> Result<String, (StatusCode, Json<serde_json::Value>)> {
    // Check length
    if tenant_id.len() > 64 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "invalid_tenant_id",
                "hint": "tenant ID must be at most 64 characters"
            })),
        ));
    }

    // Check characters: alphanumeric, hyphen, underscore
    if !TENANT_ID_REGEX.is_match(tenant_id) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "invalid_tenant_id",
                "hint": "tenant ID must contain only alphanumeric characters, hyphens, and underscores"
            })),
        ));
    }

    Ok(tenant_id.to_string())
}

pub async fn rate_limit_middleware(
    Extension(limiter): Extension<TenantLimiter>,
    Extension(metrics): Extension<Arc<GatewayMetrics>>,
    request: Request,
    next: Next,
) -> Result<Response, (StatusCode, Json<serde_json::Value>)> {
    // Extract tenant ID from X-Tenant-ID header, fallback to "anonymous"
    let tenant_id = request
        .headers()
        .get("X-Tenant-ID")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("anonymous");

    // Validate tenant ID
    let tenant_id = match validate_tenant_id(tenant_id) {
        Ok(id) => id,
        Err(err) => return Err(err),
    };

    // Check rate limit for this tenant
    match limiter.check_key(&tenant_id) {
        Ok(_) => Ok(next.run(request).await),
        Err(_) => {
            metrics.record_rate_limit_hit();
            Err((
                StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({
                    "error": "rate_limit_exceeded",
                    "hint": "too many requests, please slow down"
                })),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        routing::get,
        Router,
    };
    use std::sync::Arc;
    use tower::ServiceExt;

    #[test]
    fn test_tenant_limiter_allows_first_request() {
        let limiter = new_tenant_limiter(1);
        // Erster Request für neuen Tenant wird erlaubt
        assert!(limiter.check_key(&"tenant-a".to_string()).is_ok());
    }

    #[test]
    fn test_tenant_limiter_rejects_over_limit() {
        let limiter = new_tenant_limiter(1);
        // Ersten Request verbrauchen
        assert!(limiter.check_key(&"tenant-a".to_string()).is_ok());
        // Zweiter Request bei 1 req/s wird rejected
        assert!(limiter.check_key(&"tenant-a".to_string()).is_err());
    }

    #[test]
    fn test_tenant_limiter_isolates_tenants() {
        let limiter = new_tenant_limiter(1);
        // Tenant A erschöpft
        assert!(limiter.check_key(&"tenant-a".to_string()).is_ok());
        assert!(limiter.check_key(&"tenant-a".to_string()).is_err());

        // Tenant B wird noch erlaubt (isoliert)
        assert!(limiter.check_key(&"tenant-b".to_string()).is_ok());
    }

    #[test]
    fn test_invalid_tenant_header_rejected() {
        // Test invalid characters
        let result = validate_tenant_id("tenant@invalid");
        assert!(result.is_err());

        // Test too long
        let long_id = "a".repeat(65);
        let result = validate_tenant_id(&long_id);
        assert!(result.is_err());

        // Test valid tenant ID
        let result = validate_tenant_id("tenant-123_valid");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "tenant-123_valid");
    }

    #[tokio::test]
    async fn test_rate_limit_middleware_with_tenant() {
        let limiter = new_tenant_limiter(1);
        let metrics = Arc::new(GatewayMetrics::new());

        async fn dummy_handler() -> &'static str {
            "OK"
        }

        use tower::ServiceBuilder;

        let app = Router::new().route("/", get(dummy_handler)).layer(
            ServiceBuilder::new()
                .layer(axum::Extension(limiter.clone()))
                .layer(axum::Extension(metrics.clone()))
                .layer(axum::middleware::from_fn(rate_limit_middleware)),
        );

        // First request with tenant-a should succeed
        let request = Request::builder()
            .uri("/")
            .header("X-Tenant-ID", "tenant-a")
            .body(Body::empty())
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        println!("First request status: {}", response.status());
        assert_eq!(response.status(), StatusCode::OK);

        // Second request with same tenant should be rate limited
        let request = Request::builder()
            .uri("/")
            .header("X-Tenant-ID", "tenant-a")
            .body(Body::empty())
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        println!("Second request status: {}", response.status());
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);

        // Different tenant should still work
        let request = Request::builder()
            .uri("/")
            .header("X-Tenant-ID", "tenant-b")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        println!("Third request status: {}", response.status());
        assert_eq!(response.status(), StatusCode::OK);
    }
}

use axum::{
    extract::Request, extract::State, http::StatusCode, middleware::Next, response::Response, Json,
};
use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};

use crate::handlers::AppState;

/// Axum middleware that validates X-API-Key header.
/// Reads the expected key from AppState (cached at startup) — no per-request disk I/O.
/// Passes through if api_key is None (auth disabled).
pub async fn require_api_key(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, (StatusCode, Json<serde_json::Value>)> {
    let expected_key = match &state.api_key {
        Some(key) => key.clone(),
        None => return Ok(next.run(request).await), // auth disabled
    };

    let provided_key = request
        .headers()
        .get("X-API-Key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if provided_key != expected_key.as_str() {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": "unauthorized",
                "hint": "provide valid X-API-Key header"
            })),
        ));
    }

    Ok(next.run(request).await)
}

/// JWT claims structure for HS256 tokens
#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: String,
    exp: usize,
}

/// Axum middleware that validates JWT Bearer tokens.
/// Reads the JWT secret from AppState (cached at startup) — no per-request disk I/O.
/// Passes through if jwt_secret is None (auth disabled).
pub async fn require_jwt(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, (StatusCode, Json<serde_json::Value>)> {
    let secret = match &state.jwt_secret {
        Some(s) => s.clone(),
        None => return Ok(next.run(request).await), // JWT auth disabled
    };

    // Extract Bearer token from Authorization header
    let auth_header = request
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if !auth_header.starts_with("Bearer ") {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": "unauthorized",
                "hint": "provide valid Bearer token"
            })),
        ));
    }

    let token = &auth_header[7..]; // Skip "Bearer "

    // Validate JWT token
    let decoding_key = DecodingKey::from_secret(secret.as_bytes()); // Arc<String> deref to str
    let validation = Validation::new(Algorithm::HS256);

    match decode::<Claims>(token, &decoding_key, &validation) {
        Ok(token_data) => {
            // Add subject claim as X-JWT-Subject header
            let subject = token_data.claims.sub;
            request.headers_mut().insert(
                "X-JWT-Subject",
                axum::http::HeaderValue::from_str(&subject).unwrap(),
            );
            Ok(next.run(request).await)
        }
        Err(_) => Err((
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": "unauthorized",
                "hint": "provide valid Bearer token"
            })),
        )),
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
    use jsonwebtoken::{encode, EncodingKey, Header};
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tower::ServiceExt;

    use std::sync::Arc;

    use crate::cache::SemanticCache;
    use crate::cost_tracker::CostTracker;
    use crate::metrics::GatewayMetrics;
    use crate::redactor::Redactor;

    fn make_state(api_key: Option<&str>, jwt_secret: Option<&str>) -> AppState {
        AppState {
            redactor: Redactor::new(),
            cache: SemanticCache::new(10, 60),
            cost_tracker: CostTracker::new(),
            metrics: GatewayMetrics::new(),
            sink: None,
            s3_exporter: None,
            sink_output_dir: PathBuf::from("data/logs"),
            started_at: std::time::Instant::now(),
            api_key: api_key.map(|k| Arc::new(k.to_string())),
            jwt_secret: jwt_secret.map(|s| Arc::new(s.to_string())),
        }
    }

    async fn dummy_handler() -> &'static str {
        "OK"
    }

    #[tokio::test]
    async fn test_auth_disabled_when_no_env_var() {
        let state = make_state(None, None); // api_key = None → auth disabled

        let app = Router::new()
            .route("/", get(dummy_handler))
            .route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                require_api_key,
            ))
            .with_state(state);

        let request = Request::builder().uri("/").body(Body::empty()).unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_auth_rejects_wrong_key() {
        let state = make_state(Some("correct-key"), None);

        let app = Router::new()
            .route("/", get(dummy_handler))
            .route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                require_api_key,
            ))
            .with_state(state);

        let request = Request::builder()
            .uri("/")
            .header("X-API-Key", "wrong-key")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_auth_passes_correct_key() {
        let state = make_state(Some("correct-key"), None);

        let app = Router::new()
            .route("/", get(dummy_handler))
            .route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                require_api_key,
            ))
            .with_state(state);

        let request = Request::builder()
            .uri("/")
            .header("X-API-Key", "correct-key")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_auth_rejects_missing_key_header() {
        let state = make_state(Some("correct-key"), None);

        let app = Router::new()
            .route("/", get(dummy_handler))
            .route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                require_api_key,
            ))
            .with_state(state);

        let request = Request::builder().uri("/").body(Body::empty()).unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_jwt_disabled_when_no_env_var() {
        let state = make_state(None, None); // jwt_secret = None → JWT disabled

        let app = Router::new()
            .route("/", get(dummy_handler))
            .route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                require_jwt,
            ))
            .with_state(state);

        let request = Request::builder().uri("/").body(Body::empty()).unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_jwt_rejects_missing_token() {
        let state = make_state(None, Some("test-secret"));

        let app = Router::new()
            .route("/", get(dummy_handler))
            .route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                require_jwt,
            ))
            .with_state(state);

        let request = Request::builder().uri("/").body(Body::empty()).unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_jwt_rejects_invalid_token() {
        let state = make_state(None, Some("test-secret"));

        let app = Router::new()
            .route("/", get(dummy_handler))
            .route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                require_jwt,
            ))
            .with_state(state);

        let request = Request::builder()
            .uri("/")
            .header("Authorization", "Bearer invalid-token")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_jwt_accepts_valid_token() {
        let secret = "test-secret";
        let state = make_state(None, Some(secret));

        // Create a valid JWT token
        let claims = Claims {
            sub: "test-user".to_string(),
            exp: (SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as usize)
                + 3600, // 1 hour from now
        };

        let token = encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap();

        // Create a handler that checks for the X-JWT-Subject header
        async fn check_jwt_subject_header(request: axum::extract::Request) -> &'static str {
            let headers = request.headers();
            if let Some(subject) = headers.get("X-JWT-Subject") {
                assert_eq!(subject, "test-user");
            } else {
                panic!("X-JWT-Subject header not found");
            }
            "OK"
        }

        let app = Router::new()
            .route("/", get(check_jwt_subject_header))
            .route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                require_jwt,
            ))
            .with_state(state);

        let request = Request::builder()
            .uri("/")
            .header("Authorization", format!("Bearer {}", token))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}

use axum::{extract::Request, http::StatusCode, middleware::Next, response::Response, Json};
use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};

/// Axum middleware that validates X-API-Key header.
/// Passes through if api_key is None (auth disabled).
pub async fn require_api_key(
    request: Request,
    next: Next,
) -> Result<Response, (StatusCode, Json<serde_json::Value>)> {
    // Read expected key from Docker secrets or environment variable
    // If neither is set, auth is disabled
    let expected_key = match crate::secrets::read_secret("gateway_api_key", "GATEWAY_API_KEY") {
        Some(key) => key,
        None => return Ok(next.run(request).await), // auth disabled
    };

    let provided_key = request
        .headers()
        .get("X-API-Key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if provided_key != expected_key {
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
/// Passes through if GATEWAY_JWT_SECRET is not set (auth disabled).
pub async fn require_jwt(
    mut request: Request,
    next: Next,
) -> Result<Response, (StatusCode, Json<serde_json::Value>)> {
    // Read JWT secret from Docker secrets or environment variable
    // If neither is set, JWT auth is disabled
    let secret = match crate::secrets::read_secret("gateway_jwt_secret", "GATEWAY_JWT_SECRET") {
        Some(secret) => secret,
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
    let decoding_key = DecodingKey::from_secret(secret.as_bytes());
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
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::sync::Mutex;
    use tower::ServiceExt;

    // Mutex to prevent tests from interfering with each other via env vars
    static ENV_MUTEX: Mutex<()> = Mutex::const_new(());

    async fn dummy_handler() -> &'static str {
        "OK"
    }

    #[tokio::test]
    async fn test_auth_disabled_when_no_env_var() {
        let _guard = ENV_MUTEX.lock().await;

        // Ensure env var is not set
        std::env::remove_var("GATEWAY_API_KEY");

        let app = Router::new()
            .route("/", get(dummy_handler))
            .layer(axum::middleware::from_fn(require_api_key));

        let request = Request::builder().uri("/").body(Body::empty()).unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_auth_rejects_wrong_key() {
        let _guard = ENV_MUTEX.lock().await;

        std::env::set_var("GATEWAY_API_KEY", "correct-key");

        let app = Router::new()
            .route("/", get(dummy_handler))
            .layer(axum::middleware::from_fn(require_api_key));

        let request = Request::builder()
            .uri("/")
            .header("X-API-Key", "wrong-key")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        // Clean up
        std::env::remove_var("GATEWAY_API_KEY");
    }

    #[tokio::test]
    async fn test_auth_passes_correct_key() {
        let _guard = ENV_MUTEX.lock().await;

        std::env::set_var("GATEWAY_API_KEY", "correct-key");

        let app = Router::new()
            .route("/", get(dummy_handler))
            .layer(axum::middleware::from_fn(require_api_key));

        let request = Request::builder()
            .uri("/")
            .header("X-API-Key", "correct-key")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Clean up
        std::env::remove_var("GATEWAY_API_KEY");
    }

    #[tokio::test]
    async fn test_auth_rejects_missing_key_header() {
        let _guard = ENV_MUTEX.lock().await;

        std::env::set_var("GATEWAY_API_KEY", "correct-key");

        let app = Router::new()
            .route("/", get(dummy_handler))
            .layer(axum::middleware::from_fn(require_api_key));

        let request = Request::builder().uri("/").body(Body::empty()).unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        // Clean up
        std::env::remove_var("GATEWAY_API_KEY");
    }

    #[tokio::test]
    async fn test_jwt_disabled_when_no_env_var() {
        let _guard = ENV_MUTEX.lock().await;

        // Ensure env var is not set
        std::env::remove_var("GATEWAY_JWT_SECRET");

        let app = Router::new()
            .route("/", get(dummy_handler))
            .layer(axum::middleware::from_fn(require_jwt));

        let request = Request::builder().uri("/").body(Body::empty()).unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_jwt_rejects_missing_token() {
        let _guard = ENV_MUTEX.lock().await;

        std::env::set_var("GATEWAY_JWT_SECRET", "test-secret");

        let app = Router::new()
            .route("/", get(dummy_handler))
            .layer(axum::middleware::from_fn(require_jwt));

        let request = Request::builder().uri("/").body(Body::empty()).unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        // Clean up
        std::env::remove_var("GATEWAY_JWT_SECRET");
    }

    #[tokio::test]
    async fn test_jwt_rejects_invalid_token() {
        let _guard = ENV_MUTEX.lock().await;

        std::env::set_var("GATEWAY_JWT_SECRET", "test-secret");

        let app = Router::new()
            .route("/", get(dummy_handler))
            .layer(axum::middleware::from_fn(require_jwt));

        let request = Request::builder()
            .uri("/")
            .header("Authorization", "Bearer invalid-token")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        // Clean up
        std::env::remove_var("GATEWAY_JWT_SECRET");
    }

    #[tokio::test]
    async fn test_jwt_accepts_valid_token() {
        let _guard = ENV_MUTEX.lock().await;

        let secret = "test-secret";
        std::env::set_var("GATEWAY_JWT_SECRET", secret);

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
            .layer(axum::middleware::from_fn(require_jwt));

        let request = Request::builder()
            .uri("/")
            .header("Authorization", format!("Bearer {}", token))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Clean up
        std::env::remove_var("GATEWAY_JWT_SECRET");
    }
}

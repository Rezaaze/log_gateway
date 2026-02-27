use anyhow::Result;
use log_gateway::config::GatewayConfig;
use reqwest::{Client, StatusCode};
use serde_json::json;
use std::env;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// Spawn a test server on a random port
/// Returns (base_url, server_handle)
async fn spawn_test_server() -> Result<(String, JoinHandle<()>)> {
    // Clear auth environment variables to disable authentication
    env::remove_var("GATEWAY_API_KEY");
    env::remove_var("GATEWAY_JWT_SECRET");

    // Load default config but modify for testing
    let mut config = GatewayConfig::load()?;
    // Disable sink to avoid writing files during tests
    config.sink.enabled = false;
    // Disable S3
    config.s3.enabled = false;
    // Disable rate limiting for tests
    config.rate_limit.enabled = false;

    // Create the app using the test function from library
    let app = log_gateway::create_test_app(config)?;

    // Bind to random port
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let base_url = format!("http://{}", addr);

    let handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("Server failed to start");
    });

    // Give server a moment to start
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    Ok((base_url, handle))
}

#[tokio::test]
async fn test_health_endpoint() -> Result<()> {
    let (base_url, handle) = spawn_test_server().await?;
    let client = Client::new();

    let response = client.get(format!("{}/health", base_url)).send().await?;

    assert_eq!(response.status(), StatusCode::OK);

    let body: serde_json::Value = response.json().await?;
    assert_eq!(body["status"], "ok");
    assert!(body.get("version").is_some());

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn test_ingest_log_success() -> Result<()> {
    let (base_url, handle) = spawn_test_server().await?;
    let client = Client::new();

    let response = client
        .post(format!("{}/api/v1/logs", base_url))
        .json(&json!({
            "level": "info",
            "source": "integration-test",
            "message": "hello world"
        }))
        .send()
        .await?;

    assert_eq!(response.status(), StatusCode::ACCEPTED);

    let body: serde_json::Value = response.json().await?;
    assert!(body.get("id").is_some());
    assert_eq!(body["status"], "accepted");

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn test_ingest_log_invalid_schema() -> Result<()> {
    let (base_url, handle) = spawn_test_server().await?;
    let client = Client::new();

    let response = client
        .post(format!("{}/api/v1/logs", base_url))
        .json(&json!({
            "source": "test",
            "message": "missing level"
        }))
        .send()
        .await?;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn test_ingest_log_pii_redaction() -> Result<()> {
    let (base_url, handle) = spawn_test_server().await?;
    let client = Client::new();

    let response = client
        .post(format!("{}/api/v1/logs", base_url))
        .json(&json!({
            "level": "info",
            "source": "test",
            "message": "email: user@example.com"
        }))
        .send()
        .await?;

    assert_eq!(response.status(), StatusCode::ACCEPTED);

    let body: serde_json::Value = response.json().await?;
    // Verify exactly 1 PII pattern was detected (the email address)
    assert_eq!(
        body["pii_hits"], 1,
        "Expected exactly 1 PII hit for the email address"
    );

    // Verify the PII detection is reflected in Prometheus metrics
    let metrics_response = client.get(format!("{}/metrics", base_url)).send().await?;
    assert_eq!(metrics_response.status(), StatusCode::OK);
    let metrics_body = metrics_response.text().await?;

    // gateway_pii_hits_total must be present and non-zero after processing a PII-containing log
    assert!(
        metrics_body.contains("gateway_pii_hits_total"),
        "Expected gateway_pii_hits_total metric to be present"
    );
    // Extract the counter value to confirm it's > 0
    let pii_metric_line = metrics_body
        .lines()
        .find(|l| l.starts_with("gateway_pii_hits_total"))
        .expect("gateway_pii_hits_total metric line not found");
    let pii_count: f64 = pii_metric_line
        .split_whitespace()
        .last()
        .and_then(|v| v.parse().ok())
        .expect("Could not parse pii_hits_total value");
    assert!(
        pii_count >= 1.0,
        "Expected pii_hits_total >= 1, got {pii_count}"
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn test_cache_hit_on_repeat_request() -> Result<()> {
    let (base_url, handle) = spawn_test_server().await?;
    let client = Client::new();

    let payload = json!({
        "level": "info",
        "source": "cache-test",
        "message": "same message for cache test"
    });

    // First request — cache MISS
    let response1 = client
        .post(format!("{}/api/v1/logs", base_url))
        .json(&payload)
        .send()
        .await?;
    assert_eq!(response1.status(), StatusCode::ACCEPTED);

    // Second request — should be a cache HIT
    let response2 = client
        .post(format!("{}/api/v1/logs", base_url))
        .json(&payload)
        .send()
        .await?;
    assert_eq!(response2.status(), StatusCode::ACCEPTED);

    // Each request gets its own UUID regardless of cache hit/miss
    let body1: serde_json::Value = response1.json().await?;
    let body2: serde_json::Value = response2.json().await?;
    assert!(body1.get("id").is_some());
    assert!(body2.get("id").is_some());

    // Verify via cache stats that a hit actually occurred
    let stats_response = client
        .get(format!("{}/api/v1/cache/stats", base_url))
        .send()
        .await?;
    assert_eq!(stats_response.status(), StatusCode::OK);

    let stats: serde_json::Value = stats_response.json().await?;
    let hits = stats["cache_hits"].as_u64().unwrap_or(0);
    let misses = stats["cache_misses"].as_u64().unwrap_or(0);

    // After 2 identical requests: 1 miss (first) + 1 hit (second)
    assert_eq!(misses, 1, "Expected exactly 1 cache miss (first request)");
    assert_eq!(hits, 1, "Expected exactly 1 cache hit (second request)");

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn test_metrics_endpoint() -> Result<()> {
    let (base_url, handle) = spawn_test_server().await?;
    let client = Client::new();

    let response = client.get(format!("{}/metrics", base_url)).send().await?;

    assert_eq!(response.status(), StatusCode::OK);

    let content_type = response.headers().get("content-type").unwrap().to_str()?;
    assert!(content_type.contains("text/plain"));

    let body = response.text().await?;
    assert!(body.contains("gateway_requests_total"));

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn test_swagger_ui_accessible() -> Result<()> {
    let (base_url, handle) = spawn_test_server().await?;
    let client = Client::new();

    // Test Swagger UI redirect
    let response = client
        .get(format!("{}/swagger-ui/", base_url))
        .send()
        .await?;

    // Should be either 200 OK or 301/302 redirect
    assert!(response.status().is_success() || response.status().is_redirection());

    // Test OpenAPI JSON
    let response = client
        .get(format!("{}/api-docs/openapi.json", base_url))
        .send()
        .await?;

    assert_eq!(response.status(), StatusCode::OK);

    let body: serde_json::Value = response.json().await?;
    assert!(body.get("openapi").is_some());
    assert!(body["info"]["title"]
        .as_str()
        .unwrap()
        .contains("Log Gateway"));

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn test_cost_summary_endpoint() -> Result<()> {
    let (base_url, handle) = spawn_test_server().await?;
    let client = Client::new();

    // Send a log with tenant ID
    let ingest_response = client
        .post(format!("{}/api/v1/logs", base_url))
        .header("X-Tenant-ID", "test-tenant")
        .json(&json!({
            "level": "info",
            "source": "cost-test",
            "message": "test message for cost tracking"
        }))
        .send()
        .await?;
    assert_eq!(ingest_response.status(), StatusCode::ACCEPTED);

    // Cost tracking is synchronous in the ingest handler — no sleep needed.
    // Poll with timeout to be resilient under load (max 500ms, 10ms interval).
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(500);
    let mut found = false;
    while tokio::time::Instant::now() < deadline {
        let cost_response = client
            .get(format!("{}/api/v1/costs", base_url))
            .send()
            .await?;
        assert_eq!(cost_response.status(), StatusCode::OK);

        let body: serde_json::Value = cost_response.json().await?;
        let tenants = body["tenants"].as_array().unwrap();

        if let Some(tenant) = tenants.iter().find(|t| t["tenant_id"] == "test-tenant") {
            // Verify the tracked data is meaningful, not just presence
            assert!(
                tenant["total_requests"].as_u64().unwrap_or(0) >= 1,
                "Expected at least 1 request tracked for test-tenant"
            );
            assert!(
                tenant["total_bytes_ingested"].as_u64().unwrap_or(0) > 0,
                "Expected bytes_ingested > 0 for test-tenant"
            );
            found = true;
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }

    assert!(
        found,
        "Expected cost tracked under 'test-tenant' within 500ms"
    );

    handle.abort();
    Ok(())
}

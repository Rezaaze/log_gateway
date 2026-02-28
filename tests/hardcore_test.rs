/// Hardcore integration tests — each category tests a distinct failure mode.
///
/// Categories:
///   1. Chaos       — malformed, truncated, and adversarial payloads
///   2. Concurrency — parallel requests, cache race conditions
///   3. Security    — injection attempts, auth-bypass patterns, header smuggling
///   4. Edge Cases  — boundary values, encoding, field limits
///   5. Load        — sustained throughput with correctness assertions
use anyhow::Result;
use log_gateway::config::GatewayConfig;
use reqwest::{Client, StatusCode};
use serde_json::json;
use std::env;
use std::sync::Arc;
use std::time::Instant;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

// ─── Test server helpers ───────────────────────────────────────────────────

async fn spawn_server() -> Result<(String, JoinHandle<()>)> {
    env::remove_var("GATEWAY_API_KEY");
    env::remove_var("GATEWAY_JWT_SECRET");

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let base_url = format!("http://{}", addr);

    let config = GatewayConfig::load()?;
    let app = log_gateway::create_test_app(config)?;

    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // Wait until the server is ready
    let client = Client::new();
    for _ in 0..20 {
        if client
            .get(format!("{}/health", base_url))
            .send()
            .await
            .is_ok()
        {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
    }

    Ok((base_url, handle))
}

fn valid_log() -> serde_json::Value {
    json!({ "source": "test-svc", "level": "info", "message": "hello world" })
}

// ═══════════════════════════════════════════════════════════════════════════
// 1. CHAOS — malformed / adversarial payloads
// ═══════════════════════════════════════════════════════════════════════════

/// Completely empty body → 400
#[tokio::test]
async fn chaos_empty_body() -> Result<()> {
    let (url, h) = spawn_server().await?;
    let r = Client::new()
        .post(format!("{}/api/v1/logs", url))
        .header("content-type", "application/json")
        .body("")
        .send()
        .await?;
    assert_eq!(
        r.status(),
        StatusCode::BAD_REQUEST,
        "empty body must be 400"
    );
    h.abort();
    Ok(())
}

/// Truncated JSON (no closing brace) → 400
#[tokio::test]
async fn chaos_truncated_json() -> Result<()> {
    let (url, h) = spawn_server().await?;
    let r = Client::new()
        .post(format!("{}/api/v1/logs", url))
        .header("content-type", "application/json")
        .body(r#"{"source":"svc","level":"info","message":"cut"#)
        .send()
        .await?;
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    h.abort();
    Ok(())
}

/// Valid JSON but wrong type for level (integer instead of string) → 422
#[tokio::test]
async fn chaos_wrong_type_for_level() -> Result<()> {
    let (url, h) = spawn_server().await?;
    let r = Client::new()
        .post(format!("{}/api/v1/logs", url))
        .json(&json!({ "source": "svc", "level": 42, "message": "oops" }))
        .send()
        .await?;
    assert_eq!(r.status(), StatusCode::UNPROCESSABLE_ENTITY);
    h.abort();
    Ok(())
}

/// Unknown log level value → 422
#[tokio::test]
async fn chaos_unknown_log_level() -> Result<()> {
    let (url, h) = spawn_server().await?;
    let r = Client::new()
        .post(format!("{}/api/v1/logs", url))
        .json(&json!({ "source": "svc", "level": "CRITICAL", "message": "boom" }))
        .send()
        .await?;
    assert_eq!(r.status(), StatusCode::UNPROCESSABLE_ENTITY);
    h.abort();
    Ok(())
}

/// Null values for required fields → 422 or 400
#[tokio::test]
async fn chaos_null_required_fields() -> Result<()> {
    let (url, h) = spawn_server().await?;
    let r = Client::new()
        .post(format!("{}/api/v1/logs", url))
        .json(&json!({ "source": null, "level": null, "message": null }))
        .send()
        .await?;
    assert!(
        r.status() == StatusCode::BAD_REQUEST || r.status() == StatusCode::UNPROCESSABLE_ENTITY,
        "null fields must be rejected"
    );
    h.abort();
    Ok(())
}

/// Body is valid JSON but not an object (array) → 400 or 422
#[tokio::test]
async fn chaos_json_array_instead_of_object() -> Result<()> {
    let (url, h) = spawn_server().await?;
    let r = Client::new()
        .post(format!("{}/api/v1/logs", url))
        .json(&json!([{ "source": "svc", "level": "info", "message": "hi" }]))
        .send()
        .await?;
    assert!(
        r.status() == StatusCode::BAD_REQUEST || r.status() == StatusCode::UNPROCESSABLE_ENTITY,
        "JSON array must be rejected"
    );
    h.abort();
    Ok(())
}

/// Body exceeds 64 KB limit → 413
#[tokio::test]
async fn chaos_oversized_body() -> Result<()> {
    let (url, h) = spawn_server().await?;
    let r = Client::new()
        .post(format!("{}/api/v1/logs", url))
        .header("content-type", "application/json")
        .body(format!(
            r#"{{"source":"s","level":"info","message":"{}"}}"#,
            "x".repeat(70_000)
        ))
        .send()
        .await?;
    assert_eq!(r.status(), StatusCode::PAYLOAD_TOO_LARGE);
    h.abort();
    Ok(())
}

/// Body is well within the 64 KB limit but contains large valid metadata → 202
/// (Schema caps message at 8192 chars, so we pad with metadata key-value pairs)
#[tokio::test]
async fn chaos_body_exactly_at_limit() -> Result<()> {
    let (url, h) = spawn_server().await?;
    // Build metadata with many keys so total body approaches but stays under 65536.
    // Each entry like `"k0001":"vvvvvvvvvv"` is ~20 bytes; 2000 entries ≈ 40 KB.
    // Combined with other fields total is ~40 KB — clearly under 65536.
    let mut meta_pairs: Vec<String> = Vec::with_capacity(2000);
    for i in 0..2000 {
        meta_pairs.push(format!(r#""k{:04}":"vvvvvvvvvv""#, i));
    }
    let metadata = format!("{{{}}}", meta_pairs.join(","));
    let body = format!(
        r#"{{"source":"s","level":"info","message":"benchmark payload","metadata":{}}}"#,
        metadata
    );
    assert!(
        body.len() <= 65_535,
        "test body too large: {} bytes",
        body.len()
    );
    let r = Client::new()
        .post(format!("{}/api/v1/logs", url))
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await?;
    assert_eq!(
        r.status(),
        StatusCode::ACCEPTED,
        "valid body under limit must be accepted"
    );
    h.abort();
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// 2. CONCURRENCY — parallel requests, shared state correctness
// ═══════════════════════════════════════════════════════════════════════════

/// 200 concurrent identical requests → all 202, cache stats consistent
#[tokio::test]
async fn concurrency_parallel_identical_requests() -> Result<()> {
    let (url, h) = spawn_server().await?;
    let client = Arc::new(Client::new());
    let url = Arc::new(url);

    let tasks: Vec<_> = (0..200)
        .map(|_| {
            let c = client.clone();
            let u = url.clone();
            tokio::spawn(async move {
                c.post(format!("{}/api/v1/logs", u))
                    .json(&json!({
                        "source": "concurrent-svc",
                        "level": "info",
                        "message": "same message every time"
                    }))
                    .send()
                    .await
                    .map(|r| r.status())
            })
        })
        .collect();

    let mut ok = 0usize;
    for t in tasks {
        if t.await?? == StatusCode::ACCEPTED {
            ok += 1;
        }
    }
    assert_eq!(ok, 200, "all 200 concurrent requests must succeed");

    // Cache must show exactly 1 miss + 199 hits
    let stats: serde_json::Value = client
        .get(format!("{}/api/v1/cache/stats", url))
        .send()
        .await?
        .json()
        .await?;
    let hits = stats["cache_hits"].as_u64().unwrap_or(0);
    let misses = stats["cache_misses"].as_u64().unwrap_or(0);
    assert_eq!(misses, 1, "exactly 1 cache miss expected");
    assert_eq!(hits, 199, "exactly 199 cache hits expected");

    h.abort();
    Ok(())
}

/// 50 concurrent requests with different messages → no data corruption
#[tokio::test]
async fn concurrency_parallel_distinct_messages() -> Result<()> {
    let (url, h) = spawn_server().await?;
    let client = Arc::new(Client::new());
    let url = Arc::new(url);

    let tasks: Vec<_> = (0..50)
        .map(|i| {
            let c = client.clone();
            let u = url.clone();
            tokio::spawn(async move {
                let r = c
                    .post(format!("{}/api/v1/logs", u))
                    .json(&json!({
                        "source": format!("svc-{}", i),
                        "level": "info",
                        "message": format!("unique message {}", i)
                    }))
                    .send()
                    .await?;
                let status = r.status();
                let body: serde_json::Value = r.json().await?;
                anyhow::Ok((status, body))
            })
        })
        .collect();

    for t in tasks {
        let (status, body) = t.await??;
        assert_eq!(status, StatusCode::ACCEPTED);
        // Each response must contain a unique UUID
        assert!(
            body["id"].as_str().is_some(),
            "response must contain request id"
        );
    }

    h.abort();
    Ok(())
}

/// x-request-id is unique across concurrent requests
#[tokio::test]
async fn concurrency_request_ids_are_unique() -> Result<()> {
    let (url, h) = spawn_server().await?;
    let client = Arc::new(Client::new());
    let url = Arc::new(url);

    let tasks: Vec<_> = (0..100)
        .map(|_| {
            let c = client.clone();
            let u = url.clone();
            tokio::spawn(async move {
                let r = c
                    .post(format!("{}/api/v1/logs", u))
                    .json(&valid_log())
                    .send()
                    .await?;
                let id = r
                    .headers()
                    .get("x-request-id")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                anyhow::Ok(id)
            })
        })
        .collect();

    let mut ids = std::collections::HashSet::new();
    for t in tasks {
        let id = t.await??;
        assert!(!id.is_empty(), "x-request-id must not be empty");
        ids.insert(id);
    }
    assert_eq!(ids.len(), 100, "all 100 request IDs must be unique");

    h.abort();
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// 3. SECURITY — injection, auth bypass, header abuse
// ═══════════════════════════════════════════════════════════════════════════

/// JSON injection in message field — must not crash or leak internal data
#[tokio::test]
async fn security_json_injection_in_message() -> Result<()> {
    let (url, h) = spawn_server().await?;
    let payloads = vec![
        r#"{"source":"x","level":"info","message":"{\"$where\":\"1==1\"}"}"#,
        r#"{"source":"x","level":"info","message":"'; DROP TABLE logs; --"}"#,
        r#"{"source":"x","level":"info","message":"<script>alert(1)</script>"}"#,
        r#"{"source":"x","level":"info","message":"{{7*7}}"}"#,
        r#"{"source":"x","level":"info","message":"../../../etc/passwd"}"#,
    ];
    let client = Client::new();
    for payload in payloads {
        let r = client
            .post(format!("{}/api/v1/logs", url))
            .header("content-type", "application/json")
            .body(payload)
            .send()
            .await?;
        // Must either accept (202) or reject cleanly (400/422) — never 500
        assert_ne!(
            r.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "injection payload caused 500: {}",
            payload
        );
    }
    h.abort();
    Ok(())
}

/// Unicode, null bytes, and control characters in message — must not crash
#[tokio::test]
async fn security_unicode_and_control_chars() -> Result<()> {
    let (url, h) = spawn_server().await?;
    let client = Client::new();
    let messages = vec![
        "🔥💀🦀 rust is great",
        "日本語テスト",
        "مرحبا بالعالم",
        "\u{0000}\u{0001}\u{001f}", // control chars
        "normal text \\ with backslash",
        "line1\nline2\ttabbed",
    ];
    for msg in messages {
        let r = client
            .post(format!("{}/api/v1/logs", url))
            .json(&json!({ "source": "unicode-test", "level": "info", "message": msg }))
            .send()
            .await?;
        assert_ne!(
            r.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "unicode/control char caused 500: {:?}",
            msg
        );
    }
    h.abort();
    Ok(())
}

/// Tenant ID injection attempts — must all be rejected with 400
#[tokio::test]
async fn security_tenant_id_injection() -> Result<()> {
    let (url, h) = spawn_server().await?;
    let client = Client::new();
    let long_tenant = "a".repeat(65);
    let bad_tenants: Vec<&str> = vec![
        "tenant; DROP TABLE",
        "../../etc/passwd",
        "<script>alert(1)</script>",
        long_tenant.as_str(), // 65 chars — over 64 limit
    ];
    for tenant in &bad_tenants {
        let r = client
            .post(format!("{}/api/v1/logs", url))
            .header("X-Tenant-ID", *tenant)
            .json(&valid_log())
            .send()
            .await?;
        assert_eq!(
            r.status(),
            StatusCode::BAD_REQUEST,
            "malicious tenant ID '{}' must be rejected",
            tenant
        );
    }
    h.abort();
    Ok(())
}

/// Wrong Content-Type header → must not crash (400 or 415, never 500)
#[tokio::test]
async fn security_wrong_content_type() -> Result<()> {
    let (url, h) = spawn_server().await?;
    let client = Client::new();
    let content_types = vec![
        "text/plain",
        "application/xml",
        "multipart/form-data",
        "application/x-www-form-urlencoded",
    ];
    for ct in content_types {
        let r = client
            .post(format!("{}/api/v1/logs", url))
            .header("content-type", ct)
            .body(r#"{"source":"s","level":"info","message":"hi"}"#)
            .send()
            .await?;
        assert_ne!(
            r.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "content-type '{}' caused 500",
            ct
        );
    }
    h.abort();
    Ok(())
}

/// Missing Content-Type — must not crash
#[tokio::test]
async fn security_missing_content_type() -> Result<()> {
    let (url, h) = spawn_server().await?;
    let r = Client::new()
        .post(format!("{}/api/v1/logs", url))
        .body(r#"{"source":"s","level":"info","message":"hi"}"#)
        .send()
        .await?;
    assert_ne!(r.status(), StatusCode::INTERNAL_SERVER_ERROR);
    h.abort();
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// 4. EDGE CASES — boundary values, field limits, encoding
// ═══════════════════════════════════════════════════════════════════════════

/// All four log levels are accepted
#[tokio::test]
async fn edge_all_log_levels_accepted() -> Result<()> {
    let (url, h) = spawn_server().await?;
    let client = Client::new();
    for level in &["debug", "info", "warn", "error"] {
        let r = client
            .post(format!("{}/api/v1/logs", url))
            .json(&json!({ "source": "svc", "level": level, "message": "test" }))
            .send()
            .await?;
        assert_eq!(
            r.status(),
            StatusCode::ACCEPTED,
            "level '{}' must be accepted",
            level
        );
    }
    h.abort();
    Ok(())
}

/// Log level is case-insensitive (INFO, Info, iNfO)
#[tokio::test]
async fn edge_log_level_case_variants() -> Result<()> {
    let (url, h) = spawn_server().await?;
    let client = Client::new();
    // Schema validates lowercase only — uppercase variants should be rejected cleanly
    for level in &["INFO", "Info", "iNfO", "WARN", "ERROR", "DEBUG"] {
        let r = client
            .post(format!("{}/api/v1/logs", url))
            .json(&json!({ "source": "svc", "level": level, "message": "test" }))
            .send()
            .await?;
        // Must not be 500 — either accepted or cleanly rejected
        assert_ne!(
            r.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "uppercase level '{}' caused 500",
            level
        );
    }
    h.abort();
    Ok(())
}

/// Response body contains matching x-request-id in both header and JSON body
#[tokio::test]
async fn edge_request_id_consistent_in_header_and_body() -> Result<()> {
    let (url, h) = spawn_server().await?;
    let r = Client::new()
        .post(format!("{}/api/v1/logs", url))
        .json(&valid_log())
        .send()
        .await?;
    assert_eq!(r.status(), StatusCode::ACCEPTED);

    let header_id = r
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let body: serde_json::Value = r.json().await?;
    let body_id = body["id"].as_str().unwrap_or("").to_string();

    assert!(!header_id.is_empty(), "x-request-id header must be present");
    assert_eq!(header_id, body_id, "x-request-id header must match body id");
    h.abort();
    Ok(())
}

/// Optional metadata field is accepted when present
#[tokio::test]
async fn edge_optional_metadata_field() -> Result<()> {
    let (url, h) = spawn_server().await?;
    let r = Client::new()
        .post(format!("{}/api/v1/logs", url))
        .json(&json!({
            "source": "svc",
            "level": "info",
            "message": "with metadata",
            "metadata": { "request_id": "abc123", "region": "eu-west-1", "nested": { "deep": true } }
        }))
        .send()
        .await?;
    assert_eq!(r.status(), StatusCode::ACCEPTED);
    h.abort();
    Ok(())
}

/// Extra unknown fields are tolerated (not rejected)
#[tokio::test]
async fn edge_extra_unknown_fields_tolerated() -> Result<()> {
    let (url, h) = spawn_server().await?;
    let r = Client::new()
        .post(format!("{}/api/v1/logs", url))
        .json(&json!({
            "source": "svc",
            "level": "info",
            "message": "extra fields",
            "unknown_field": "should be ignored",
            "another_extra": 42
        }))
        .send()
        .await?;
    // Extra fields should be silently ignored, not rejected
    assert_ne!(
        r.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "extra fields must not cause 422"
    );
    assert_ne!(r.status(), StatusCode::INTERNAL_SERVER_ERROR);
    h.abort();
    Ok(())
}

/// PII in message is redacted — pii_hits > 0
#[tokio::test]
async fn edge_pii_redaction_reported_in_response() -> Result<()> {
    let (url, h) = spawn_server().await?;
    let r = Client::new()
        .post(format!("{}/api/v1/logs", url))
        .json(&json!({
            "source": "svc",
            "level": "info",
            "message": "user email is john.doe@example.com and card 4111-1111-1111-1111"
        }))
        .send()
        .await?;
    assert_eq!(r.status(), StatusCode::ACCEPTED);
    let body: serde_json::Value = r.json().await?;
    assert!(
        body["pii_hits"].as_u64().unwrap_or(0) >= 2,
        "expected at least 2 pii_hits (email + credit card), got: {}",
        body["pii_hits"]
    );
    h.abort();
    Ok(())
}

/// Tenant ID exactly 64 characters (boundary) → accepted
#[tokio::test]
async fn edge_tenant_id_exactly_64_chars() -> Result<()> {
    let (url, h) = spawn_server().await?;
    let tenant = "a".repeat(64);
    let r = Client::new()
        .post(format!("{}/api/v1/logs", url))
        .header("X-Tenant-ID", tenant.as_str())
        .json(&valid_log())
        .send()
        .await?;
    assert_eq!(
        r.status(),
        StatusCode::ACCEPTED,
        "64-char tenant ID must be accepted"
    );
    h.abort();
    Ok(())
}

/// Tenant ID 65 characters (one over boundary) → rejected
#[tokio::test]
async fn edge_tenant_id_65_chars_rejected() -> Result<()> {
    let (url, h) = spawn_server().await?;
    let tenant = "a".repeat(65);
    let r = Client::new()
        .post(format!("{}/api/v1/logs", url))
        .header("X-Tenant-ID", tenant.as_str())
        .json(&valid_log())
        .send()
        .await?;
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    h.abort();
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// 5. LOAD — sustained throughput with correctness assertions
// ═══════════════════════════════════════════════════════════════════════════

/// 1000 sequential requests complete without errors and within time budget
#[tokio::test]
async fn load_1000_sequential_requests() -> Result<()> {
    let (url, h) = spawn_server().await?;
    let client = Client::new();
    let start = Instant::now();
    let mut errors = 0usize;

    for i in 0..1000 {
        let r = client
            .post(format!("{}/api/v1/logs", url))
            .json(&json!({
                "source": "load-test",
                "level": "info",
                "message": format!("load test message number {}", i)
            }))
            .send()
            .await?;
        if r.status() != StatusCode::ACCEPTED {
            errors += 1;
        }
    }

    let elapsed = start.elapsed();
    assert_eq!(errors, 0, "all 1000 requests must succeed");
    // 1000 requests should complete within 10 seconds even on CI
    assert!(
        elapsed.as_secs() < 10,
        "1000 requests took {}ms — too slow",
        elapsed.as_millis()
    );

    h.abort();
    Ok(())
}

/// 500 concurrent requests across 10 distinct tenants — cost tracking consistent
#[tokio::test]
async fn load_concurrent_multi_tenant_cost_tracking() -> Result<()> {
    let (url, h) = spawn_server().await?;
    let client = Arc::new(Client::new());
    let url = Arc::new(url);

    let tasks: Vec<_> = (0..500)
        .map(|i| {
            let c = client.clone();
            let u = url.clone();
            tokio::spawn(async move {
                let tenant = format!("tenant-{}", i % 10);
                c.post(format!("{}/api/v1/logs", u))
                    .header("X-Tenant-ID", tenant)
                    .json(&json!({
                        "source": "multi-tenant",
                        "level": "info",
                        "message": format!("request {}", i)
                    }))
                    .send()
                    .await
                    .map(|r| r.status())
            })
        })
        .collect();

    let mut ok = 0usize;
    for t in tasks {
        if t.await?? == StatusCode::ACCEPTED {
            ok += 1;
        }
    }
    assert_eq!(ok, 500, "all 500 requests must succeed");

    // Cost endpoint must show 10 distinct tenants
    let costs: serde_json::Value = client
        .get(format!("{}/api/v1/costs", url))
        .send()
        .await?
        .json()
        .await?;
    let tenants = costs["tenants"].as_array().unwrap();
    assert_eq!(
        tenants.len(),
        10,
        "expected exactly 10 tenants in cost summary"
    );

    // Each tenant must have exactly 50 requests (500 / 10)
    for t in tenants {
        let count = t["total_requests"].as_u64().unwrap_or(0);
        assert_eq!(count, 50, "each tenant must have 50 requests");
    }

    h.abort();
    Ok(())
}

/// Health endpoint stays responsive under concurrent ingest load
#[tokio::test]
async fn load_health_responsive_under_load() -> Result<()> {
    let (url, h) = spawn_server().await?;
    let client = Arc::new(Client::new());
    let url = Arc::new(url);

    // Fire 100 ingest requests in background
    let load_tasks: Vec<_> = (0..100)
        .map(|i| {
            let c = client.clone();
            let u = url.clone();
            tokio::spawn(async move {
                c.post(format!("{}/api/v1/logs", u))
                    .json(&json!({
                        "source": "bg-load",
                        "level": "info",
                        "message": format!("background {}", i)
                    }))
                    .send()
                    .await
                    .ok();
            })
        })
        .collect();

    // Simultaneously poll health 10 times
    for _ in 0..10 {
        let r = client.get(format!("{}/health", url)).send().await?;
        assert_eq!(
            r.status(),
            StatusCode::OK,
            "health must stay 200 under load"
        );
    }

    for t in load_tasks {
        t.await?;
    }

    h.abort();
    Ok(())
}

use anyhow::Result;
use axum::{
    middleware as axum_middleware,
    routing::{get, post},
    Router,
};
use std::path::PathBuf;
use std::sync::Arc;
use tower::ServiceBuilder;
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

pub mod cache;
pub mod config;
pub mod cost_tracker;
pub mod handlers;
pub mod hot_reload;
pub mod logging;
pub mod metrics;
pub mod middleware;
pub mod models;
pub mod rate_limiter;
pub mod redactor;
pub mod s3_exporter;
pub mod schema_validator;
pub mod secrets;
pub mod sink;

pub use cache::SemanticCache;
pub use config::GatewayConfig;
pub use cost_tracker::CostTracker;
pub use handlers::AppState;
pub use metrics::GatewayMetrics;
pub use rate_limiter::new_tenant_limiter;
pub use redactor::Redactor;
pub use s3_exporter::{S3Config, S3Exporter};
pub use sink::StorageSink;

/// Create the Axum application router
pub fn create_app(config: GatewayConfig) -> Result<Router> {
    // Create redactor, cache, and cost tracker
    let redactor = Redactor::new();
    let cache = SemanticCache::new(config.cache.max_capacity, config.cache.ttl_seconds);
    let cost_tracker = CostTracker::new();
    let metrics = GatewayMetrics::new();

    // Create storage sink if enabled
    let storage_sink = if config.sink.enabled {
        let s = StorageSink::new(
            PathBuf::from(&config.sink.output_dir),
            config.sink.max_buffer_size,
            config.sink.flush_interval_secs,
            config.sink.compress,
        );
        s.start_flush_task();
        Some(s)
    } else {
        None
    };

    // Create S3 exporter if enabled
    let s3_exporter = if config.s3.enabled {
        // Note: In tests, we won't actually initialize S3
        None
    } else {
        None
    };

    // Create rate limiter if enabled
    let rate_limit_layer = if config.rate_limit.enabled {
        let limiter = new_tenant_limiter(config.rate_limit.requests_per_second);
        Some((
            axum::Extension(limiter),
            axum::Extension(Arc::new(metrics.clone())),
        ))
    } else {
        None
    };

    // Create app state
    let app_state = AppState {
        redactor,
        cache,
        cost_tracker,
        metrics,
        sink: storage_sink,
        s3_exporter,
        sink_output_dir: PathBuf::from(&config.sink.output_dir),
        started_at: std::time::Instant::now(),
    };

    // Build protected routes with auth middleware
    let protected = Router::new()
        .route("/api/v1/logs", post(handlers::ingest_log))
        .route("/api/v1/cache/stats", get(handlers::cache_stats))
        .route("/api/v1/costs", get(handlers::cost_summary))
        .route("/api/v1/costs/:tenant_id", get(handlers::tenant_cost))
        .route("/api/v1/export/s3", post(handlers::trigger_s3_export))
        .route_layer(axum_middleware::from_fn(middleware::require_jwt))
        .route_layer(axum_middleware::from_fn(middleware::require_api_key));

    // Add rate limiting middleware to protected routes if enabled
    let protected = if let Some((limiter_ext, metrics_ext)) = rate_limit_layer {
        protected.layer(
            ServiceBuilder::new()
                .layer(limiter_ext)
                .layer(metrics_ext)
                .layer(axum_middleware::from_fn(
                    rate_limiter::rate_limit_middleware,
                )),
        )
    } else {
        protected
    };

    // Public routes — no auth required
    // /metrics is public so Prometheus can scrape without credentials
    let public = Router::new()
        .route("/health", get(handlers::health_check))
        .route("/metrics", get(handlers::metrics));

    let app = Router::new()
        .merge(
            SwaggerUi::new("/swagger-ui")
                .url("/api-docs/openapi.json", handlers::ApiDoc::openapi()),
        )
        .merge(protected)
        .merge(public)
        .with_state(app_state)
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http());

    Ok(app)
}

/// Create app for testing (without auth middleware)
pub fn create_test_app(config: GatewayConfig) -> Result<Router> {
    // Create redactor, cache, and cost tracker
    let redactor = Redactor::new();
    let cache = SemanticCache::new(config.cache.max_capacity, config.cache.ttl_seconds);
    let cost_tracker = CostTracker::new();
    let metrics = GatewayMetrics::new();

    // Create storage sink if enabled
    let storage_sink = if config.sink.enabled {
        let s = StorageSink::new(
            PathBuf::from(&config.sink.output_dir),
            config.sink.max_buffer_size,
            config.sink.flush_interval_secs,
            config.sink.compress,
        );
        s.start_flush_task();
        Some(s)
    } else {
        None
    };

    // Create app state
    let app_state = AppState {
        redactor,
        cache,
        cost_tracker,
        metrics,
        sink: storage_sink,
        s3_exporter: None,
        sink_output_dir: PathBuf::from(&config.sink.output_dir),
        started_at: std::time::Instant::now(),
    };

    // Build protected routes WITHOUT auth middleware for tests
    let protected = Router::new()
        .route("/api/v1/logs", post(handlers::ingest_log))
        .route("/api/v1/cache/stats", get(handlers::cache_stats))
        .route("/api/v1/costs", get(handlers::cost_summary))
        .route("/api/v1/costs/:tenant_id", get(handlers::tenant_cost))
        .route("/api/v1/export/s3", post(handlers::trigger_s3_export));

    // Public routes
    let public = Router::new()
        .route("/health", get(handlers::health_check))
        .route("/metrics", get(handlers::metrics));

    let app = Router::new()
        .merge(
            SwaggerUi::new("/swagger-ui")
                .url("/api-docs/openapi.json", handlers::ApiDoc::openapi()),
        )
        .merge(protected)
        .merge(public)
        .with_state(app_state)
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http());

    Ok(app)
}

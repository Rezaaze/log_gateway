use anyhow::Result;
use axum::{
    extract::DefaultBodyLimit,
    middleware as axum_middleware,
    routing::{delete, get, post, put},
    Router,
};
use std::path::PathBuf;
use std::sync::Arc;
use tower::ServiceBuilder;
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

pub mod alert_api;
pub mod alert_manager;
pub mod anomaly_detector;
pub mod bgp_query;
pub mod cache;
pub mod clickhouse_exporter;
pub mod config;
pub mod cost_tracker;
pub mod escalation;
pub mod handlers;
pub mod hot_reload;
pub mod logging;
pub mod metrics;
pub mod middleware;
pub mod models;
pub mod rate_limiter;
pub mod redactor;
pub mod rpki_cache;
pub mod s3_exporter;
pub mod schema_validator;
pub mod secrets;
pub mod sink;

pub use anomaly_detector::AnomalyDetector;
pub use cache::SemanticCache;
pub use clickhouse_exporter::ClickHouseExporter;
pub use config::{ClickHouseConfig, GatewayConfig};
pub use cost_tracker::CostTracker;
pub use handlers::AppState;
pub use metrics::GatewayMetrics;
pub use rate_limiter::new_tenant_limiter;
pub use redactor::Redactor;
pub use s3_exporter::{S3Config, S3Exporter};
pub use sink::StorageSink;

/// Create the Axum application router
pub async fn create_app(config: GatewayConfig) -> Result<Router> {
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

    let s3_exporter = if config.s3.enabled {
        let s3_cfg = config.s3.clone();
        match S3Exporter::new(crate::s3_exporter::S3Config {
            enabled: s3_cfg.enabled,
            endpoint_url: s3_cfg.endpoint_url,
            bucket: s3_cfg.bucket,
            region: s3_cfg.region,
            prefix: s3_cfg.prefix,
            access_key_id: s3_cfg.access_key_id,
            secret_access_key: s3_cfg.secret_access_key,
            delete_after_upload: s3_cfg.delete_after_upload,
        })
        .await
        {
            Ok(exporter) => {
                tracing::info!("S3 exporter initialized");
                Some(Arc::new(exporter))
            }
            Err(e) => {
                tracing::warn!("S3 exporter disabled — init failed: {}", e);
                None
            }
        }
    } else {
        None
    };

    // Create ClickHouse exporter if enabled
    let clickhouse_exporter = if config.clickhouse.enabled {
        let exporter =
            ClickHouseExporter::new(config.clickhouse.clone(), Arc::new(metrics.clone()));
        let _flush_handle = exporter.start_flush_task();
        // Store the handle somewhere if needed for graceful shutdown
        // For now, we just let it run in the background
        Some(Arc::new(exporter))
    } else {
        None
    };

    // Create BGP query client if ClickHouse is enabled
    let bgp_query_client = if config.clickhouse.enabled {
        Some(Arc::new(bgp_query::ClickHouseQueryClient::new(
            config.clickhouse.url.clone(),
            config.clickhouse.database.clone(),
        )))
    } else {
        None
    };

    // Create alert manager client if ClickHouse is enabled
    let alert_manager_client = if config.clickhouse.enabled {
        Some(Arc::new(crate::alert_manager::AlertManagerClient::new(
            config.clickhouse.url.clone(),
            config.clickhouse.database.clone(),
        )))
    } else {
        None
    };

    // Create anomaly detector + alert channel
    let (alert_tx, alert_rx) = tokio::sync::mpsc::channel::<anomaly_detector::Anomaly>(1024);
    let detector = anomaly_detector::AnomalyDetector::new(alert_tx);

    // Warmup HijackDetector from ClickHouse history if available
    if let Some(ref qclient) = bgp_query_client {
        detector.hijack_detector().warmup(qclient).await;
    }

    let anomaly_detector = Some(Arc::new(detector));

    // Create escalation router if alert manager and anomaly detector are available
    let escalation_router =
        if let (Some(ref am), Some(_)) = (&alert_manager_client, &anomaly_detector) {
            Some(Arc::new(crate::escalation::EscalationRouter::new(
                Arc::clone(am),
                Arc::new(metrics.clone()),
            )))
        } else {
            None
        };

    // Spawn alert logger task
    let metrics_for_alerts = Arc::new(metrics.clone());
    tokio::spawn(anomaly_detector::run_alert_logger(
        alert_rx,
        metrics_for_alerts,
        escalation_router,
    ));

    // Create RPKI cache if enabled
    let rpki_cache = if config.rpki.enabled {
        Some(Arc::new(rpki_cache::RpkiCache::new(
            config.rpki.routinator_url.clone(),
        )))
    } else {
        None
    };

    // Spawn RPKI enrichment task if both RPKI and anomaly detection are enabled
    let rpki_tx = if let (Some(ref rpki), Some(ref detector_arc)) = (&rpki_cache, &anomaly_detector)
    {
        let (rpki_tx, rpki_rx) =
            tokio::sync::mpsc::channel::<crate::clickhouse_exporter::BgpClickHouseRecord>(2048);
        let hijack = detector_arc.hijack_detector_arc();
        let alert_tx_clone = detector_arc.alert_tx();
        let metrics_for_rpki = Arc::new(metrics.clone());
        tokio::spawn(anomaly_detector::run_rpki_enrichment(
            rpki_rx,
            hijack,
            Arc::clone(rpki),
            alert_tx_clone,
            metrics_for_rpki,
        ));
        Some(rpki_tx)
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

    // Read secrets once at startup — cached in AppState to avoid per-request disk I/O
    let api_key = secrets::read_secret("gateway_api_key", "GATEWAY_API_KEY").map(Arc::new);
    let jwt_secret = secrets::read_secret("gateway_jwt_secret", "GATEWAY_JWT_SECRET").map(Arc::new);

    // Create app state
    let app_state = AppState {
        redactor,
        cache,
        cost_tracker,
        metrics,
        sink: storage_sink,
        s3_exporter,
        clickhouse_exporter,
        bgp_query_client,
        anomaly_detector,
        rpki_tx,
        sink_output_dir: PathBuf::from(&config.sink.output_dir),
        started_at: std::time::Instant::now(),
        api_key,
        jwt_secret,
        alert_manager_client,
    };

    // Build protected routes with auth middleware (secrets come from AppState, no disk reads)
    let protected = Router::new()
        .route(
            "/api/v1/logs",
            post(handlers::ingest_log).layer(DefaultBodyLimit::max(65_536)),
        )
        .route(
            "/api/v1/logs/batch",
            // 1 000 entries × ~1 KB each = ~1 MB; allow up to 4 MB for headroom
            post(handlers::ingest_log_batch).layer(DefaultBodyLimit::max(4_194_304)),
        )
        .route("/api/v1/cache/stats", get(handlers::cache_stats))
        .route("/api/v1/costs", get(handlers::cost_summary))
        .route("/api/v1/costs/:tenant_id", get(handlers::tenant_cost))
        .route("/api/v1/export/s3", post(handlers::trigger_s3_export))
        .route(
            "/api/v1/bgp/prefixes/:prefix/history",
            get(bgp_query::bgp_prefix_history),
        )
        .route(
            "/api/v1/bgp/asn/:asn/prefixes",
            get(bgp_query::bgp_asn_prefixes),
        )
        .route("/api/v1/bgp/events", get(bgp_query::bgp_events))
        .route("/api/v1/bgp/stats/top-as", get(bgp_query::bgp_top_as))
        // Alert API routes
        .route("/api/v1/alerts/rules", get(alert_api::list_rules_handler))
        .route("/api/v1/alerts/rules", post(alert_api::create_rule_handler))
        .route(
            "/api/v1/alerts/rules/:id",
            put(alert_api::update_rule_handler),
        )
        .route(
            "/api/v1/alerts/rules/:id",
            delete(alert_api::delete_rule_handler),
        )
        .route(
            "/api/v1/alerts/active",
            get(alert_api::list_active_alerts_handler),
        )
        .route_layer(axum_middleware::from_fn_with_state(
            app_state.clone(),
            middleware::require_jwt,
        ))
        .route_layer(axum_middleware::from_fn_with_state(
            app_state.clone(),
            middleware::require_api_key,
        ));

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

    // Create ClickHouse exporter if enabled for tests
    let clickhouse_exporter = if config.clickhouse.enabled {
        let exporter =
            ClickHouseExporter::new(config.clickhouse.clone(), Arc::new(metrics.clone()));
        let _flush_handle = exporter.start_flush_task();
        Some(Arc::new(exporter))
    } else {
        None
    };

    // Create app state (no secrets needed — test app has no auth middleware)
    let app_state = AppState {
        redactor,
        cache,
        cost_tracker,
        metrics,
        sink: storage_sink,
        s3_exporter: None,
        clickhouse_exporter,
        bgp_query_client: None,
        anomaly_detector: None,
        rpki_tx: None,
        sink_output_dir: PathBuf::from(&config.sink.output_dir),
        started_at: std::time::Instant::now(),
        api_key: None,
        jwt_secret: None,
        alert_manager_client: None,
    };

    // Build protected routes WITHOUT auth middleware for tests
    let protected = Router::new()
        .route(
            "/api/v1/logs",
            post(handlers::ingest_log).layer(DefaultBodyLimit::max(65_536)),
        )
        .route(
            "/api/v1/logs/batch",
            post(handlers::ingest_log_batch).layer(DefaultBodyLimit::max(4_194_304)),
        )
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

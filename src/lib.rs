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
pub mod alert_dedup;
pub mod alert_manager;
pub mod anomaly_detector;
pub mod baseline_model;
pub mod bgp_query;
pub mod cache;
pub mod clickhouse_exporter;
pub mod collector_registry;
pub mod config;
pub mod cost_reporter;
pub mod cost_tracker;
pub mod detector_runner;
pub mod escalation;
pub mod handlers;
pub mod hot_reload;
pub mod irr_cache;
pub mod logging;
pub mod loki_logger;
pub mod metrics;
pub mod metrics_exporter;
pub mod middleware;
pub mod model_trainer;
pub mod models;
pub mod nats_subscriber;
pub mod propagation;
pub mod quota_manager;
pub mod rate_limiter;
pub mod redactor;
pub mod roa_poller;
pub mod rpki_cache;
pub mod s3_exporter;
pub mod schema_validator;
pub mod secrets;
pub mod sink;
pub mod telemetry;
pub mod tenant_api;
pub mod tenant_manager;
pub mod wave_anomaly_detector;
pub mod wave_baseline;
pub mod webhook;

pub use anomaly_detector::AnomalyDetector;
pub use cache::SemanticCache;
pub use clickhouse_exporter::ClickHouseExporter;
pub use config::{ClickHouseConfig, GatewayConfig};
pub use cost_tracker::CostTracker;
pub use handlers::AppState;
pub use metrics::GatewayMetrics;
pub use quota_manager::QuotaManager;
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
    metrics.record_gateway_up();

    // Create and spawn cost reporter if SMTP is enabled
    if config.smtp.enabled {
        let cost_tracker_for_reporter = Arc::new(cost_tracker.clone());
        let reporter = Arc::new(cost_reporter::CostReporter::new(
            cost_tracker_for_reporter,
            config.smtp.clone(),
        ));
        tokio::spawn(reporter.run_monthly());
        tracing::info!("Monthly cost reporter task started (SMTP enabled)");
    } else {
        tracing::info!("SMTP reporting disabled, cost reporter not started");
    }

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

    // Create tenant manager client if ClickHouse is enabled
    let tenant_manager_client = if config.clickhouse.enabled {
        Some(Arc::new(crate::tenant_manager::TenantManagerClient::new(
            config.clickhouse.url.clone(),
            config.clickhouse.database.clone(),
        )))
    } else {
        None
    };

    // Create anomaly detector + alert channel
    let (alert_tx, alert_rx) = tokio::sync::mpsc::channel::<anomaly_detector::Anomaly>(1024);
    let detector =
        anomaly_detector::AnomalyDetector::with_metrics(alert_tx, Arc::new(metrics.clone()));

    // Warmup HijackDetector from ClickHouse history if available
    if let Some(ref qclient) = bgp_query_client {
        detector.hijack_detector().warmup(qclient).await;
    }

    let anomaly_detector = Some(Arc::new(detector));

    // Build webhook targets from config
    let webhook_targets: Vec<crate::webhook::WebhookTarget> = if config.webhooks.enabled {
        config
            .webhooks
            .targets
            .iter()
            .filter_map(|t| match t.target_type.as_str() {
                "slack" => {
                    let channel = t.channel.clone().unwrap_or_else(|| "#alerts".to_string());
                    Some(crate::webhook::WebhookTarget::Slack {
                        url: t.url.clone(),
                        channel,
                    })
                }
                "generic" => {
                    let headers = t.headers.clone().unwrap_or_default();
                    Some(crate::webhook::WebhookTarget::Generic {
                        url: t.url.clone(),
                        headers,
                    })
                }
                other => {
                    tracing::warn!("Unknown webhook target_type '{}', skipping", other);
                    None
                }
            })
            .collect()
    } else {
        Vec::new()
    };

    // Create escalation router if alert manager and anomaly detector are available
    let escalation_router =
        if let (Some(ref am), Some(_)) = (&alert_manager_client, &anomaly_detector) {
            let router = if !webhook_targets.is_empty() {
                match crate::escalation::EscalationRouter::with_webhooks(
                    Arc::clone(am),
                    Arc::new(metrics.clone()),
                    webhook_targets,
                ) {
                    Ok(r) => {
                        tracing::info!("EscalationRouter initialized with webhook support");
                        r
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Webhook init failed, falling back to no-webhook router: {}",
                            e
                        );
                        crate::escalation::EscalationRouter::new(
                            Arc::clone(am),
                            Arc::new(metrics.clone()),
                        )
                    }
                }
            } else {
                crate::escalation::EscalationRouter::new(Arc::clone(am), Arc::new(metrics.clone()))
            };
            Some(Arc::new(router))
        } else {
            None
        };

    // Spawn auto-escalation task if enabled
    if config.escalation.auto_escalation_enabled {
        if let (Some(ref router), Some(ref am)) = (&escalation_router, &alert_manager_client) {
            tokio::spawn(crate::escalation::run_auto_escalation(
                Arc::clone(am),
                Arc::clone(router),
                config.escalation.check_interval_secs,
                config.escalation.timeout_secs,
            ));
            tracing::info!("Auto-escalation task started");
        }
    }

    // Spawn alert logger task
    let metrics_for_alerts = Arc::new(metrics.clone());
    let reload_rx = hot_reload::reload_signal_rx();
    tokio::spawn(anomaly_detector::run_alert_logger(
        alert_rx,
        metrics_for_alerts,
        escalation_router.clone(),
        alert_manager_client.clone(),
        reload_rx,
        None, // tenant_id from context/header (None at startup)
    ));

    // Create RPKI cache if enabled and start background VRP-dump refresh loop
    let rpki_cache = if config.rpki.enabled {
        let cache = Arc::new(
            rpki_cache::RpkiCache::new(config.rpki.routinator_url.clone()).with_refresh_interval(
                std::time::Duration::from_secs(config.rpki.refresh_interval_secs),
            ),
        );
        cache.start_refresh_loop().await;
        Some(cache)
    } else {
        None
    };

    // Create IRR cache if RPKI is enabled (IRR checking is always enabled when RPKI is enabled)
    let irr_cache = if config.rpki.enabled {
        Some(Arc::new(irr_cache::IrrCache::new()))
    } else {
        None
    };

    // Spawn RoaPoller if both RPKI and ClickHouse are enabled
    if config.rpki.enabled && config.clickhouse.enabled {
        // Use with_anomaly_sender if anomaly detector is available, otherwise use new()
        let poller = if let Some(ref detector_arc) = anomaly_detector {
            Arc::new(roa_poller::RoaPoller::with_anomaly_sender(
                config.rpki.routinator_url.clone(),
                config.clickhouse.url.clone(),
                config.clickhouse.database.clone(),
                detector_arc.alert_tx(),
            ))
        } else {
            Arc::new(roa_poller::RoaPoller::new(
                config.rpki.routinator_url.clone(),
                config.clickhouse.url.clone(),
                config.clickhouse.database.clone(),
            ))
        };
        poller.start();
        tracing::info!("RoaPoller started — polling every 5 minutes");
    }

    // Spawn daily model snapshot task (replaces ClickHouse-based training)
    // The model now learns incrementally from NATS stream, trainer only handles persistence
    if let Some(ref detector_arc) = anomaly_detector {
        let baseline_arc = detector_arc.baseline_arc();
        let trainer = Arc::new(model_trainer::ModelTrainer::new(
            baseline_arc,
            std::path::PathBuf::from(&config.snapshot.dir),
            config.snapshot.retention_days,
        ));

        // Load snapshot immediately on startup (cold start if no snapshot exists)
        let trainer_for_startup = Arc::clone(&trainer);
        tokio::spawn(async move {
            model_trainer::ModelTrainer::load_snapshot_on_startup(trainer_for_startup).await;
        });

        tokio::spawn(model_trainer::ModelTrainer::run_daily(trainer));
        tracing::info!(
            "ModelTrainer daily snapshot task spawned (snapshot dir: {}, retention: {} days)",
            config.snapshot.dir,
            config.snapshot.retention_days
        );
    }

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
            irr_cache.clone(),
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

    // Create quota manager
    let quota_manager = Arc::new(QuotaManager::new());

    // Create detector runner if NATS is enabled in config
    let detector_runner = if config.nats.enabled {
        // Create detector channel
        let (detector_tx, detector_rx) =
            tokio::sync::mpsc::channel::<crate::nats_subscriber::BgpRecord>(64000);

        // Create detector runner — with RPKI/IRR enrichment if both caches are available
        let mut detector_runner_builder = match (&rpki_cache, &irr_cache) {
            (Some(rpki), Some(irr)) => {
                tracing::info!("DetectorRunner: RPKI+IRR enrichment enabled");
                detector_runner::DetectorRunner::with_enrichment(Arc::clone(rpki), Arc::clone(irr))
            }
            _ => {
                tracing::warn!("DetectorRunner: running without RPKI/IRR enrichment (enable RPKI in config for higher-confidence detection)");
                detector_runner::DetectorRunner::new()
            }
        };

        // Share the HTTP-ingest path's HijackDetector instance instead of
        // DetectorRunner's own default (empty, never warmed up) one — see
        // with_hijack_detector's doc comment for why this matters: without
        // it, every prefix's first sighting on the live NATS path is
        // flagged as a possible hijack on every restart.
        if let Some(ref detector_arc) = anomaly_detector {
            detector_runner_builder =
                detector_runner_builder.with_hijack_detector(detector_arc.hijack_detector_arc());
        }

        if let Some(ref router) = escalation_router {
            tracing::info!(
                "DetectorRunner: escalation router attached (persist + dedup + webhook)"
            );
            detector_runner_builder = detector_runner_builder.with_escalation(Arc::clone(router));
        } else {
            tracing::warn!(
                "DetectorRunner: no escalation router available — detected anomalies from the \
                 NATS stream will only be logged and counted, not persisted or sent to webhooks"
            );
        }

        // Attach the wave-physics propagation anomaly detector. Loading a
        // baseline is best-effort: without one (e.g. not yet built via
        // tools/baseline_builder) the detector stays live but never raises
        // anomalies, so this is safe to enable unconditionally.
        if config.wave.enabled {
            let baseline_path = std::path::Path::new(&config.wave.baseline_path);
            match wave_anomaly_detector::WaveAnomalyDetector::new(Some(baseline_path)) {
                Ok(wave_detector) => {
                    tracing::info!(
                        "DetectorRunner: wave anomaly detector attached (baseline_path: {})",
                        config.wave.baseline_path
                    );
                    detector_runner_builder =
                        detector_runner_builder.with_wave_detector(Arc::new(wave_detector));
                }
                Err(e) => {
                    tracing::warn!(
                        "Wave anomaly detector disabled — failed to load baseline from {}: {}",
                        config.wave.baseline_path,
                        e
                    );
                }
            }
        }

        let detector_runner = Arc::new(detector_runner_builder);

        // Clone for task
        let detector_runner_clone = Arc::clone(&detector_runner);

        // Spawn detector task
        tokio::spawn(async move {
            detector_runner_clone.run(detector_rx).await;
        });

        // Periodically flush propagation groups whose window expired
        // without a natural completion, so they still get scored.
        if config.wave.enabled {
            let detector_runner_for_flush = Arc::clone(&detector_runner);
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
                loop {
                    interval.tick().await;
                    detector_runner_for_flush.flush_propagation_events().await;
                }
            });
        }

        // Start NATS subscriber if URL is configured
        if !config.nats.url.is_empty() {
            let nats_config = nats_subscriber::SubscriberConfig {
                nats_url: config.nats.url.clone(),
                subject: config.nats.subject.clone(),
            };

            tokio::spawn(async move {
                if let Err(e) =
                    nats_subscriber::subscribe_bgp_events(nats_config, detector_tx).await
                {
                    tracing::error!("NATS subscriber failed: {}", e);
                }
            });

            tracing::info!(
                "Detector runner started with NATS subscription to {}",
                config.nats.url
            );
        } else {
            tracing::warn!(
                "NATS enabled but no URL configured, detector runner started without NATS"
            );
        }

        Some(detector_runner)
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
        clickhouse_exporter,
        bgp_query_client,
        anomaly_detector,
        rpki_tx,
        irr_cache,
        detector_runner,
        sink_output_dir: PathBuf::from(&config.sink.output_dir),
        started_at: std::time::Instant::now(),
        api_key,
        jwt_secret,
        alert_manager_client,
        tenant_manager_client,
        quota_manager,
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
        .route(
            "/api/v1/alerts/:id/silence",
            post(alert_api::silence_alert_handler),
        )
        .route(
            "/api/v1/alerts/:id/resolve",
            post(alert_api::resolve_alert_handler),
        )
        .route(
            "/api/v1/alerts/silences",
            get(alert_api::list_silences_handler),
        )
        .route(
            "/api/v1/alerts/silences/:id",
            delete(alert_api::expire_silence_handler),
        )
        // Tenant API routes
        .route("/api/v1/tenants", get(tenant_api::list_tenants_handler))
        .route("/api/v1/tenants", post(tenant_api::create_tenant_handler))
        .route("/api/v1/tenants/:id", get(tenant_api::get_tenant_handler))
        .route(
            "/api/v1/tenants/:id",
            put(tenant_api::update_tenant_handler),
        )
        .route(
            "/api/v1/tenants/:id",
            delete(tenant_api::delete_tenant_handler),
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
    metrics.record_gateway_up();

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

    // Create quota manager for tests
    let quota_manager = Arc::new(QuotaManager::new());

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
        irr_cache: None,
        detector_runner: None,
        sink_output_dir: PathBuf::from(&config.sink.output_dir),
        started_at: std::time::Instant::now(),
        api_key: None,
        jwt_secret: None,
        alert_manager_client: None,
        tenant_manager_client: None,
        quota_manager,
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

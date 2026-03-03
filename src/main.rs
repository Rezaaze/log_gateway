use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

use anyhow::{Context, Result};
use axum_server::tls_rustls::RustlsConfig;
use log_gateway::config::GatewayConfig;
use log_gateway::loki_logger;
use log_gateway::s3_exporter::{S3Config, S3Exporter};
use log_gateway::sink::StorageSink;
use log_gateway::telemetry;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Initialize tracing with Loki support if enabled in config
fn init_tracing_with_loki(config: &GatewayConfig) -> Result<()> {
    // tokio-console: intercepts tracing events for async task inspection.
    // Activate with: RUSTFLAGS="--cfg tokio_unstable" cargo run --features tokio-console
    #[cfg(feature = "tokio-console")]
    {
        console_subscriber::init();
        tracing::info!("tokio-console subscriber active on 127.0.0.1:6669");
        // With tokio-console, we still want to add Loki layer if enabled
        if config.loki.enabled {
            if let Some((loki_layer, background_task)) = loki_logger::build_loki_layer(
                &config.loki.endpoint,
                &config.loki.service_name,
            )? {
                // Add Loki layer to existing subscriber
                tracing::subscriber::set_global_default(
                    tracing_subscriber::registry()
                        .with(loki_layer)
                )?;
                // Spawn background task
                tokio::spawn(background_task);
                info!("Loki logging enabled with endpoint: {}", config.loki.endpoint);
            } else {
                info!("Loki logging disabled (invalid endpoint or empty)");
            }
        } else {
            info!("Loki logging disabled via config");
        }
        return Ok(());
    }
    
    // Standard initialization without tokio-console
    let log_format = std::env::var("LOG_FORMAT").unwrap_or_else(|_| "text".to_string());
    
    // Create base subscriber based on LOG_FORMAT
    let fmt_layer = match log_format.to_lowercase().as_str() {
        "json" => {
            fmt::layer()
                .json()
                .with_timer(fmt::time::UtcTime::rfc_3339())
                .with_level(true)
                .with_target(true)
                .with_file(false)
                .with_line_number(false)
                .with_thread_ids(false)
                .with_thread_names(false)
                .boxed()
        }
        "text" => {
            fmt::layer()
                .with_timer(fmt::time::UtcTime::rfc_3339())
                .with_level(true)
                .with_target(true)
                .boxed()
        }
        _ => {
            tracing::warn!(
                "Invalid LOG_FORMAT value '{}', using default text format",
                log_format
            );
            fmt::layer()
                .with_timer(fmt::time::UtcTime::rfc_3339())
                .with_level(true)
                .with_target(true)
                .boxed()
        }
    };
    
    // Create env filter
    let filter_layer = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info".into());
    
    // Start with registry and filter
    let subscriber = tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt_layer);
    
    // Add Loki layer if enabled
    let loki_background_task = if config.loki.enabled {
        match loki_logger::build_loki_layer(
            &config.loki.endpoint,
            &config.loki.service_name,
        )? {
            Some((loki_layer, background_task)) => {
                info!("Loki logging enabled with endpoint: {}", config.loki.endpoint);
                // Add Loki layer to subscriber
                let subscriber = subscriber.with(loki_layer.boxed());
                // Set as global default
                tracing::subscriber::set_global_default(subscriber)
                    .context("Failed to set global tracing subscriber")?;
                Some(background_task)
            }
            None => {
                info!("Loki logging disabled (invalid endpoint or empty)");
                // Set subscriber without Loki layer
                tracing::subscriber::set_global_default(subscriber)
                    .context("Failed to set global tracing subscriber")?;
                None
            }
        }
    } else {
        info!("Loki logging disabled via config");
        // Set subscriber without Loki layer
        tracing::subscriber::set_global_default(subscriber)
            .context("Failed to set global tracing subscriber")?;
        None
    };
    
    // Spawn Loki background task if we have one
    if let Some(background_task) = loki_background_task {
        tokio::spawn(background_task);
    }
    
    tracing::info!("Logging initialized with {} format", log_format);
    Ok(())
}

// Worker thread count: Tokio reads TOKIO_WORKER_THREADS env var at startup.
// For 16+ core servers set: TOKIO_WORKER_THREADS=16 (or 2× physical cores).
// Default: number of logical CPUs (auto-detected by Tokio).
#[tokio::main]
async fn main() -> Result<()> {
    // Load configuration first (needed for Loki initialization)
    let config = GatewayConfig::load()?;

    // Initialize tracing with Loki support if enabled
    init_tracing_with_loki(&config)?;

    // Log effective worker thread count for observability on production servers
    let worker_threads = std::env::var("TOKIO_WORKER_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
        });

    info!(
        "Tokio runtime: {} worker thread(s) (set TOKIO_WORKER_THREADS to override)",
        worker_threads
    );
    let _config_rx = log_gateway::hot_reload::start_config_watcher();
    #[cfg(unix)]
    info!(
        "Config hot-reload enabled — send SIGHUP to PID {} to reload",
        std::process::id()
    );
    let addr = format!("{}:{}", config.server.host, config.server.port);

    // Initialize OpenTelemetry tracer if enabled
    if config.telemetry.enabled {
        telemetry::init_tracer(&config.telemetry.service_name, Some(&config.telemetry.otlp_endpoint))
            .context("Failed to initialize OpenTelemetry tracer")?;
        info!("OpenTelemetry tracing enabled with endpoint: {}", config.telemetry.otlp_endpoint);
    } else {
        info!("OpenTelemetry tracing disabled");
    }

    // Conditional logging based on config
    if config.cost.enabled {
        info!("Cost tracking enabled");
    }
    if config.metrics.enabled {
        info!("Prometheus metrics enabled at /metrics");
    }

    // Log rate limiting status
    if config.rate_limit.enabled {
        info!(
            "Rate limiting enabled: {} req/s",
            config.rate_limit.requests_per_second
        );
    } else {
        info!("Rate limiting disabled");
    }

    // Log API key authentication status
    match log_gateway::secrets::read_secret("gateway_api_key", "GATEWAY_API_KEY") {
        Some(k) => info!("API key authentication enabled ({} chars)", k.len()),
        None => info!("API key authentication disabled (set GATEWAY_API_KEY or /run/secrets/gateway_api_key to enable)"),
    }

    // Log JWT authentication status
    match log_gateway::secrets::read_secret("gateway_jwt_secret", "GATEWAY_JWT_SECRET") {
        Some(k) => info!("JWT authentication enabled ({} chars)", k.len()),
        None => info!("JWT authentication disabled (set GATEWAY_JWT_SECRET or /run/secrets/gateway_jwt_secret to enable)"),
    }

    // Create storage sink if enabled (needed for S3 export task)
    let storage_sink = if config.sink.enabled {
        let s = StorageSink::new(
            PathBuf::from(&config.sink.output_dir),
            config.sink.max_buffer_size,
            config.sink.flush_interval_secs,
            config.sink.compress,
        );
        s.start_flush_task();
        info!("Storage sink enabled → {}", config.sink.output_dir);
        Some(s)
    } else {
        None
    };

    // Clone sink for shutdown handler before moving into app_state
    let shutdown_sink = storage_sink.clone();

    // Create S3 exporter if enabled
    let s3_exporter = if config.s3.enabled {
        let s3_cfg = config.s3.clone();
        match S3Exporter::new(S3Config {
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
                info!("S3 export enabled → bucket: {}", exporter.bucket_name());
                Some(Arc::new(exporter))
            }
            Err(e) => {
                tracing::error!("Failed to initialize S3 exporter: {}", e);
                None
            }
        }
    } else {
        None
    };

    // Start S3 background export task if enabled
    if config.s3.enabled {
        if let Some(exporter) = s3_exporter.clone() {
            let output_dir = PathBuf::from(&config.sink.output_dir);
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
                loop {
                    interval.tick().await;
                    match exporter.export_pending_files(&output_dir).await {
                        Ok(n) if n > 0 => info!("S3: exported {} files", n),
                        Ok(_) => {} // nichts zu exportieren
                        Err(e) => tracing::error!("S3 export error: {}", e),
                    }
                }
            });
        }
    }

    // Create the app using the library function
    let app = log_gateway::create_app(config.clone()).await?;

    if config.tls.enabled {
        info!("TLS enabled — listening on https://{}", addr);
        let rustls_config =
            RustlsConfig::from_pem_file(&config.tls.cert_path, &config.tls.key_path)
                .await
                .expect("Failed to load TLS certificate/key");

        let listener_addr: std::net::SocketAddr = addr.parse().expect("Invalid address");

        axum_server::bind_rustls(listener_addr, rustls_config)
            .serve(app.into_make_service())
            .await?;
    } else {
        info!("TLS disabled — listening on http://{}", addr);
        let listener = tokio::net::TcpListener::bind(&addr).await?;
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                // Wait for Ctrl+C or SIGTERM
                let ctrl_c = async {
                    tokio::signal::ctrl_c()
                        .await
                        .expect("failed to install Ctrl+C handler");
                };

                #[cfg(unix)]
                let sigterm = async {
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .expect("failed to install SIGTERM handler")
                        .recv()
                        .await;
                };

                #[cfg(not(unix))]
                let sigterm = std::future::pending::<()>();

                tokio::select! {
                    _ = ctrl_c  => info!("Received Ctrl+C — shutting down"),
                    _ = sigterm => info!("Received SIGTERM — shutting down"),
                }

                // Flush sink before exit
                if let Some(sink) = &shutdown_sink {
                    sink.flush_on_shutdown().await;
                }

                // Shutdown OpenTelemetry tracer
                telemetry::shutdown_tracer();
            })
            .await?;
    }

    Ok(())
}

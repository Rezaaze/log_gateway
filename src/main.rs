use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

use anyhow::Result;
use axum_server::tls_rustls::RustlsConfig;
use log_gateway::config::GatewayConfig;
use log_gateway::logging;
use log_gateway::s3_exporter::{S3Config, S3Exporter};
use log_gateway::sink::StorageSink;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;

// Worker thread count: Tokio reads TOKIO_WORKER_THREADS env var at startup.
// For 16+ core servers set: TOKIO_WORKER_THREADS=16 (or 2× physical cores).
// Default: number of logical CPUs (auto-detected by Tokio).
#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing subscriber based on LOG_FORMAT environment variable
    logging::init_tracing();

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

    // Load configuration
    let config = GatewayConfig::load()?;
    let _config_rx = log_gateway::hot_reload::start_config_watcher();
    #[cfg(unix)]
    info!(
        "Config hot-reload enabled — send SIGHUP to PID {} to reload",
        std::process::id()
    );
    let addr = format!("{}:{}", config.server.host, config.server.port);

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
    let app = log_gateway::create_app(config.clone())?;

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
            })
            .await?;
    }

    Ok(())
}

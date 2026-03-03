//! OpenTelemetry tracing setup for distributed tracing with Jaeger.
//!
//! This module provides initialization and shutdown of OpenTelemetry tracing
//! with OTLP exporter (Jaeger-compatible). When no OTLP endpoint is configured,
//! it gracefully degrades to a NoopTracer.

use anyhow::{Context, Result};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::trace::TracerProvider;
use opentelemetry_sdk::Resource;
use std::sync::{Arc, Mutex, OnceLock};
use tracing::subscriber::set_global_default;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::Registry;

/// Global tracer provider that can be shut down on application exit.
/// Uses OnceLock<Mutex<...>> for safe, lock-free initialization and mutable access.
static TRACER_PROVIDER: OnceLock<Mutex<Option<Arc<TracerProvider>>>> = OnceLock::new();

fn tracer_provider_slot() -> &'static Mutex<Option<Arc<TracerProvider>>> {
    TRACER_PROVIDER.get_or_init(|| Mutex::new(None))
}

/// Initializes OpenTelemetry Tracer with OTLP-Exporter (Jaeger-compatible).
///
/// Returns `Tracer` on success. Graceful degradation: when no OTLP endpoint
/// is configured → NoopTracer.
///
/// # Arguments
///
/// * `service_name` - Name of the service for tracing (e.g., "log-gateway")
/// * `otlp_endpoint` - Optional OTLP endpoint URL (e.g., "http://localhost:4317")
///
/// # Returns
///
/// * `Ok(())` if initialization succeeded or was skipped (disabled)
/// * `Err` if OTLP endpoint is provided but connection fails
pub fn init_tracer(service_name: &str, otlp_endpoint: Option<&str>) -> Result<()> {
    // If no endpoint is provided, use a no-op tracer (graceful degradation)
    let endpoint = match otlp_endpoint {
        Some(endpoint) if !endpoint.trim().is_empty() => endpoint,
        _ => {
            tracing::info!("OpenTelemetry disabled: no OTLP endpoint configured");
            return Ok(());
        }
    };

    tracing::info!(
        "Initializing OpenTelemetry tracer for service '{}' with endpoint '{}'",
        service_name,
        endpoint
    );

    // Create OTLP exporter
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .build()
        .context("Failed to build OTLP exporter")?;

    // Create tracer provider with batch span processor
    let provider = TracerProvider::builder()
        .with_batch_exporter(exporter, opentelemetry_sdk::runtime::Tokio)
        .with_resource(Resource::new(vec![
            opentelemetry::KeyValue::new(
                opentelemetry_semantic_conventions::resource::SERVICE_NAME,
                service_name.to_string(),
            ),
            opentelemetry::KeyValue::new(
                opentelemetry_semantic_conventions::resource::SERVICE_VERSION,
                env!("CARGO_PKG_VERSION").to_string(),
            ),
        ]))
        .build();

    // Get tracer from provider
    let tracer = provider.tracer("log-gateway");

    // Create OpenTelemetry layer for tracing subscriber
    let telemetry_layer = tracing_opentelemetry::layer().with_tracer(tracer);

    // Configure tracing subscriber with OpenTelemetry layer
    let subscriber = Registry::default().with(telemetry_layer);

    // Set as global default
    set_global_default(subscriber).context("Failed to set global tracing subscriber")?;

    // Store provider for shutdown
    *tracer_provider_slot().lock().unwrap() = Some(Arc::new(provider));

    tracing::info!("OpenTelemetry tracer initialized successfully");
    Ok(())
}

/// Shuts down the tracer gracefully (flush pending spans).
///
/// This should be called during application shutdown to ensure all spans
/// are exported before the process exits.
pub fn shutdown_tracer() {
    tracing::info!("Shutting down OpenTelemetry tracer...");

    let provider = tracer_provider_slot().lock().unwrap().take();
    if let Some(provider) = provider {
        // Force flush of pending spans
        provider.force_flush();

        // Shutdown the provider
        if let Err(e) = provider.shutdown() {
            tracing::error!("Failed to shutdown tracer provider: {}", e);
        } else {
            tracing::info!("OpenTelemetry tracer shut down successfully");
        }
    } else {
        tracing::debug!("No OpenTelemetry tracer to shut down");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_init_tracer_noop_when_disabled() {
        // Should succeed with no endpoint (graceful degradation)
        let result = init_tracer("test-service", None);
        assert!(result.is_ok(), "Should succeed with no endpoint");

        // Should succeed with empty endpoint
        let result = init_tracer("test-service", Some(""));
        assert!(result.is_ok(), "Should succeed with empty endpoint");

        // Should succeed with whitespace-only endpoint
        let result = init_tracer("test-service", Some("   "));
        assert!(result.is_ok(), "Should succeed with whitespace-only endpoint");
    }

    #[test]
    fn test_shutdown_tracer_without_init() {
        // Should not panic when called without initialization
        shutdown_tracer();
    }
}
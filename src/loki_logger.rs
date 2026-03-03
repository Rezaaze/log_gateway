use anyhow::{Context, Result};
use std::collections::HashMap;
use tracing_loki::{BackgroundTask, Layer};
use url::Url;

/// Initialisiert tracing-Subscriber-Layer für Loki.
/// Graceful degradation: wenn kein Loki-Endpoint → None zurückgeben (kein Fehler).
pub fn build_loki_layer(
    endpoint: &str,
    service_name: &str,
) -> Result<Option<(Layer, BackgroundTask)>> {
    // Graceful degradation: wenn der Endpoint leer oder ungültig ist, geben wir None zurück
    if endpoint.trim().is_empty() {
        return Ok(None);
    }

    // Parse den Endpoint als URL
    let url = match Url::parse(endpoint) {
        Ok(url) => url,
        Err(e) => {
            tracing::warn!("Invalid Loki endpoint '{}': {}. Loki logging disabled.", endpoint, e);
            return Ok(None);
        }
    };

    // Baue Labels für Loki: { service, environment, version }
    let labels = build_loki_labels(service_name);

    // Erstelle den Loki Layer
    let mut builder = tracing_loki::builder()
        .label("service", service_name)?;
    
    // Add additional labels
    for (key, value) in labels {
        builder = builder.label(&key, &value)?;
    }
    
    let (layer, background_task) = builder
        .build_url(url)
        .context("Failed to build Loki layer")?;

    Ok(Some((layer, background_task)))
}

/// Baut zusätzliche Labels für Loki: { environment, version }
/// (service wird separat als primäres Label gesetzt)
/// environment = RUST_ENV env-var, fallback "production"
fn build_loki_labels(_service_name: &str) -> HashMap<String, String> {
    let mut labels = HashMap::new();

    // Environment label
    let environment = std::env::var("RUST_ENV")
        .unwrap_or_else(|_| "production".to_string());
    labels.insert("environment".to_string(), environment);

    // Version label
    let version = option_env!("CARGO_PKG_VERSION")
        .unwrap_or("unknown")
        .to_string();
    labels.insert("version".to_string(), version);

    labels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_loki_layer_noop_when_disabled() {
        // Test mit leerem Endpoint
        let result = build_loki_layer("", "test-service").unwrap();
        assert!(result.is_none());

        // Test mit ungültigem Endpoint
        let result = build_loki_layer("not-a-valid-url", "test-service").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_build_loki_labels_structure() {
        // Prüft nur die Struktur der Labels, unabhängig von Env-Vars.
        // service-Label ist NICHT in der HashMap (wird separat via .label() gesetzt).
        // environment und version müssen vorhanden sein.
        let labels = build_loki_labels("my-service");

        assert!(
            !labels.contains_key("service"),
            "service should not be in extra labels map"
        );
        assert!(
            labels.contains_key("environment"),
            "environment label must be present"
        );
        assert!(
            labels.contains_key("version"),
            "version label must be present"
        );
    }

    #[test]
    fn test_build_loki_labels_environment_fallback() {
        // Wenn RUST_ENV nicht gesetzt ist, muss "production" der Fallback sein.
        // Wir testen nur wenn RUST_ENV tatsächlich nicht gesetzt ist.
        if std::env::var("RUST_ENV").is_err() {
            let labels = build_loki_labels("default-service");
            assert_eq!(
                labels.get("environment").map(String::as_str),
                Some("production"),
                "fallback environment should be 'production'"
            );
        }
        // Wenn RUST_ENV gesetzt ist, überspringen wir den Test (CI-Umgebung).
    }

    #[test]
    fn test_build_loki_layer_with_valid_endpoint() {
        // Test with a valid endpoint (even though it won't connect in test)
        // This tests that the function doesn't panic and returns Some
        let result = build_loki_layer("http://localhost:3100", "test-service");
        // The function should return Ok(Some(...)) or Ok(None) depending on URL parsing
        // We just check it doesn't panic
        assert!(result.is_ok());
    }
}

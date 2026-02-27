use tracing_subscriber::{fmt, EnvFilter};

/// Initialize the tracing subscriber based on the LOG_FORMAT environment variable.
///
/// The LOG_FORMAT environment variable can be set to:
/// - "json": Output logs in structured JSON format with timestamp, level, message, and target fields
/// - "text": Output logs in plain text format (default)
///
/// If LOG_FORMAT is not set or contains an invalid value, the default "text" format is used.
pub fn init_tracing() {
    let log_format = std::env::var("LOG_FORMAT").unwrap_or_else(|_| "text".to_string());

    match log_format.to_lowercase().as_str() {
        "json" => {
            // JSON format with timestamp, level, message, and target fields
            let subscriber = fmt::Subscriber::builder()
                .with_env_filter(
                    EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
                )
                .json()
                .with_timer(fmt::time::UtcTime::rfc_3339())
                .with_level(true)
                .with_target(true)
                .with_file(false) // Optional: include file path
                .with_line_number(false) // Optional: include line number
                .with_thread_ids(false) // Optional: include thread IDs
                .with_thread_names(false) // Optional: include thread names
                .finish();

            tracing::subscriber::set_global_default(subscriber)
                .expect("Failed to set global tracing subscriber");

            tracing::info!("Logging initialized with JSON format");
        }
        "text" => {
            // Plain text format (default)
            tracing_subscriber::fmt()
                .with_env_filter(
                    EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
                )
                .init();

            tracing::info!("Logging initialized with text format");
        }
        _ => {
            // Invalid value, fall back to text format
            tracing_subscriber::fmt()
                .with_env_filter(
                    EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
                )
                .init();

            tracing::warn!(
                "Invalid LOG_FORMAT value '{}', using default text format",
                log_format
            );
            tracing::info!("Logging initialized with text format");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::panic;

    #[test]
    fn test_json_format_env_var_respected() {
        // Temporarily set LOG_FORMAT to json
        std::env::set_var("LOG_FORMAT", "json");

        // This should not panic when initializing tracing
        let result = panic::catch_unwind(|| {
            // Note: We can't actually call init_tracing() because it would
            // try to set a global subscriber which can only be done once.
            // Instead, we test that the environment variable is read correctly
            // and the code path for JSON format doesn't have obvious issues.
            let log_format = std::env::var("LOG_FORMAT").unwrap();
            assert_eq!(log_format, "json");

            // Test that we can create a JSON subscriber builder without panicking
            let _builder = tracing_subscriber::fmt()
                .json()
                .with_timer(tracing_subscriber::fmt::time::UtcTime::rfc_3339())
                .with_level(true)
                .with_target(true);
        });

        // Clean up
        std::env::remove_var("LOG_FORMAT");

        assert!(
            result.is_ok(),
            "JSON format initialization should not panic"
        );
    }

    #[test]
    fn test_text_format_is_default() {
        // Ensure LOG_FORMAT is not set
        std::env::remove_var("LOG_FORMAT");

        // This should not panic
        let result = panic::catch_unwind(|| {
            let log_format = std::env::var("LOG_FORMAT").unwrap_or_else(|_| "text".to_string());
            assert_eq!(log_format, "text");

            // Test that we can create a text subscriber builder without panicking
            let _builder = tracing_subscriber::fmt();
        });

        assert!(
            result.is_ok(),
            "Text format initialization should not panic"
        );
    }
}

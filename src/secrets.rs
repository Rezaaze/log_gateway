use std::fs;
use std::path::Path;

/// Reads a secret: first from `/run/secrets/<secret_name>`, then from ENV var `<env_name>`.
///
/// Docker Swarm mounts secrets as files under `/run/secrets/`. This function checks
/// for the file first, falling back to environment variables for local development
/// or compatibility.
///
/// # Arguments
///
/// * `secret_name` - Name of the secret file (without path), e.g., "gateway_api_key"
/// * `env_name` - Name of the environment variable, e.g., "GATEWAY_API_KEY"
///
/// # Returns
///
/// * `Some(String)` - The secret value (trimmed) if found
/// * `None` - If neither the secret file nor environment variable exists, or if the value is empty
pub fn read_secret(secret_name: &str, env_name: &str) -> Option<String> {
    let secret_path = format!("/run/secrets/{}", secret_name);
    read_secret_with_path(Path::new(&secret_path), env_name)
}

/// Inner implementation with an injectable path — used by `read_secret` and tests.
fn read_secret_with_path(path: &Path, env_name: &str) -> Option<String> {
    if path.exists() {
        match fs::read_to_string(path) {
            Ok(content) => {
                let trimmed = content.trim();
                if trimmed.is_empty() {
                    // Fall back to environment variable if secret file is empty
                    std::env::var(env_name).ok().filter(|s| !s.is_empty())
                } else {
                    Some(trimmed.to_string())
                }
            }
            Err(_) => {
                // If we can't read the file, fall back to environment variable
                std::env::var(env_name).ok().filter(|s| !s.is_empty())
            }
        }
    } else {
        // No secret file, try environment variable
        std::env::var(env_name).ok().filter(|s| !s.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use tempfile::NamedTempFile;

    #[test]
    fn test_read_secret_from_env() {
        // Set up environment variable
        env::set_var("TEST_SECRET_ENV", "env-secret-value");

        // Test reading from environment variable (no secret file)
        let result = read_secret("test_secret", "TEST_SECRET_ENV");
        assert_eq!(result, Some("env-secret-value".to_string()));

        // Clean up
        env::remove_var("TEST_SECRET_ENV");
    }

    #[test]
    fn test_read_secret_returns_none_when_empty() {
        // Set up empty environment variable
        env::set_var("TEST_EMPTY_ENV", "");

        // Test that empty environment variable returns None
        let result = read_secret("test_empty", "TEST_EMPTY_ENV");
        assert_eq!(result, None);

        // Clean up
        env::remove_var("TEST_EMPTY_ENV");
    }

    #[test]
    fn test_read_secret_from_file() {
        // Write secret content with trailing newline to verify trimming
        let temp_file = NamedTempFile::new().unwrap();
        std::fs::write(temp_file.path(), "file-secret-value\n").unwrap();

        let result = read_secret_with_path(temp_file.path(), "TEST_FILE_SECRET_ENV_UNUSED");
        assert_eq!(
            result,
            Some("file-secret-value".to_string()),
            "Expected trimmed file content"
        );
    }

    #[test]
    fn test_read_secret_file_overrides_env() {
        // When both a secret file and an env var exist, the file must take precedence
        env::set_var("TEST_FILE_OVERRIDE_ENV", "env-value");

        let temp_file = NamedTempFile::new().unwrap();
        std::fs::write(temp_file.path(), "file-value").unwrap();

        let result = read_secret_with_path(temp_file.path(), "TEST_FILE_OVERRIDE_ENV");
        assert_eq!(
            result,
            Some("file-value".to_string()),
            "File secret must override env var"
        );

        env::remove_var("TEST_FILE_OVERRIDE_ENV");
    }

    #[test]
    fn test_read_secret_none_when_neither_exists() {
        // Ensure no environment variable is set
        env::remove_var("TEST_NONEXISTENT_ENV");

        // Test that None is returned when neither file nor env exists
        let result = read_secret("nonexistent_secret", "TEST_NONEXISTENT_ENV");
        assert_eq!(result, None);
    }
}

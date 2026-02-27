use anyhow::Result;
use aws_credential_types::Credentials;
use aws_sdk_s3::primitives::ByteStream;
use chrono::Utc;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct S3Config {
    #[allow(dead_code)]
    pub enabled: bool,
    pub endpoint_url: Option<String>,
    pub bucket: String,
    pub region: String,
    pub prefix: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub delete_after_upload: bool,
}

impl Default for S3Config {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint_url: None,
            bucket: String::new(),
            region: "us-east-1".to_string(),
            prefix: "logs/".to_string(),
            access_key_id: String::new(),
            secret_access_key: String::new(),
            delete_after_upload: false,
        }
    }
}

#[derive(Debug)]
pub struct S3Exporter {
    client: aws_sdk_s3::Client,
    config: S3Config,
}

impl S3Exporter {
    pub async fn new(config: S3Config) -> Result<Self> {
        let creds = Credentials::new(
            &config.access_key_id,
            &config.secret_access_key,
            None,
            None,
            "log-gateway",
        );

        let mut builder = aws_sdk_s3::config::Builder::new()
            .credentials_provider(creds)
            .region(aws_sdk_s3::config::Region::new(config.region.clone()))
            .force_path_style(true)
            .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest());

        if let Some(ref endpoint) = config.endpoint_url {
            builder = builder.endpoint_url(endpoint);
        }

        let s3_config = builder.build();
        let client = aws_sdk_s3::Client::from_conf(s3_config);

        Ok(Self { client, config })
    }

    pub fn bucket_name(&self) -> &str {
        &self.config.bucket
    }

    pub async fn upload_file(&self, file_path: &Path) -> Result<()> {
        let filename = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| anyhow::anyhow!("invalid filename"))?;

        let date_prefix = Utc::now().format("%Y-%m-%d").to_string();
        let key = format!("{}{}/{}", self.config.prefix, date_prefix, filename);

        let body = ByteStream::from_path(file_path).await?;

        self.client
            .put_object()
            .bucket(&self.config.bucket)
            .key(&key)
            .content_type("application/x-ndjson")
            .body(body)
            .send()
            .await?;

        tracing::info!(
            "Uploaded {} → s3://{}/{}",
            filename,
            self.config.bucket,
            key
        );

        if self.config.delete_after_upload {
            tokio::fs::remove_file(file_path).await?;
            tracing::info!("Deleted local file: {}", file_path.display());
        }

        Ok(())
    }

    pub async fn export_pending_files(&self, output_dir: &Path) -> Result<usize> {
        let mut uploaded = 0;
        let mut dir = tokio::fs::read_dir(output_dir).await?;

        while let Some(entry) = dir.next_entry().await? {
            let path = entry.path();

            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !name.ends_with(".ndjson") && !name.ends_with(".ndjson.zst") {
                continue;
            }

            let metadata = tokio::fs::metadata(&path).await?;
            if metadata.len() == 0 {
                continue;
            }

            match self.upload_file(&path).await {
                Ok(_) => uploaded += 1,
                Err(e) => tracing::error!("Failed to upload {}: {}", path.display(), e),
            }
        }

        Ok(uploaded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_s3_key_format() {
        let date = "2026-02-26";
        let prefix = "logs/";
        let filename = "1740000000000.ndjson";
        let key = format!("{}{}/{}", prefix, date, filename);
        assert_eq!(key, "logs/2026-02-26/1740000000000.ndjson");
    }

    #[test]
    fn test_s3_config_default() {
        let cfg = S3Config::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.region, "us-east-1");
        assert_eq!(cfg.prefix, "logs/");
        assert!(!cfg.delete_after_upload);
    }

    #[tokio::test]
    async fn test_export_skips_empty_files() {
        let dir = tempdir().unwrap();
        let empty_file = dir.path().join("empty.ndjson");
        tokio::fs::write(&empty_file, b"").await.unwrap();

        let config = S3Config {
            enabled: true,
            endpoint_url: Some("http://localhost:9999".to_string()),
            bucket: "test-bucket".to_string(),
            region: "us-east-1".to_string(),
            prefix: "logs/".to_string(),
            access_key_id: "test".to_string(),
            secret_access_key: "test".to_string(),
            delete_after_upload: false,
        };

        let exporter = S3Exporter::new(config).await.unwrap();
        let result = exporter.export_pending_files(dir.path()).await.unwrap();
        assert_eq!(result, 0);
    }
}

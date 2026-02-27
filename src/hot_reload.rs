use crate::config::GatewayConfig;
use std::sync::Arc;
use tokio::sync::watch;
use tracing::{error, info};

pub fn start_config_watcher() -> watch::Receiver<Arc<GatewayConfig>> {
    let config = GatewayConfig::load().expect("initial config load failed");
    let (tx, rx) = watch::channel(Arc::new(config));

    #[cfg(unix)]
    tokio::spawn(async move {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sighup = match signal(SignalKind::hangup()) {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to install SIGHUP handler: {}", e);
                return;
            }
        };
        loop {
            sighup.recv().await;
            info!("SIGHUP received — reloading config...");
            match GatewayConfig::load() {
                Ok(new_cfg) => {
                    info!("Config reloaded successfully");
                    let _ = tx.send(Arc::new(new_cfg));
                }
                Err(e) => {
                    error!("Config reload failed: {} — keeping current config", e);
                }
            }
        }
    });

    #[cfg(not(unix))]
    {
        drop(tx);
    }

    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_config_watcher_returns_valid_config() {
        let rx = start_config_watcher();
        let cfg = rx.borrow();
        assert_eq!(cfg.server.port, 8080);
    }
}

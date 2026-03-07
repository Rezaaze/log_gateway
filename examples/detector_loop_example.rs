//! Example usage of the DetectorLoop
//!
//! This example shows how to set up and run the detector loop that connects
//! all isolated components:
//! NATS Consumer → RPKI → IRR → HijackDetector → FlappingDetector → Dedup → Webhook

use std::sync::Arc;

use log_gateway::detector_loop::{DetectorLoop, DetectorLoopConfig};
use log_gateway::rpki_cache::RpkiCache;
use log_gateway::webhook::WebhookTarget;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    // Create RPKI cache (in production, use a real RPKI validator URL)
    let rpki_cache = Arc::new(RpkiCache::new("http://localhost:8080".to_string()));

    // Configure webhook targets
    let webhook_targets = vec![
        // Example Slack webhook
        WebhookTarget::Slack {
            url: "https://hooks.slack.com/services/XXX/YYY/ZZZ".to_string(),
            channel: "#bgp-alerts".to_string(),
        },
        // Example generic webhook
        WebhookTarget::Generic {
            url: "https://webhook.example.com/alerts".to_string(),
            headers: vec![
                ("X-API-Key".to_string(), "secret-key".to_string()),
                ("Content-Type".to_string(), "application/json".to_string()),
            ]
            .into_iter()
            .collect(),
        },
    ];

    // Configure the detector loop
    let config = DetectorLoopConfig {
        nats_url: "nats://localhost:4222".to_string(),
        subject: "bgp.events".to_string(),
        webhook_targets,
        irr_enabled: true,    // Enable IRR checking (slower but more comprehensive)
        channel_size: 10_000, // Buffer size for NATS events
    };

    // Create the detector loop
    let detector_loop = DetectorLoop::new(config, rpki_cache)?;

    tracing::info!("Starting detector loop...");
    tracing::info!("Listening for BGP events on NATS subject: bgp.events");
    tracing::info!("RPKI validation enabled");
    tracing::info!("IRR checking enabled");
    tracing::info!("Webhook notifications configured");

    // Run the detector loop (blocks until error or shutdown)
    detector_loop.run().await?;

    Ok(())
}

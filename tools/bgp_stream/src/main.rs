/*!
BGP Stream (RIPE NCC RIS Live) → NATS Jetstream
=================================================
Rust rewrite of tools/bgp_stream.py v3.

Architecture
────────────
  WebSocket task  ──async-channel──►  NATS Publisher task  ──► nats://bgp-events
                                       (batch-1000, NATS publisher)

Env vars (all optional)
───────────────────────
  NATS_URL         default: nats://localhost:4222
  WORKERS          NATS publisher tasks  (default: 1, single thread OK)
  BATCH_SIZE       entries per publish   (default: 1000)
  BATCH_TIMEOUT_MS max wait for full batch (default: 10)
  TENANT_ID        log tenant           (default: bgp, embedded in metadata)
  SAMPLE_RATE      0.0-1.0 keep fraction (default: 1.0)
  CHANNEL_CAP      bounded channel size  (default: 64_000)
  RUST_LOG         logging level        (default: warn)
*/

use async_channel::{bounded, Receiver, Sender, TrySendError};
use chrono::{DateTime, Utc};
use futures_util::{SinkExt, StreamExt};
use rustls::RootCertStore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    env,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::time::sleep;
use tokio_tungstenite::{
    connect_async_tls_with_config,
    tungstenite::{client::IntoClientRequest, Message},
    Connector,
};
use tracing::{error, info, warn};
use uuid::Uuid;

// ── Local model types (mirrors log-gateway API contract) ──────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "lowercase")]
enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct BgpEvent {
    #[serde(default = "Uuid::new_v4")]
    id: Uuid,
    #[serde(default = "Utc::now")]
    timestamp: DateTime<Utc>,
    level: LogLevel,
    source: String,
    message: String,
    metadata: Option<serde_json::Value>,
}

// ── Configuration ─────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct Config {
    nats_url: String,
    workers: usize,
    batch_size: usize,
    batch_timeout: Duration,
    #[allow(dead_code)] // reserved for future NATS metadata tagging
    tenant_id: String,
    sample_rate: f64,
    channel_cap: usize,
}

impl Config {
    fn from_env() -> Self {
        Self {
            nats_url: env::var("NATS_URL").unwrap_or_else(|_| "nats://localhost:4222".into()),
            workers: env::var("WORKERS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1),
            batch_size: env::var("BATCH_SIZE")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1000),
            batch_timeout: Duration::from_millis(
                env::var("BATCH_TIMEOUT_MS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(10),
            ),
            tenant_id: env::var("TENANT_ID").unwrap_or_else(|_| "bgp".into()),
            sample_rate: env::var("SAMPLE_RATE")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1.0_f64),
            channel_cap: env::var("CHANNEL_CAP")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(64_000),
        }
    }
}

// ── Shared stats ──────────────────────────────────────────────────────────────

#[derive(Default)]
struct Stats {
    published: AtomicU64,
    errors: AtomicU64,
    dropped: AtomicU64,
    ann: AtomicU64,
    with: AtomicU64,
}

// ── Known AS names ─────────────────────────────────────────────────────────────

fn known_as_map() -> HashMap<u64, &'static str> {
    let mut m = HashMap::new();
    m.insert(15169, "Google");
    m.insert(8075, "Microsoft");
    m.insert(16509, "Amazon AWS");
    m.insert(13335, "Cloudflare");
    m.insert(32934, "Meta");
    m.insert(714, "Apple");
    m.insert(2906, "Netflix");
    m.insert(20940, "Akamai");
    m.insert(6939, "Hurricane Electric");
    m.insert(1299, "Telia");
    m.insert(3356, "Lumen");
    m.insert(174, "Cogent");
    m.insert(3320, "Deutsche Telekom");
    m.insert(2914, "NTT");
    m.insert(7018, "AT&T");
    m.insert(4134, "China Telecom");
    m.insert(7922, "Comcast");
    m
}

// ── BGP message structures ─────────────────────────────────────────────────────

/// Top-level RIS Live WebSocket message
#[derive(Deserialize, Debug)]
struct RisMessage {
    #[serde(rename = "type")]
    msg_type: String,
    data: Option<RisData>,
}

/// data field inside a ris_message
#[derive(Deserialize, Debug)]
struct RisData {
    timestamp: Option<f64>,
    id: Option<String>,       // collector name, e.g. "rrc12"
    peer_asn: Option<Value>,  // can be string or number
    path: Option<Vec<Value>>, // can be nested (AS-sets)
    announcements: Option<Vec<Announcement>>,
    withdrawals: Option<Vec<String>>,
    peer: Option<String>,     // Peer-IP-Adresse, z.B. "80.249.211.0"
}

#[derive(Deserialize, Debug)]
struct Announcement {
    next_hop: Option<String>,
    prefixes: Option<Vec<String>>,
}

// ── Message → BgpEvent conversion ──────────────────────────────────────────────

fn asn_to_u64(v: &Value) -> u64 {
    match v {
        Value::Number(n) => n.as_u64().unwrap_or(0),
        Value::String(s) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

/// Flatten possibly-nested AS-path entries (AS-sets like [64496, 64497]) into a
/// simple sequence of u64 values. We take the first element of any set.
fn flatten_path(path: &[Value]) -> Vec<u64> {
    path.iter()
        .map(|v| match v {
            Value::Array(arr) => arr.first().map(asn_to_u64).unwrap_or(0),
            _ => asn_to_u64(v),
        })
        .collect()
}

fn process_ris_data(
    data: &RisData,
    known: &HashMap<u64, &'static str>,
    tx: &Sender<BgpEvent>,
    stats: &Arc<Stats>,
    sample_rate: f64,
    collector: &str,
) {
    let ts = data.timestamp.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64()
    });

    let _ts_str = chrono::DateTime::<Utc>::from_timestamp(ts as i64, 0)
        .unwrap_or_else(Utc::now)
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();

    let peer_asn_raw = data.peer_asn.as_ref().map(asn_to_u64).unwrap_or(0);

    let path_vec: Vec<u64> = data.path.as_deref().map(flatten_path).unwrap_or_default();

    let origin = path_vec.last().copied().unwrap_or(peer_asn_raw);

    let peer_name = known
        .get(&peer_asn_raw)
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("AS{peer_asn_raw}"));

    let _origin_name = known
        .get(&origin)
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("AS{origin}"));

    // Build the last-4-hops path string
    let path_display: String = if path_vec.is_empty() {
        peer_name.clone()
    } else {
        path_vec
            .iter()
            .rev()
            .take(4)
            .rev()
            .map(|a| {
                known
                    .get(a)
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| a.to_string())
            })
            .collect::<Vec<_>>()
            .join(" → ")
    };

    // ── Announcements ──────────────────────────────────────────────────────────
    if let Some(announcements) = &data.announcements {
        for ann in announcements {
            let nh = ann.next_hop.as_deref().unwrap_or("").to_string();
            let prefixes = ann.prefixes.as_deref().unwrap_or(&[]);
            for pfx in prefixes {
                // Sampling
                if sample_rate < 1.0 && fastrand::f64() > sample_rate {
                    continue;
                }

                let event = BgpEvent {
                    id: Uuid::new_v4(),
                    timestamp: Utc::now(),
                    level: LogLevel::Info,
                    source: "ripe-ris".into(),
                    message: format!("ANNOUNCE {pfx} via {peer_name} (path: {path_display})"),
                    metadata: Some(json!({
                        "event_type": "announce",
                        "prefix":     pfx,
                        "peer_asn":   peer_asn_raw,
                        "origin_as":  origin as u32,
                        "peer_ip":    nh,
                        "as_path":    path_vec.iter().map(|&a| a as u32).collect::<Vec<u32>>(),
                        "community":  Vec::<String>::new(),
                        "collector":  collector,
                    })),
                };

                match tx.try_send(event) {
                    Ok(_) => {
                        stats.ann.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(TrySendError::Full(_)) => {
                        stats.dropped.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(TrySendError::Closed(_)) => return,
                }
            }
        }
    }

    // ── Withdrawals ────────────────────────────────────────────────────────────
    if let Some(withdrawals) = &data.withdrawals {
        for pfx in withdrawals {
            if sample_rate < 1.0 && fastrand::f64() > sample_rate {
                continue;
            }

            let event = BgpEvent {
                id: Uuid::new_v4(),
                timestamp: Utc::now(),
                level: LogLevel::Warn,
                source: "ripe-ris".into(),
                message: format!("WITHDRAW {pfx} from {peer_name}"),
                metadata: Some(json!({
                    "event_type": "withdraw",
                    "prefix":     pfx,
                    "peer_asn":   peer_asn_raw,
                    "origin_as":  origin as u32,
                    "peer_ip":    data.peer.as_deref().unwrap_or(""),
                    "as_path":    path_vec.iter().map(|&a| a as u32).collect::<Vec<u32>>(),
                    "community":  Vec::<String>::new(),
                    "collector":  collector,
                })),
            };

            match tx.try_send(event) {
                Ok(_) => {
                    stats.with.fetch_add(1, Ordering::Relaxed);
                }
                Err(TrySendError::Full(_)) => {
                    stats.dropped.fetch_add(1, Ordering::Relaxed);
                }
                Err(TrySendError::Closed(_)) => return,
            }
        }
    }
}

// ── NATS Publisher task ────────────────────────────────────────────────────────

async fn nats_publisher_task(
    id: usize,
    rx: Receiver<BgpEvent>,
    js_context: async_nats::jetstream::Context,
    cfg: Arc<Config>,
    stats: Arc<Stats>,
) {
    let subject = "bgp.events";
    let mut batch: Vec<BgpEvent> = Vec::with_capacity(cfg.batch_size);

    loop {
        // Collect events until batch_size reached or batch_timeout expires.
        let timer = tokio::time::sleep(cfg.batch_timeout);
        tokio::pin!(timer);

        loop {
            tokio::select! {
                _ = &mut timer => break,
                result = rx.recv() => {
                    match result {
                        Ok(event) => {
                            batch.push(event);
                            if batch.len() >= cfg.batch_size {
                                break;
                            }
                        }
                        Err(_) => return, // channel closed — shut down
                    }
                }
            }
        }

        if batch.is_empty() {
            continue;
        }

        let _batch_len = batch.len(); // telemetry placeholder

        // Publish each event to NATS JetStream
        for event in batch.drain(..) {
            let json_bytes = match serde_json::to_vec(&event) {
                Ok(b) => b,
                Err(e) => {
                    error!(worker = id, error = %e, "failed to serialize event");
                    stats.errors.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            };

            match js_context.publish(subject, json_bytes.into()).await {
                Ok(_) => {
                    stats.published.fetch_add(1, Ordering::Relaxed);
                }
                Err(e) => {
                    error!(worker = id, subject = subject, error = %e, "JetStream publish failed");
                    stats.errors.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
}

// ── WebSocket stream task ──────────────────────────────────────────────────────

async fn bgp_stream_task(
    tx: Sender<BgpEvent>,
    cfg: Arc<Config>,
    stats: Arc<Stats>,
    known: Arc<HashMap<u64, &'static str>>,
) {
    let url = "wss://ris-live.ripe.net/v1/ws/";
    let subscribe_msg = serde_json::to_string(&json!({
        "type": "ris_subscribe",
        "data": { "type": "UPDATE" }
    }))
    .unwrap();

    // Build a rustls ClientConfig with WebPKI roots and no ALPN.
    let mut root_store = RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let tls_config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    let connector = Connector::Rustls(std::sync::Arc::new(tls_config));

    loop {
        info!("Connecting to {url} ...");

        let request = match url.into_client_request() {
            Ok(r) => r,
            Err(e) => {
                error!("Bad URL: {e}");
                sleep(Duration::from_secs(3)).await;
                continue;
            }
        };

        let ws_stream = match tokio::time::timeout(
            Duration::from_secs(20),
            connect_async_tls_with_config(request, None, false, Some(connector.clone())),
        )
        .await
        {
            Ok(Ok((ws, _))) => ws,
            Ok(Err(e)) => {
                warn!("Connect failed: {e} — retry in 5s");
                sleep(Duration::from_secs(5)).await;
                continue;
            }
            Err(_) => {
                warn!("Connect timed out after 20s — retry in 5s");
                sleep(Duration::from_secs(5)).await;
                continue;
            }
        };

        info!("Connected ✓");

        let (mut write, mut read) = ws_stream.split();

        // Subscribe to UPDATE events
        if let Err(e) = write.send(Message::Text(subscribe_msg.clone())).await {
            warn!("Subscribe failed: {e} — retry in 3s");
            sleep(Duration::from_secs(3)).await;
            continue;
        }
        info!("Subscribed — receiving BGP UPDATEs ...");

        while let Some(msg_result) = read.next().await {
            match msg_result {
                Ok(Message::Text(text)) => {
                    match serde_json::from_str::<RisMessage>(&text) {
                        Ok(ris_msg) if ris_msg.msg_type == "ris_message" => {
                            if let Some(data) = &ris_msg.data {
                                let collector = data.id.as_deref().unwrap_or("unknown");
                                process_ris_data(data, &known, &tx, &stats, cfg.sample_rate, collector);
                            }
                        }
                        Ok(_) => {}  // ris_subscribe_ok or other control messages
                        Err(_) => {} // malformed JSON — ignore
                    }
                }
                Ok(Message::Ping(p)) => {
                    let _ = write.send(Message::Pong(p)).await;
                }
                Ok(Message::Close(_)) => {
                    warn!("Close frame received — reconnecting ...");
                    break;
                }
                Err(e) => {
                    warn!("WebSocket error: {e} — reconnecting ...");
                    break;
                }
                _ => {}
            }
        }

        sleep(Duration::from_secs(3)).await;
    }
}

// ── Stats printer ─────────────────────────────────────────────────────────────

async fn stats_task(stats: Arc<Stats>, cfg: Arc<Config>, start: Instant, chan_cap: usize) {
    let mut prev_published: u64 = 0;
    loop {
        sleep(Duration::from_secs(10)).await;
        let elapsed = start.elapsed().as_secs_f64();
        let published = stats.published.load(Ordering::Relaxed);
        let errors = stats.errors.load(Ordering::Relaxed);
        let dropped = stats.dropped.load(Ordering::Relaxed);
        let ann = stats.ann.load(Ordering::Relaxed);
        let with = stats.with.load(Ordering::Relaxed);

        let delta = published.saturating_sub(prev_published);
        prev_published = published;

        let pps_window = delta as f64 / 10.0;
        let pps_avg = if elapsed > 0.0 {
            published as f64 / elapsed
        } else {
            0.0
        };
        let ingress = if elapsed > 0.0 {
            (ann + with) as f64 / elapsed
        } else {
            0.0
        };

        println!(
            "[stats] published={published} | errors={errors} | dropped={dropped} | \
             pps_10s={pps_window:.0}/s | pps_avg={pps_avg:.0}/s | \
             ingress={ingress:.0}/s | workers={} | batch={} | \
             ann={ann} | with={with} | elapsed={elapsed:.0}s",
            cfg.workers, cfg.batch_size,
        );
        let _ = chan_cap; // suppress unused warning if needed
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Parse a NATS URL and extract credentials and clean server URL.
///
/// Handles `nats://user:pass@host:port` or `nats://host:port`.
/// Returns `(clean_url, Option<(username, password)>)`.
fn parse_nats_url(url: &str) -> (String, Option<(String, String)>) {
    if let Some(at_pos) = url.rfind('@') {
        let scheme_end = url.find("://").map(|i| i + 3).unwrap_or(0);
        let creds = &url[scheme_end..at_pos];
        let clean_url = format!("{}{}", &url[..scheme_end], &url[at_pos + 1..]);
        if let Some(colon_pos) = creds.find(':') {
            let user = creds[..colon_pos].to_string();
            let pass = creds[colon_pos + 1..].to_string();
            return (clean_url, Some((user, pass)));
        }
        return (clean_url, None);
    }
    (url.to_string(), None)
}

// ── Main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    // Install aws-lc-rs as the process-level rustls CryptoProvider.
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install rustls CryptoProvider");

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("bgp_stream=info".parse().unwrap()),
        )
        .init();

    let cfg = Arc::new(Config::from_env());
    let stats = Arc::new(Stats::default());
    let known = Arc::new(known_as_map());

    println!("{}", "=".repeat(65));
    println!("BGP Stream (RIPE NCC RIS Live) → NATS Jetstream  (Rust v2)");
    println!("{}", "=".repeat(65));
    println!("  NATS URL:    {}", cfg.nats_url);
    println!("  Workers:     {}", cfg.workers);
    println!("  Batch size:  {}", cfg.batch_size);
    println!("  Batch tmo:   {}ms", cfg.batch_timeout.as_millis());
    println!("  Sample rate: {:.0}%", cfg.sample_rate * 100.0);
    println!("  Channel cap: {}", cfg.channel_cap);
    println!("{}", "=".repeat(65));

    // Connect to NATS
    // async-nats 0.34 does not extract credentials from the URL automatically —
    // parse user:pass from URL and use ConnectOptions::user_and_password() instead.
    let (clean_nats_url, nats_creds) = parse_nats_url(&cfg.nats_url);
    let connect_opts = if let Some((user, pass)) = nats_creds {
        async_nats::ConnectOptions::new().user_and_password(user, pass)
    } else {
        async_nats::ConnectOptions::new()
    };
    let nats_client = match connect_opts.connect(&clean_nats_url).await {
        Ok(client) => {
            info!("Connected to NATS at {}", cfg.nats_url);
            client
        }
        Err(e) => {
            error!("Failed to connect to NATS: {e}");
            std::process::exit(1);
        }
    };

    // Create JetStream context
    let js = async_nats::jetstream::new(nats_client.clone());

    // Create or get the BGP_EVENTS stream
    let stream_config = async_nats::jetstream::stream::Config {
        name: "BGP_EVENTS".to_string(),
        subjects: vec!["bgp.events".to_string()],
        retention: async_nats::jetstream::stream::RetentionPolicy::WorkQueue,
        max_age: std::time::Duration::from_secs(86400), // 24 hours
        max_bytes: 500 * 1024 * 1024,                   // 500 MB
        storage: async_nats::jetstream::stream::StorageType::File,
        ..Default::default()
    };

    match js.get_or_create_stream(stream_config).await {
        Ok(_stream) => {
            info!("JetStream stream 'BGP_EVENTS' ready (subjects: bgp.events)");
        }
        Err(e) => {
            error!("Failed to create/get JetStream stream: {e}");
            std::process::exit(1);
        }
    }

    // Bounded channel: WebSocket producer → NATS publishers
    let (tx, rx) = bounded::<BgpEvent>(cfg.channel_cap);

    let start = Instant::now();

    // Spawn NATS publisher workers
    for i in 0..cfg.workers {
        let rx_i = rx.clone();
        let js_i = js.clone();
        let cfg_i = cfg.clone();
        let stats_i = stats.clone();
        tokio::spawn(async move {
            nats_publisher_task(i, rx_i, js_i, cfg_i, stats_i).await;
        });
    }

    // Spawn stats printer
    {
        let stats_s = stats.clone();
        let cfg_s = cfg.clone();
        let cap = cfg.channel_cap;
        tokio::spawn(async move {
            stats_task(stats_s, cfg_s, start, cap).await;
        });
    }

    // Run WebSocket stream (reconnects forever)
    bgp_stream_task(tx, cfg, stats, known).await;
}

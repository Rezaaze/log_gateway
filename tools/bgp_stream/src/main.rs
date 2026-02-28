/*!
BGP Stream (RIPE NCC RIS Live) → Log Gateway
============================================
Rust rewrite of tools/bgp_stream.py v3.

Architecture
────────────
  WebSocket task  ──crossbeam-channel──►  N sender tasks  ──► POST /api/v1/logs/batch
                                          (batch-100, keep-alive via reqwest pool)

Env vars (all optional)
───────────────────────
  GATEWAY_URL          default: http://localhost:8090
  GATEWAY_API_KEY      optional API key
  GATEWAY_JWT_SECRET   optional JWT secret (HS256, never expires)
  WORKERS              HTTP sender threads  (default: 8)
  BATCH_SIZE           entries per POST     (default: 100)
  BATCH_TIMEOUT_MS     max wait for full batch (default: 20)
  TENANT_ID            log tenant           (default: bgp)
  SAMPLE_RATE          0.0-1.0 keep fraction (default: 1.0)
  CHANNEL_CAP          bounded channel size  (default: 64_000)
*/

use std::{
    collections::HashMap,
    env,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use chrono::Utc;
use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};
use futures_util::{SinkExt, StreamExt};
use log_gateway::models::{LogEntry, LogLevel};
use reqwest::{header, Client};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::time::sleep;
use tokio_tungstenite::{
    connect_async_tls_with_config,
    tungstenite::{client::IntoClientRequest, Message},
};
use tracing::{error, info, warn};
use uuid::Uuid;

// ── Configuration ─────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct Config {
    gateway_url: String,
    api_key: String,
    jwt_secret: String,
    workers: usize,
    batch_size: usize,
    batch_timeout: Duration,
    tenant_id: String,
    sample_rate: f64,
    channel_cap: usize,
}

impl Config {
    fn from_env() -> Self {
        Self {
            gateway_url: env::var("GATEWAY_URL").unwrap_or_else(|_| "http://localhost:8090".into()),
            api_key: env::var("GATEWAY_API_KEY").unwrap_or_default(),
            jwt_secret: env::var("GATEWAY_JWT_SECRET").unwrap_or_default(),
            workers: env::var("WORKERS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(8),
            batch_size: env::var("BATCH_SIZE")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(100),
            batch_timeout: Duration::from_millis(
                env::var("BATCH_TIMEOUT_MS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(20),
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
    sent: AtomicU64,
    errors: AtomicU64,
    dropped: AtomicU64,
    ann: AtomicU64,
    with: AtomicU64,
}

// ── JWT (HS256, no external crate needed beyond hmac/sha2 — use jsonwebtoken) ─

fn make_jwt(secret: &str, tenant_id: &str) -> String {
    use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
    use serde::Serialize;

    #[derive(Serialize)]
    struct Claims {
        sub: String,
        tenant_id: String,
        exp: u64,
    }

    let claims = Claims {
        sub: "bgp-stream-rs".into(),
        tenant_id: tenant_id.to_owned(),
        exp: 9_999_999_999,
    };

    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .unwrap_or_default()
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
    peer_asn: Option<Value>,  // can be string or number
    path: Option<Vec<Value>>, // can be nested (AS-sets)
    announcements: Option<Vec<Announcement>>,
    withdrawals: Option<Vec<String>>,
}

#[derive(Deserialize, Debug)]
struct Announcement {
    next_hop: Option<String>,
    prefixes: Option<Vec<String>>,
}

// ── Message → LogEntry conversion ─────────────────────────────────────────────

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
    tx: &Sender<LogEntry>,
    stats: &Arc<Stats>,
    sample_rate: f64,
) {
    let ts = data.timestamp.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64()
    });

    let ts_str = chrono::DateTime::<Utc>::from_timestamp(ts as i64, 0)
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

    let origin_name = known
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

                let entry = LogEntry {
                    id: Uuid::new_v4(),
                    timestamp: Utc::now(),
                    level: LogLevel::Info,
                    source: "ripe-ris".into(),
                    message: format!("ANNOUNCE {pfx} via {peer_name} (path: {path_display})"),
                    metadata: Some(json!({
                        "event":      "ANNOUNCE",
                        "prefix":     pfx,
                        "peer_asn":   peer_asn_raw.to_string(),
                        "origin_asn": origin.to_string(),
                        "origin":     origin_name,
                        "nexthop":    nh,
                        "timestamp":  ts_str,
                    })),
                };

                match tx.try_send(entry) {
                    Ok(_) => {
                        stats.ann.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(TrySendError::Full(_)) => {
                        stats.dropped.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(TrySendError::Disconnected(_)) => return,
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

            let entry = LogEntry {
                id: Uuid::new_v4(),
                timestamp: Utc::now(),
                level: LogLevel::Warn,
                source: "ripe-ris".into(),
                message: format!("WITHDRAW {pfx} from {peer_name}"),
                metadata: Some(json!({
                    "event":      "WITHDRAW",
                    "prefix":     pfx,
                    "peer_asn":   peer_asn_raw.to_string(),
                    "origin_asn": origin.to_string(),
                    "origin":     origin_name,
                    "nexthop":    "",
                    "timestamp":  ts_str,
                })),
            };

            match tx.try_send(entry) {
                Ok(_) => {
                    stats.with.fetch_add(1, Ordering::Relaxed);
                }
                Err(TrySendError::Full(_)) => {
                    stats.dropped.fetch_add(1, Ordering::Relaxed);
                }
                Err(TrySendError::Disconnected(_)) => return,
            }
        }
    }
}

// ── HTTP sender task ───────────────────────────────────────────────────────────

async fn sender_task(
    id: usize,
    rx: Receiver<LogEntry>,
    client: Client,
    cfg: Arc<Config>,
    stats: Arc<Stats>,
    jwt: String,
) {
    let batch_url = format!("{}/api/v1/logs/batch", cfg.gateway_url);

    let mut batch: Vec<LogEntry> = Vec::with_capacity(cfg.batch_size);
    let mut deadline = Instant::now() + cfg.batch_timeout;

    loop {
        // How long until the deadline?
        let now = Instant::now();
        let remaining = if now >= deadline {
            Duration::from_millis(1)
        } else {
            deadline - now
        };

        // Non-blocking drain up to batch_size or timeout
        match rx.recv_timeout(remaining.min(cfg.batch_timeout)) {
            Ok(entry) => {
                batch.push(entry);
            }
            Err(_) => {} // timeout or disconnected — fall through to flush check
        }

        let now = Instant::now();
        if batch.is_empty() {
            if now >= deadline {
                deadline = now + cfg.batch_timeout;
            }
            continue;
        }

        // Flush when full or deadline reached
        if batch.len() >= cfg.batch_size || now >= deadline {
            let n = batch.len();
            let mut req = client
                .post(&batch_url)
                .header("Content-Type", "application/json")
                .header("X-Tenant-ID", &cfg.tenant_id)
                .header("User-Agent", "bgp-stream-rs/1.0")
                .json(&batch);

            if !cfg.api_key.is_empty() {
                req = req.header("X-API-Key", &cfg.api_key);
            }
            if !jwt.is_empty() {
                req = req.header(header::AUTHORIZATION, format!("Bearer {jwt}"));
            }

            match req.send().await {
                Ok(resp) => {
                    if resp.status().is_success() {
                        stats.sent.fetch_add(n as u64, Ordering::Relaxed);
                    } else {
                        warn!(
                            worker = id,
                            status = resp.status().as_u16(),
                            "batch rejected"
                        );
                        stats.errors.fetch_add(n as u64, Ordering::Relaxed);
                    }
                }
                Err(e) => {
                    error!(worker = id, error = %e, "send failed");
                    stats.errors.fetch_add(n as u64, Ordering::Relaxed);
                }
            }

            batch.clear();
            deadline = Instant::now() + cfg.batch_timeout;
        }
    }
}

// ── WebSocket stream task ──────────────────────────────────────────────────────

async fn bgp_stream_task(
    tx: Sender<LogEntry>,
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

        let ws_stream = match connect_async_tls_with_config(request, None, false, None).await {
            Ok((ws, _)) => ws,
            Err(e) => {
                warn!("Connect failed: {e} — retry in 3s");
                sleep(Duration::from_secs(3)).await;
                continue;
            }
        };

        info!("Connected ✓");

        let (mut write, mut read) = ws_stream.split();

        // Subscribe to UPDATE events
        if let Err(e) = write
            .send(Message::Text(subscribe_msg.clone().into()))
            .await
        {
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
                                process_ris_data(data, &known, &tx, &stats, cfg.sample_rate);
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
    let mut prev_sent: u64 = 0;
    loop {
        sleep(Duration::from_secs(10)).await;
        let elapsed = start.elapsed().as_secs_f64();
        let sent = stats.sent.load(Ordering::Relaxed);
        let errors = stats.errors.load(Ordering::Relaxed);
        let dropped = stats.dropped.load(Ordering::Relaxed);
        let ann = stats.ann.load(Ordering::Relaxed);
        let with = stats.with.load(Ordering::Relaxed);

        let delta = sent.saturating_sub(prev_sent);
        prev_sent = sent;

        let rps_window = delta as f64 / 10.0;
        let rps_avg = if elapsed > 0.0 {
            sent as f64 / elapsed
        } else {
            0.0
        };
        let ingress = if elapsed > 0.0 {
            (ann + with) as f64 / elapsed
        } else {
            0.0
        };

        println!(
            "[stats] sent={sent} | errors={errors} | dropped={dropped} | \
             rps_10s={rps_window:.0}/s | rps_avg={rps_avg:.0}/s | \
             ingress={ingress:.0}/s | workers={} | batch={} | \
             ann={ann} | with={with} | elapsed={elapsed:.0}s",
            cfg.workers, cfg.batch_size,
        );
        let _ = chan_cap; // suppress unused warning if needed
    }
}

// ── Main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("bgp_stream=info".parse().unwrap()),
        )
        .init();

    let cfg = Arc::new(Config::from_env());
    let stats = Arc::new(Stats::default());
    let known = Arc::new(known_as_map());

    let jwt = if cfg.jwt_secret.is_empty() {
        String::new()
    } else {
        make_jwt(&cfg.jwt_secret, &cfg.tenant_id)
    };

    println!("{}", "=".repeat(65));
    println!("BGP Stream (RIPE NCC RIS Live) → Log Gateway  (Rust v1)");
    println!("{}", "=".repeat(65));
    println!("  Gateway:     {}/api/v1/logs/batch", cfg.gateway_url);
    println!("  Workers:     {}", cfg.workers);
    println!("  Batch size:  {}", cfg.batch_size);
    println!("  Batch tmo:   {}ms", cfg.batch_timeout.as_millis());
    println!("  Sample rate: {:.0}%", cfg.sample_rate * 100.0);
    println!("  Channel cap: {}", cfg.channel_cap);
    println!(
        "  API-Key:     {}",
        if cfg.api_key.is_empty() { "no" } else { "yes" }
    );
    println!(
        "  JWT:         {}",
        if jwt.is_empty() { "no" } else { "yes" }
    );
    println!("{}", "=".repeat(65));

    // Build shared reqwest client (connection pool = keep-alive per host)
    let client = Client::builder()
        .pool_max_idle_per_host(cfg.workers + 2)
        .timeout(Duration::from_secs(10))
        .tcp_keepalive(Duration::from_secs(30))
        .use_rustls_tls()
        .build()
        .expect("failed to build HTTP client");

    // Bounded channel: WebSocket producer → sender consumers
    let (tx, rx) = bounded::<LogEntry>(cfg.channel_cap);

    let start = Instant::now();

    // Spawn sender workers
    for i in 0..cfg.workers {
        let rx_i = rx.clone();
        let client_i = client.clone();
        let cfg_i = cfg.clone();
        let stats_i = stats.clone();
        let jwt_i = jwt.clone();
        tokio::spawn(async move {
            sender_task(i, rx_i, client_i, cfg_i, stats_i, jwt_i).await;
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

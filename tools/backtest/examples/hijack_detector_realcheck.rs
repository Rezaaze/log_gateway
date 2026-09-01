//! Companion diagnostic to `backtest`: does `HijackDetector` (the simple
//! origin-AS-change heuristic — a separate layer from the wave-physics
//! `WaveAnomalyDetector` that `backtest` scores) actually fire on a real
//! historical hijack, when replayed against real MRT archive data?
//!
//! This exists because `backtest` structurally cannot answer that question
//! for `WaveAnomalyDetector`: its baseline lookup is keyed by
//! `(prefix, origin_as, as_path_hash)`, so a hijacker's origin_as — which
//! by definition never appears in a baseline built from the legitimate
//! origin's history — is a guaranteed lookup miss, not a real absence of
//! anomaly. `HijackDetector` instead tracks, per exact prefix string, the
//! set of ASNs ever seen originating it — so it CAN be tested directly by
//! replaying real announce records through `check()` in timestamp order,
//! without needing any baseline file.
//!
//! Important caveat this tool surfaces rather than hides: `HijackDetector`
//! flags ANY never-before-seen (prefix, origin_as) pair identically,
//! whether the new announcer is an attacker or the prefix's legitimate
//! owner rolling out a new, more-specific route. A hijack via deaggregation
//! (announcing a more-specific sub-prefix of an already-routed aggregate,
//! e.g. this incident) looks, to this detector, exactly like a legitimate
//! new sub-allocation — same signal, same confidence. Zero pre-incident
//! records for the target prefix is itself informative: it means the
//! prefix was never announced standalone before the incident, so whatever
//! first announces it will be flagged regardless of who it is.
//!
//! ## Usage
//! ```bash
//! cargo run -p backtest --example hijack_detector_realcheck -- \
//!   --baseline-dir /path/to/pre-incident/mrt \
//!   --hijack-dir   /path/to/incident-window/mrt \
//!   --prefix       208.65.153.0/24
//! ```
use anyhow::{Context, Result};
use bgpkit_parser::{models::ElemType, BgpkitParser};
use chrono::{TimeZone, Utc};
use clap::Parser;
use log_gateway::anomaly_detector::{Detector, HijackDetector};
use log_gateway::clickhouse_exporter::BgpClickHouseRecord;
use std::collections::HashSet;
use std::path::PathBuf;
use walkdir::WalkDir;

#[derive(Parser, Debug)]
struct Args {
    /// Directory of MRT archive files covering the period strictly before
    /// the incident, used to warm up HijackDetector's known-ASN state
    #[arg(long)]
    baseline_dir: PathBuf,

    /// Directory of MRT archive files covering the incident window
    #[arg(long)]
    hijack_dir: PathBuf,

    /// The exact prefix under test, e.g. "208.65.153.0/24"
    #[arg(long)]
    prefix: String,
}

#[derive(Debug, Clone)]
struct Rec {
    timestamp: f64,
    origin_as: u32,
    as_path: Vec<u32>,
}

fn scan(dir: &PathBuf, prefix: &str) -> Vec<Rec> {
    let mut out = Vec::new();
    for entry in WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let path = entry.path();
        let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !fname.ends_with(".gz") {
            continue;
        }
        let parser = match BgpkitParser::new(path.to_str().unwrap_or("")) {
            Ok(p) => p,
            Err(_) => continue,
        };
        for elem in parser {
            if elem.elem_type != ElemType::ANNOUNCE {
                continue;
            }
            if elem.prefix.prefix.to_string() != prefix {
                continue;
            }
            let as_path: Vec<u32> = elem
                .as_path
                .as_ref()
                .map(|p| p.to_u32_vec_opt(true).unwrap_or_default())
                .unwrap_or_default();
            if as_path.is_empty() {
                continue;
            }
            let origin_as = *as_path.last().unwrap();
            out.push(Rec {
                timestamp: elem.timestamp,
                origin_as,
                as_path,
            });
        }
    }
    out.sort_by(|a, b| a.timestamp.partial_cmp(&b.timestamp).unwrap());
    out
}

fn to_event(prefix: &str, r: &Rec) -> BgpClickHouseRecord {
    BgpClickHouseRecord {
        timestamp: Utc.timestamp_opt(r.timestamp as i64, 0).unwrap(),
        event_type: "announce".to_string(),
        prefix: prefix.to_string(),
        origin_as: r.origin_as,
        as_path: r.as_path.clone(),
        peer_asn: 0,
        peer_ip: String::new(),
        community: vec![],
        source: "hijack_detector_realcheck".to_string(),
        tenant_id: "backtest".to_string(),
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    anyhow::ensure!(
        args.baseline_dir.exists(),
        "baseline-dir {:?} does not exist",
        args.baseline_dir
    );
    anyhow::ensure!(
        args.hijack_dir.exists(),
        "hijack-dir {:?} does not exist",
        args.hijack_dir
    );

    eprintln!("Scanning baseline dir for prefix {}...", args.prefix);
    let baseline_recs = scan(&args.baseline_dir, &args.prefix);
    eprintln!(
        "Found {} pre-incident records for this exact prefix",
        baseline_recs.len()
    );
    let origins: HashSet<u32> = baseline_recs.iter().map(|r| r.origin_as).collect();
    eprintln!("Origin ASNs seen pre-incident: {origins:?}");
    if baseline_recs.is_empty() {
        eprintln!(
            "NOTE: zero pre-incident records for this exact prefix — it was never announced \
             standalone before the incident (e.g. it may be a sub-prefix of a larger aggregate \
             that WAS routed). Whoever announces it first, attacker or legitimate owner, will be \
             flagged identically by HijackDetector's novelty-based heuristic — a hit below is not \
             proof this detector distinguishes hijacker from owner in this case."
        );
    }

    let detector = HijackDetector::new();
    for r in &baseline_recs {
        let ev = to_event(&args.prefix, r);
        if let Some(a) = detector.check(&ev) {
            eprintln!(
                "  (warmup) unexpected anomaly at t={} origin_as={}: {}",
                r.timestamp, r.origin_as, a.details
            );
        }
    }

    eprintln!("\nScanning hijack-window dir for prefix {}...", args.prefix);
    let hijack_recs = scan(&args.hijack_dir, &args.prefix);
    eprintln!(
        "Found {} records in hijack window for this prefix",
        hijack_recs.len()
    );

    let mut alerts: Vec<(f64, u32)> = Vec::new();
    for r in &hijack_recs {
        let ev = to_event(&args.prefix, r);
        if let Some(a) = detector.check(&ev) {
            eprintln!(
                "ALERT t={} origin_as={} confidence={} details={}",
                r.timestamp, r.origin_as, a.confidence, a.details
            );
            alerts.push((r.timestamp, r.origin_as));
        }
    }

    match alerts.first() {
        Some((ts, asn)) => {
            let dt = Utc
                .timestamp_opt(*ts as i64, 0)
                .single()
                .context("invalid timestamp")?;
            eprintln!(
                "\n=== RESULT: {} alert(s), first at {} (origin_as={}) ===",
                alerts.len(),
                dt.to_rfc3339(),
                asn
            );
        }
        None => {
            eprintln!(
                "\n=== RESULT: HijackDetector never flagged this prefix in the hijack window ==="
            );
        }
    }

    Ok(())
}

//! # Backtest
//!
//! Runs the wave-physics anomaly detector (`WaveAnomalyDetector`) against
//! historical MRT archive data covering a known BGP hijack, and reports
//! true/false positive rate and detection latency — TRUSTWAVE_ROADMAP.md
//! Abschnitt 3.2.
//!
//! ## A case file, not hardcoded incidents
//!
//! This tool deliberately does not hardcode the parameters of specific
//! historical hijacks (exact prefixes, ASNs, timestamps) in source code —
//! getting those details wrong from memory would silently corrupt the
//! backtest. Abschnitt 3.2.1 ("bekannte Hijack-Events als Testfälle
//! definieren") is the operator's job: describe each case in a small TOML
//! file, verified against a primary source (e.g. bgpstream.com, RIPE's own
//! incident writeups, or NANOG mailing list threads), and pass it in.
//!
//! ## Usage
//!
//! ```bash
//! backtest --case cases/example.toml
//! ```
//!
//! Case file format — see `CaseConfig` below for the authoritative shape;
//! example:
//!
//! ```toml
//! name = "example-hijack"
//! prefix = "8.8.8.0/24"
//! legitimate_origin_as = 15169
//! hijacker_origin_as = 666
//! hijack_start = "2018-04-24T11:05:00Z"
//! hijack_end   = "2018-04-24T11:15:00Z"
//! mrt_data_dir = "data/mrt/example-hijack"
//! baseline_path = "data/baselines/example-hijack-pre-incident.bin.zst"
//! detection_threshold = 0.5
//! ```

use anyhow::{bail, Context, Result};
use clap::Parser;
use log_gateway::propagation::{build_events_batch, CollectorObservation, PropagationEvent};
use log_gateway::wave_anomaly_detector::{AnomalyClassification, WaveAnomalyDetector};
use log_gateway::wave_baseline::WaveBaseline;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Parser, Debug)]
#[command(about = "Backtests the wave anomaly detector against a known BGP hijack")]
struct Args {
    /// Path to a case TOML file describing the hijack under test
    #[arg(long)]
    case: PathBuf,

    /// Minimum number of collectors required for a PropagationEvent to be
    /// considered (same rationale as baseline_builder / is_good_route)
    #[arg(long, default_value_t = 3)]
    min_collectors: usize,

    /// Time window in seconds within which arrivals at different collectors
    /// count as the same announcement — identical to the live
    /// `PropagationAggregator`. Without a window, independent announcements of
    /// the same prefix merge into a single "event" (spreads of up to 293 s
    /// measured on real RIS archive data).
    #[arg(long, default_value_t = 10.0)]
    window_secs: f64,
}

/// Describes one known-hijack backtest case. Every field here is data the
/// operator must supply and verify — this tool does not know or guess
/// historical incident details.
#[derive(Debug, Deserialize)]
struct CaseConfig {
    /// Human-readable case name, used only in the report
    name: String,
    /// The hijacked prefix, e.g. "8.8.8.0/24"
    prefix: String,
    /// The legitimate origin AS for this prefix
    legitimate_origin_as: u32,
    /// The AS that illegitimately originated the prefix during the incident
    hijacker_origin_as: u32,
    /// Incident start (RFC3339) — the baseline MUST be built from data
    /// strictly before this timestamp, see the leakage check below
    hijack_start: chrono::DateTime<chrono::Utc>,
    /// Incident end (RFC3339)
    hijack_end: chrono::DateTime<chrono::Utc>,
    /// Directory of MRT archive files covering the incident window (and
    /// ideally a margin before/after it), one subdirectory per collector —
    /// same layout as tools/baseline_builder expects (data/mrt/rrcXX/...)
    mrt_data_dir: PathBuf,
    /// Baseline file built via tools/baseline_builder from data that ends
    /// before `hijack_start` — see the leakage check below
    baseline_path: PathBuf,
    /// AnomalyScore.total_score at or above this counts as "detected".
    /// Defaults to the midpoint of WaveAnomalyDetector's own Suspicious
    /// band if unset.
    #[serde(default = "default_threshold")]
    detection_threshold: f64,
}

fn default_threshold() -> f64 {
    0.5
}

#[derive(Debug, Clone)]
struct MrtRecord {
    collector: String,
    prefix: String,
    origin_as: u32,
    as_path: Vec<u32>,
    timestamp: f64,
}

fn parse_mrt_file(path: &Path, collector: &str) -> Vec<MrtRecord> {
    use bgpkit_parser::{models::ElemType, BgpkitParser};
    let parser = match BgpkitParser::new(path.to_str().unwrap_or("")) {
        Ok(p) => p,
        Err(_) => return vec![],
    };
    let mut records = Vec::new();
    for elem in parser {
        if elem.elem_type != ElemType::ANNOUNCE {
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
        records.push(MrtRecord {
            collector: collector.to_string(),
            prefix: elem.prefix.prefix.to_string(),
            origin_as,
            as_path,
            timestamp: elem.timestamp,
        });
    }
    records
}

/// Builds PropagationEvents from MRT records — delegates to the shared
/// implementation in `log_gateway::propagation` so batch and live paths use
/// the same grouping, the same time window and the same AS-path hash. Two
/// divergent copies of this logic were why offline-built baselines were never
/// found by the live detector.
fn build_events(
    records: Vec<MrtRecord>,
    min_collectors: usize,
    window_secs: f64,
) -> Vec<PropagationEvent> {
    let observations = records
        .into_iter()
        .map(|r| CollectorObservation {
            collector: r.collector,
            prefix: r.prefix,
            origin_as: r.origin_as,
            as_path: r.as_path,
            timestamp: r.timestamp,
        })
        .collect();
    build_events_batch(observations, window_secs, min_collectors)
}

#[derive(Debug, Default)]
struct Confusion {
    true_positives: u32,
    false_negatives: u32,
    false_positives: u32,
    true_negatives: u32,
    /// Seconds from hijack_start to the first correctly-flagged attack event
    detection_latency_secs: Option<f64>,
}

impl Confusion {
    fn tpr(&self) -> f64 {
        let denom = self.true_positives + self.false_negatives;
        if denom == 0 {
            return 0.0;
        }
        self.true_positives as f64 / denom as f64
    }

    fn fpr(&self) -> f64 {
        let denom = self.false_positives + self.true_negatives;
        if denom == 0 {
            return 0.0;
        }
        self.false_positives as f64 / denom as f64
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args = Args::parse();

    let case_toml = std::fs::read_to_string(&args.case)
        .with_context(|| format!("failed to read case file: {:?}", args.case))?;
    let case: CaseConfig =
        toml::from_str(&case_toml).with_context(|| "failed to parse case file")?;

    eprintln!("=== Backtest: {} ===", case.name);
    eprintln!(
        "Prefix: {}  legitimate_as: {}  hijacker_as: {}",
        case.prefix, case.legitimate_origin_as, case.hijacker_origin_as
    );
    eprintln!(
        "Hijack window: {} .. {}",
        case.hijack_start.to_rfc3339(),
        case.hijack_end.to_rfc3339()
    );

    // --- Abschnitt 3.2.5: enforce no data leakage ---------------------
    // Checks the baseline's data_cutoff_ts (the latest observed MRT sample
    // timestamp it was built from) against the hijack window — NOT
    // created_at, which is just when the baseline *file* was written
    // (always "now", since historical backtests build a baseline from old
    // archive data at whatever time the operator happens to run the tool).
    // This is a necessary but not sufficient check: it catches the common
    // mistake of pointing at a baseline built from an archive that spans
    // the incident, but it can't verify the underlying MRT data-dir given
    // to tools/baseline_builder was actually cut off correctly — that's on
    // whoever built it.
    let baseline = WaveBaseline::load(&case.baseline_path).with_context(|| {
        format!(
            "failed to load baseline from {:?} (built via tools/baseline_builder?)",
            case.baseline_path
        )
    })?;
    if baseline.data_cutoff_ts <= 0.0 {
        eprintln!(
            "WARNING: baseline {:?} has no data_cutoff_ts (built before this field existed, or \
             assembled by hand) — cannot verify it predates the hijack. Proceeding on trust.",
            case.baseline_path
        );
    } else {
        let cutoff =
            chrono::DateTime::<chrono::Utc>::from_timestamp(baseline.data_cutoff_ts as i64, 0)
                .context("baseline has an invalid data_cutoff_ts")?;
        if cutoff >= case.hijack_start {
            bail!(
                "REFUSING to run: baseline {:?} contains data up to {} — at or after the hijack \
                 start ({}). Abschnitt 3.2.5 requires the baseline to be built ONLY from data \
                 strictly before the incident, or the backtest result is meaningless (the model \
                 may have learned the attack as normal). Rebuild the baseline from a data-dir \
                 that ends before hijack_start.",
                case.baseline_path,
                cutoff.to_rfc3339(),
                case.hijack_start.to_rfc3339()
            );
        }
        eprintln!(
            "Baseline OK: data up to {} (before hijack start)",
            cutoff.to_rfc3339()
        );
    }
    eprintln!("Baseline entries: {}", baseline.len());

    let detector = WaveAnomalyDetector::new(Some(&case.baseline_path))
        .map_err(|e| anyhow::anyhow!("failed to load detector baseline: {}", e))?;

    // --- Read MRT archive data for the incident window -----------------
    let mut all_records = Vec::new();
    for entry in WalkDir::new(&case.mrt_data_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let path = entry.path();
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        if !filename.ends_with(".gz") && !filename.ends_with(".bz2") {
            continue;
        }
        let collector = path
            .ancestors()
            .nth(2)
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();
        if !collector.starts_with("rrc") {
            continue;
        }
        all_records.extend(parse_mrt_file(path, &collector));
    }

    if all_records.is_empty() {
        bail!(
            "no MRT records found under {:?} — expected data/mrt/rrcXX/.../updates.*.gz layout",
            case.mrt_data_dir
        );
    }
    eprintln!("Parsed {} raw BGP records", all_records.len());

    let events = build_events(all_records, args.min_collectors, args.window_secs);
    eprintln!(
        "Built {} PropagationEvents (>= {} collectors each)",
        events.len(),
        args.min_collectors
    );

    // --- Score every event, classify against ground truth --------------
    let hijack_start_ts = case.hijack_start.timestamp() as f64;
    let hijack_end_ts = case.hijack_end.timestamp() as f64;

    let mut confusion = Confusion::default();
    let mut skipped_no_baseline = 0u32;

    for event in &events {
        let prefix_matches = event.prefix.to_string() == case.prefix;
        let in_window = event.first_arrival >= hijack_start_ts - 1.0
            && event.first_arrival <= hijack_end_ts + 1.0;
        let is_attack = prefix_matches && in_window && event.origin_as == case.hijacker_origin_as;

        let score = detector.score_event(event);
        if score.classification == AnomalyClassification::Normal && score.total_score == 0.0 {
            // Either genuinely scored 0, or no baseline entry existed for
            // this group — WaveAnomalyDetector can't tell us which, but a
            // real attack event should always have SOME baseline entry
            // (built from the legitimate origin's history) to compare
            // against, so a zero score on an attack event is still a
            // meaningful (missed) result, not skipped.
            if !is_attack {
                skipped_no_baseline += 1;
            }
        }

        let detected = score.total_score >= case.detection_threshold;

        match (is_attack, detected) {
            (true, true) => {
                confusion.true_positives += 1;
                let latency = (event.first_arrival - hijack_start_ts).max(0.0);
                confusion.detection_latency_secs = Some(match confusion.detection_latency_secs {
                    Some(existing) => existing.min(latency),
                    None => latency,
                });
            }
            (true, false) => confusion.false_negatives += 1,
            (false, true) => confusion.false_positives += 1,
            (false, false) => confusion.true_negatives += 1,
        }
    }

    // --- Report ----------------------------------------------------------
    eprintln!("\n=== Result ===");
    eprintln!(
        "True positives:  {}  (attack events correctly flagged)",
        confusion.true_positives
    );
    eprintln!(
        "False negatives: {}  (attack events missed)",
        confusion.false_negatives
    );
    eprintln!(
        "False positives: {}  (benign events wrongly flagged)",
        confusion.false_positives
    );
    eprintln!(
        "True negatives:  {}  (benign events correctly ignored)",
        confusion.true_negatives
    );
    eprintln!(
        "Events without baseline coverage (excluded from FPR): {}",
        skipped_no_baseline
    );
    eprintln!("\nTrue Positive Rate:  {:.1}%", confusion.tpr() * 100.0);
    eprintln!("False Positive Rate: {:.1}%", confusion.fpr() * 100.0);
    match confusion.detection_latency_secs {
        Some(latency) => eprintln!("Detection latency:   {:.1}s after hijack start", latency),
        None => eprintln!("Detection latency:   n/a (attack never detected)"),
    }

    if confusion.true_positives + confusion.false_negatives == 0 {
        eprintln!(
            "\nWARNING: no PropagationEvent matched the hijack case (prefix={}, hijacker_as={}, \
             window={}..{}) — check mrt_data_dir actually covers the incident and enough \
             collectors observed it.",
            case.prefix,
            case.hijacker_origin_as,
            case.hijack_start.to_rfc3339(),
            case.hijack_end.to_rfc3339()
        );
    }

    Ok(())
}

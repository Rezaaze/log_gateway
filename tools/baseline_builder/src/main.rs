//! # Baseline Builder
//!
//! Liest MRT-Archivdaten von mehreren RIPE RIS Kollektoren und baut
//! daraus die PropagationBaseline (Wellenphysik-Fingerabdrücke).
//!
//! ## Verzeichnisstruktur erwartet (von download_mrt.sh):
//!
//! ```text
//! data/mrt/
//! ├── rrc00/2024.01/updates.20240101.0000.gz
//! ├── rrc00/2024.01/updates.20240101.0005.gz
//! ├── rrc12/2024.01/updates.20240101.0000.gz
//! └── rrc12/2024.01/updates.20240101.0005.gz
//! ```
//!
//! ## Verwendung:
//! ```bash
//! baseline-builder \
//!   --data-dir data/mrt/ \
//!   --output   data/baselines/baseline.bin.zst \
//!   --min-collectors 3
//! ```

use anyhow::Result;
use clap::Parser;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use log_gateway::propagation::PropagationEvent;
use log_gateway::wave_baseline::{is_good_route, BaselineBuilder};
use rayon::prelude::*;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Instant;
use walkdir::WalkDir;

#[derive(Parser, Debug)]
#[command(about = "Baut PropagationBaseline aus RIPE RIS MRT-Archivdaten")]
struct Args {
    /// Verzeichnis mit MRT-Daten (Struktur: data/mrt/rrcXX/YYYY.MM/updates.*.gz)
    #[arg(long, default_value = "data/mrt")]
    data_dir: PathBuf,

    /// Ausgabedatei für die Baseline (bincode + zstd)
    #[arg(long, default_value = "data/baselines/baseline.bin.zst")]
    output: PathBuf,

    /// Mindestanzahl Kollektoren pro PropagationEvent
    #[arg(long, default_value_t = 3)]
    min_collectors: usize,
}

/// Einfacher Schlüssel für das Zusammenführen über Kollektoren
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SlotKey {
    /// Dateiname ohne Kollektor-Prefix (z.B. "updates.20240101.0000.gz")
    filename: String,
}

/// Ein einzelner BGP-Eintrag aus einer MRT-Datei
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

/// Gruppiert Records nach (prefix, origin_as, path_hash) und baut PropagationEvents.
fn build_events(records: Vec<MrtRecord>, min_collectors: usize) -> Vec<PropagationEvent> {
    // Zwischenspeicher: GroupKey → Map<collector, timestamp>
    // Für gleiche Announcements von verschiedenen Kollektoren innerhalb ±30s
    let mut groups: HashMap<(String, u32, u64), BTreeMap<String, f64>> = HashMap::new();

    for record in records {
        // path_hash: gleicher Algorithmus wie GroupKey::from_record()
        let path_hash: u64 = record.as_path.iter().fold(0u64, |acc, &asn| {
            acc.wrapping_mul(31).wrapping_add(asn as u64)
        });
        let key = (record.prefix.clone(), record.origin_as, path_hash);
        let entry = groups.entry(key).or_default();
        // Behalte frühesten Timestamp pro Kollektor (falls Duplikate)
        entry
            .entry(record.collector.clone())
            .and_modify(|t| {
                if record.timestamp < *t {
                    *t = record.timestamp;
                }
            })
            .or_insert(record.timestamp);
    }

    // Konvertiere Gruppen mit genug Kollektoren in PropagationEvents
    let mut events = Vec::new();
    for ((prefix_str, origin_as, _), arrivals) in groups {
        if arrivals.len() < min_collectors {
            continue;
        }
        let prefix = match prefix_str.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let first = arrivals.values().copied().fold(f64::INFINITY, f64::min);
        let last = arrivals.values().copied().fold(f64::NEG_INFINITY, f64::max);
        let mut order: Vec<String> = arrivals.keys().cloned().collect();
        order.sort_by(|a, b| arrivals[a].partial_cmp(&arrivals[b]).unwrap());

        // AS-Pfad aus erstem Arrival rekonstruieren (näherungsweise)
        let as_path = vec![origin_as]; // Vereinfachung für Batch-Verarbeitung

        events.push(PropagationEvent {
            prefix,
            origin_as,
            as_path,
            arrivals,
            first_arrival: first,
            last_arrival: last,
            spread_ms: (last - first) * 1000.0,
            arrival_order: order,
        });
    }
    events
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args = Args::parse();

    // Alle MRT-Dateien entdecken, gruppiert nach Zeitslot
    eprintln!("Scanne Verzeichnis: {}", args.data_dir.display());
    let mut slots: HashMap<SlotKey, Vec<(String, PathBuf)>> = HashMap::new();

    for entry in WalkDir::new(&args.data_dir)
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

        if !filename.ends_with(".gz") {
            continue;
        }

        // Kollektor-Name aus Verzeichnisstruktur: data/mrt/rrc12/2024.01/updates...gz
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

        slots
            .entry(SlotKey { filename })
            .or_default()
            .push((collector, path.to_path_buf()));
    }

    eprintln!("Gefunden: {} Zeitslots", slots.len());

    // Zwei Balken: einer für Slots, einer für Statistiken
    let mp = MultiProgress::new();

    let slot_list: Vec<_> = slots.into_iter().collect();
    let pb_slots = mp.add(ProgressBar::new(slot_list.len() as u64));
    pb_slots.set_style(ProgressStyle::with_template(
        "[{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} Slots ({per_sec} slots/s, ETA {eta})"
    ).unwrap().progress_chars("=>-"));

    let pb_stats = mp.add(ProgressBar::new_spinner());
    pb_stats.set_style(ProgressStyle::with_template("  Events: {msg}").unwrap());

    let start = Instant::now();

    // Baseline-Builder
    let mut builder = BaselineBuilder::new();
    let mut total_events = 0u64;
    let mut good_events = 0u64;
    let mut total_bytes = 0u64;

    // Zeitslots verarbeiten (sequenziell, aber Dateien pro Slot parallel)
    for (_slot, files) in &slot_list {
        // Dateigrößen für diesen Slot sammeln
        for (_, path) in files {
            if let Ok(metadata) = std::fs::metadata(path) {
                total_bytes += metadata.len();
            }
        }

        // Alle Kollektor-Dateien dieses Slots parallel parsen
        let all_records: Vec<MrtRecord> = files
            .par_iter()
            .flat_map(|(collector, path)| parse_mrt_file(path, collector))
            .collect();

        // PropagationEvents aus gemergten Records bauen
        let events = build_events(all_records, args.min_collectors);

        for event in &events {
            total_events += 1;
            if is_good_route(event) {
                builder.add_event(event);
                good_events += 1;
            }
        }

        // Fortschrittsanzeige aktualisieren
        pb_stats.set_message(format!(
            "{} total / {} good ({:.1}%) — {} Präfixe — {:.0} events/s — {:.1} MB",
            total_events,
            good_events,
            if total_events > 0 {
                good_events as f64 / total_events as f64 * 100.0
            } else {
                0.0
            },
            builder.entry_count(),
            total_events as f64 / start.elapsed().as_secs_f64().max(0.001),
            total_bytes as f64 / 1_000_000.0,
        ));
        pb_slots.inc(1);
    }

    pb_slots.finish_with_message("Alle Slots verarbeitet");
    pb_stats.finish();

    // Baseline finalisieren und speichern
    let store = builder.build();

    // Ausgabeverzeichnis anlegen
    if let Some(parent) = args.output.parent() {
        std::fs::create_dir_all(parent)?;
    }

    log_gateway::wave_baseline::save_baseline(&store, &args.output)?;

    // Zusammenfassung ausgeben
    let elapsed = start.elapsed();
    eprintln!("\n=== Baseline Builder Ergebnis ===");
    eprintln!("Laufzeit:          {:.1}s", elapsed.as_secs_f64());
    eprintln!("Zeitslots:         {}", slot_list.len());
    eprintln!(
        "Verarbeitete Daten: {:.1} MB",
        total_bytes as f64 / 1_000_000.0
    );
    eprintln!("Events total:      {}", total_events);
    eprintln!(
        "Events gut:        {} ({:.1}%)",
        good_events,
        if total_events > 0 {
            good_events as f64 / total_events as f64 * 100.0
        } else {
            0.0
        }
    );
    eprintln!("Baseline-Einträge: {} (reliable)", store.len());
    eprintln!("Ausgabe:           {}", args.output.display());

    Ok(())
}

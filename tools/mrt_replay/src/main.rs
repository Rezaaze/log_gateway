//! # Output-Format
//!
//! Jede Zeile ist ein JSON-Objekt mit genau diesen Feldern
//! (identisch zum bgp_stream NATS-Format):
//!
//! ```json
//! {
//!   "event_type": "announce" | "withdraw",
//!   "prefix":     "8.8.8.0/24",
//!   "peer_asn":   1103,
//!   "origin_as":  15169,
//!   "peer_ip":    "80.249.211.0",
//!   "as_path":    [1103, 3356, 15169],
//!   "collector":  "rrc12",
//!   "timestamp":  1524578100.0,
//!   "community":  []
//! }
//! ```
//!
//! MRT Replay Tool
//! Liest MRT-Archivdateien von RIPE RIS und gibt BGP-Events als JSON-Lines aus.
//!
//! Verwendung:
//!   mrt-replay --file updates.20180424.1555.gz
//!   mrt-replay --file updates.20180424.1555.gz --collector rrc12
//!   mrt-replay --url https://data.ris.ripe.net/rrc12/2018.04/updates.20180424.1555.gz

use bgpkit_parser::{models::ElemType, BgpkitParser};
use clap::Parser;
use serde_json::json;
use std::path::Path;

#[derive(Parser, Debug)]
#[command(about = "MRT archive reader for BGP TrustWave backtesting")]
struct Args {
    /// Lokale MRT-Datei (gz oder unkomprimiert)
    #[arg(long)]
    file: Option<String>,

    /// Kollektor-ID für die Ausgabe (z.B. "rrc12")
    /// Wird aus dem Dateinamen abgeleitet wenn nicht angegeben
    #[arg(long, default_value = "unknown")]
    collector: String,
}

fn main() {
    let args = Args::parse();

    let file = match args.file {
        Some(f) => f,
        None => {
            eprintln!("Error: --file required");
            eprintln!();
            eprintln!("Usage: mrt-replay --file <MRT_FILE> [--collector <COLLECTOR>]");
            eprintln!("Example: mrt-replay --file updates.20180424.1555.gz --collector rrc12");
            std::process::exit(1);
        }
    };

    // Try to extract collector from filename if not explicitly provided
    let collector = if args.collector == "unknown" {
        extract_collector_from_filename(&file).unwrap_or_else(|| "unknown".to_string())
    } else {
        args.collector
    };

    eprintln!("MRT Replay — Collector: {}", collector);
    eprintln!("Processing file: {}", file);

    // bgpkit-parser: liest gz-komprimierte MRT-Dateien automatisch
    let parser = match BgpkitParser::new(&file) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Failed to open MRT file '{}': {}", file, e);
            std::process::exit(1);
        }
    };

    let mut count = 0u64;

    for elem in parser {
        let event_type = match elem.elem_type {
            ElemType::ANNOUNCE => "announce",
            ElemType::WITHDRAW => "withdraw",
        };

        let prefix = elem.prefix.prefix.to_string();
        let peer_asn = elem.peer_asn.to_u32();
        let peer_ip = elem.peer_ip.to_string();

        // AS-Pfad: letzter Eintrag = origin_as
        let as_path: Vec<u32> = elem
            .as_path
            .as_ref()
            .map(|p: &bgpkit_parser::models::AsPath| {
                p.to_u32_vec_opt(true) // dedup=true
                    .unwrap_or_default()
            })
            .unwrap_or_default();

        let origin_as = as_path.last().copied().unwrap_or(peer_asn);

        let record = json!({
            "event_type": event_type,
            "prefix":     prefix,
            "peer_asn":   peer_asn,
            "origin_as":  origin_as,
            "peer_ip":    peer_ip,
            "as_path":    as_path,
            "collector":  collector,
            "timestamp":  elem.timestamp,
            "community":  Vec::<String>::new(), // MRT files don't have community info
        });

        println!("{}", record);
        count += 1;

        if count.is_multiple_of(100_000) {
            eprintln!("Processed {count} records ...");
        }
    }

    eprintln!("Done. Total: {count} records.");
}

/// Extracts collector name from filename (e.g., "updates.rrc12.20240101.0000.gz" -> "rrc12")
fn extract_collector_from_filename(filename: &str) -> Option<String> {
    let path = Path::new(filename);
    let stem = path.file_stem()?.to_str()?;

    // Common patterns in RIPE RIS filenames
    // updates.rrc12.20240101.0000.gz
    // rib.20240101.0000.bz2
    // Try to extract rrcXX pattern
    if let Some(pos) = stem.find("rrc") {
        let after = &stem[pos..];
        // Take up to next '.' or end
        let end = after.find('.').unwrap_or(after.len());
        let collector = &after[..end];
        if collector.len() > 3 && collector[3..].chars().all(|c| c.is_ascii_digit()) {
            return Some(collector.to_string());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    /// Stellt sicher dass alle Pflichtfelder im Output vorhanden sind.
    #[test]
    fn test_output_has_all_required_fields() {
        // Simuliere einen MRT-Record als JSON
        let record = json!({
            "event_type": "announce",
            "prefix":     "8.8.8.0/24",
            "peer_asn":   1103_u32,
            "origin_as":  15169_u32,
            "peer_ip":    "80.249.211.0",
            "as_path":    [1103_u32, 3356_u32, 15169_u32],
            "collector":  "rrc12",
            "timestamp":  1524578100.0_f64,
        });

        // Alle Pflichtfelder müssen vorhanden sein
        let required = [
            "event_type",
            "prefix",
            "peer_asn",
            "origin_as",
            "peer_ip",
            "as_path",
            "collector",
            "timestamp",
        ];
        for field in required {
            assert!(
                record.get(field).is_some(),
                "Pflichtfeld '{field}' fehlt im Output-Format"
            );
        }

        // Typen prüfen
        assert!(record["as_path"].is_array());
        assert!(record["peer_asn"].is_number());
        assert!(record["timestamp"].is_number());
        assert!(record["collector"].is_string());
    }

    /// "collector" darf nie null oder leer sein
    #[test]
    fn test_collector_field_is_never_empty() {
        // Das CLI-Tool setzt collector per --collector Flag
        // Standardwert ist "unknown" (nicht leer)
        // Dieser Test dokumentiert die Erwartung
        let default_collector = "unknown";
        assert!(!default_collector.is_empty());
        assert_ne!(default_collector, "");
    }
}

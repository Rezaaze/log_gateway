//! Diagnostic: was a given origin_as visible to RIPE RIS collectors at all
//! in a directory of MRT archive files, for ANY prefix?
//!
//! Exists because a baseline can have zero "reliable" entries (see
//! `BaselineBuilder`'s `min_samples` threshold) for a prefix/aggregate
//! even when the underlying AS was clearly visible in the raw data — the
//! two failure modes look identical from the outside (score always 0.0)
//! but need different fixes. This distinguishes them directly: if this
//! tool finds plenty of raw ANNOUNCE records but `baseline-builder`
//! still produced no reliable entry, the cause is sample-count filtering
//! (`--min-samples`), not missing visibility. Used to root-cause exactly
//! that for the real Pakistan Telecom/YouTube 2008 backtest — see
//! TRUSTWAVE_ROADMAP.md Abschnitt 3.2: AS36561 had 971 real ANNOUNCE
//! records across 11 collectors in the baseline week (including its real
//! 208.65.152.0/22), but too few *co-occurring, same-time-slot* samples
//! to clear the default `min_samples=30` reliability bar.
//!
//! ## Usage
//! ```bash
//! cargo run -p baseline-builder --example check_origin_visibility -- \
//!   --data-dir /path/to/mrt --origin-as 36561
//! ```
use bgpkit_parser::{models::ElemType, BgpkitParser};
use clap::Parser;
use rayon::prelude::*;
use std::collections::HashSet;
use std::path::PathBuf;
use walkdir::WalkDir;

#[derive(Parser, Debug)]
struct Args {
    /// Directory of MRT archive files to scan (data/mrt/rrcXX/... layout)
    #[arg(long)]
    data_dir: PathBuf,

    /// The origin AS to check visibility for
    #[arg(long)]
    origin_as: u32,
}

fn main() {
    let args = Args::parse();

    let files: Vec<PathBuf> = WalkDir::new(&args.data_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("gz"))
        .collect();
    eprintln!(
        "Scanning {} files for origin_as={}...",
        files.len(),
        args.origin_as
    );

    let results: Vec<(String, String)> = files
        .par_iter()
        .flat_map(|path| {
            let mut out = Vec::new();
            let parser = match BgpkitParser::new(path.to_str().unwrap_or("")) {
                Ok(p) => p,
                Err(_) => return out,
            };
            for elem in parser {
                if elem.elem_type != ElemType::ANNOUNCE {
                    continue;
                }
                let as_path: Vec<u32> = elem
                    .as_path
                    .as_ref()
                    .map(|p| p.to_u32_vec_opt(true).unwrap_or_default())
                    .unwrap_or_default();
                if as_path.last() == Some(&args.origin_as) {
                    let collector = path
                        .ancestors()
                        .nth(2)
                        .and_then(|p| p.file_name())
                        .and_then(|n| n.to_str())
                        .unwrap_or("unknown")
                        .to_string();
                    out.push((elem.prefix.prefix.to_string(), collector));
                }
            }
            out
        })
        .collect();

    let distinct_prefixes: HashSet<&String> = results.iter().map(|(p, _)| p).collect();
    let distinct_collectors: HashSet<&String> = results.iter().map(|(_, c)| c).collect();
    eprintln!(
        "Total ANNOUNCE records with origin_as={}: {}",
        args.origin_as,
        results.len()
    );
    eprintln!("Distinct prefixes: {}", distinct_prefixes.len());
    eprintln!(
        "Distinct collectors that saw this AS: {}",
        distinct_collectors.len()
    );
    let mut prefixes: Vec<&&String> = distinct_prefixes.iter().collect();
    prefixes.sort();
    for p in prefixes.iter().take(30) {
        eprintln!("  {}", p);
    }
}

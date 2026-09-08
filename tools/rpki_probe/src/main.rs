//! # rpki-probe
//!
//! Misst die reale RPKI-Alert-Rate: verbindet einen VRP-Feed mit dem
//! RIS-Live-Stream und validiert jede beobachtete Ankündigung.
//!
//! Beantwortet die Frage, die vor jedem Kundengespräch steht: **wie viele
//! Alerts produziert das System pro Minute, und wie viele davon sind
//! chronische Fehlkonfigurationen statt Ereignisse?**
//!
//! Braucht keine Infrastruktur — kein NATS, kein ClickHouse, kein eigener
//! Validator. Läuft auf einem Laptop.
//!
//! ```bash
//! cargo run --release -p rpki-probe -- --seconds 300
//! ```

use anyhow::{Context, Result};
use clap::Parser;
use futures_util::StreamExt;
use log_gateway::rpki_cache::{RpkiCache, RpkiStatus};
use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
#[command(about = "Misst die RPKI-Alert-Rate auf dem Live-BGP-Stream")]
struct Args {
    /// Messdauer in Sekunden
    #[arg(long, default_value_t = 180)]
    seconds: u64,

    /// VRP-Feed: volle URL eines öffentlichen Feeds (endet auf .json) oder
    /// Basis-Adresse eines eigenen Validators
    #[arg(long, default_value = "https://rpki.cloudflare.com/rpki.json")]
    vrp_url: String,

    /// RIS-Live-Stream
    #[arg(
        long,
        default_value = "https://ris-live.ripe.net/v1/stream/?format=json&client=rpki-probe"
    )]
    stream_url: String,

    /// Nur Ankündigungen dieser Präfixe zählen (mehrfach angebbar). Ohne
    /// Angabe wird der komplette globale Stream ausgewertet — nützlich für
    /// eine Gesamtschau, aber nicht repräsentativ für einen einzelnen Kunden,
    /// der nur seine eigenen Präfixe überwacht.
    #[arg(long)]
    prefix: Vec<String>,
}

#[derive(Default)]
struct Counts {
    total: u64,
    valid: u64,
    not_found: u64,
    invalid_asn: u64,
    invalid_length: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let cache = RpkiCache::new(args.vrp_url.clone());
    cache.start_refresh_loop().await;
    eprint!("Lade VRP-Feed ({}) ", args.vrp_url);
    let mut loaded = false;
    for _ in 0..180 {
        if cache.validate("1.1.1.0/24", 13335) != RpkiStatus::Unavailable {
            loaded = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        eprint!(".");
    }
    if !loaded {
        anyhow::bail!("VRP-Feed konnte nicht geladen werden");
    }
    eprintln!(" ok");
    eprintln!("Messe {} s Live-Stream ...", args.seconds);

    let response = reqwest::Client::new()
        .get(&args.stream_url)
        .send()
        .await
        .context("RIS-Live-Stream nicht erreichbar")?;

    let mut counts = Counts::default();
    let mut invalid_routes: HashMap<(String, u32), u64> = HashMap::new();
    let deadline = Instant::now() + Duration::from_secs(args.seconds);
    let mut stream = response.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();

    'outer: while let Some(chunk) = stream.next().await {
        buf.extend_from_slice(&chunk.context("Stream abgebrochen")?);
        while let Some(nl) = buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buf.drain(..=nl).collect();
            if Instant::now() > deadline {
                break 'outer;
            }
            process_line(
                &line,
                &cache,
                &args.prefix,
                &mut counts,
                &mut invalid_routes,
            );
        }
        if Instant::now() > deadline {
            break;
        }
    }

    report(&args, &counts, invalid_routes);
    Ok(())
}

fn process_line(
    line: &[u8],
    cache: &RpkiCache,
    prefix_filter: &[String],
    counts: &mut Counts,
    invalid_routes: &mut HashMap<(String, u32), u64>,
) {
    let Ok(msg) = serde_json::from_slice::<serde_json::Value>(line) else {
        return;
    };
    let data = &msg["data"];
    if data["type"] != "UPDATE" {
        return;
    }
    let Some(path) = data["path"].as_array() else {
        return;
    };
    // Das Origin-AS ist der letzte Eintrag im AS-Pfad. AS-Sets (verschachtelte
    // Arrays) am Pfadende werden übersprungen — dort ist das Origin nicht
    // eindeutig bestimmbar.
    let Some(origin) = path.last().and_then(|v| v.as_u64()) else {
        return;
    };
    let origin = origin as u32;

    let empty = Vec::new();
    for ann in data["announcements"].as_array().unwrap_or(&empty) {
        for pfx in ann["prefixes"].as_array().unwrap_or(&empty) {
            let Some(pfx) = pfx.as_str() else { continue };
            if !prefix_filter.is_empty() && !prefix_filter.iter().any(|p| p == pfx) {
                continue;
            }
            counts.total += 1;
            match cache.validate(pfx, origin) {
                RpkiStatus::Valid => counts.valid += 1,
                RpkiStatus::NotFound => counts.not_found += 1,
                RpkiStatus::InvalidAsn => {
                    counts.invalid_asn += 1;
                    *invalid_routes.entry((pfx.to_string(), origin)).or_default() += 1;
                }
                RpkiStatus::InvalidLength => {
                    counts.invalid_length += 1;
                    *invalid_routes.entry((pfx.to_string(), origin)).or_default() += 1;
                }
                RpkiStatus::Unavailable => {}
            }
        }
    }
}

fn report(args: &Args, c: &Counts, invalid_routes: HashMap<(String, u32), u64>) {
    let pct = |n: u64| {
        if c.total > 0 {
            100.0 * n as f64 / c.total as f64
        } else {
            0.0
        }
    };
    let invalid = c.invalid_asn + c.invalid_length;
    let per_min = |n: f64| n * 60.0 / args.seconds as f64;

    println!("\n=== {} s Live-BGP gegen RPKI ===", args.seconds);
    if !args.prefix.is_empty() {
        println!("Gefiltert auf {} Präfix(e)", args.prefix.len());
    }
    println!("Ankündigungen gesamt : {}", c.total);
    println!(
        "  Valid              : {:>9}  ({:.1} %)",
        c.valid,
        pct(c.valid)
    );
    println!(
        "  NotFound (keine ROA): {:>8}  ({:.1} %)",
        c.not_found,
        pct(c.not_found)
    );
    println!(
        "  InvalidAsn         : {:>9}  ({:.2} %)",
        c.invalid_asn,
        pct(c.invalid_asn)
    );
    println!(
        "  InvalidLength      : {:>9}  ({:.2} %)",
        c.invalid_length,
        pct(c.invalid_length)
    );

    println!(
        "\nRohes Alert-Volumen  : {:.1} invalide Ankündigungen/Minute",
        per_min(invalid as f64)
    );
    println!(
        "Betroffene Routen    : {} eindeutige (Präfix, Origin)-Paare",
        invalid_routes.len()
    );
    println!(
        "Nach Dedup pro Route : {:.1} Alerts/Minute",
        per_min(invalid_routes.len() as f64)
    );
    println!(
        "\nHinweis: Dieselben Routen wiederholen sich — die meisten sind chronisch\n\
         invalide Fehlkonfigurationen, keine Ereignisse. Der Produktwert liegt in der\n\
         Änderung (\"war valid, ist es seit 14:03 nicht mehr\"), nicht im Zustand."
    );

    let mut top: Vec<_> = invalid_routes.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1));
    if !top.is_empty() {
        println!("\nHäufigste invalide Routen:");
        for ((pfx, asn), n) in top.iter().take(10) {
            println!("  {:<22} AS{:<8} {}x", pfx, asn, n);
        }
    }
}

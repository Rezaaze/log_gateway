//! # asn-report
//!
//! Erstellt für ein beliebiges autonomes System einen RPKI-Statusbericht aus
//! **öffentlichen Daten** — angekündigte Präfixe von RIPEstat, ROA-Daten aus
//! einem VRP-Feed. Kein Zugang zum fremden Netz nötig, kein Server, keine
//! Anmeldung.
//!
//! ```bash
//! # Einzelbericht, lesbar im Terminal
//! cargo run --release -p asn-report -- AS3320
//!
//! # Als Markdown zum Verschicken
//! cargo run --release -p asn-report -- AS3320 --format markdown --out berichte/
//!
//! # Viele ASNs durchgehen, nach Handlungsbedarf sortiert
//! cargo run --release -p asn-report -- --file asns.txt --survey
//! ```
//!
//! ## Zum Ton der Berichte
//!
//! Die Ausgabe ist bewusst nüchtern. Ein fehlendes ROA ist kein Vorfall und
//! keine Sicherheitslücke, sondern eine nicht getroffene Vorsorge — und ein
//! Bericht, der das dramatisiert, verliert genau bei den Lesern an
//! Glaubwürdigkeit, die etwas davon verstehen. Jeder Bericht nennt seine
//! Quellen, damit der Empfänger jede Aussage selbst nachprüfen kann.

use anyhow::{Context, Result};
use clap::Parser;
use log_gateway::rpki_cache::{RpkiCache, RpkiStatus};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

const VRP_DEFAULT: &str = "https://rpki.cloudflare.com/rpki.json";

#[derive(Parser, Debug)]
#[command(about = "RPKI-Statusbericht für ein AS aus öffentlichen Daten")]
struct Args {
    /// Zu prüfende ASNs, z. B. AS3320 oder 3320
    #[arg(value_name = "ASN")]
    asns: Vec<String>,

    /// Datei mit einer ASN pro Zeile (ergänzt die Positionsargumente)
    #[arg(long)]
    file: Option<PathBuf>,

    /// Ausgabeformat
    #[arg(long, value_enum, default_value_t = Format::Text)]
    format: Format,

    /// Verzeichnis für die Berichte; ohne Angabe Ausgabe auf stdout
    #[arg(long)]
    out: Option<PathBuf>,

    /// Statt Einzelberichten eine nach Handlungsbedarf sortierte Übersicht
    #[arg(long)]
    survey: bool,

    /// VRP-Feed: volle URL (endet auf .json) oder Basis-Adresse eines eigenen
    /// Validators
    #[arg(long, default_value = VRP_DEFAULT)]
    vrp_url: String,

    /// Pause zwischen RIPEstat-Abfragen in Millisekunden. RIPEstat ist ein
    /// kostenloser Dienst der RIPE NCC — nicht ohne Not hochdrehen.
    #[arg(long, default_value_t = 300)]
    delay_ms: u64,
}

#[derive(clap::ValueEnum, Clone, Debug, PartialEq)]
enum Format {
    Text,
    Markdown,
}

/// Ein angekündigtes Präfix mit seinem RPKI-Befund.
struct PrefixFinding {
    prefix: String,
    status: RpkiStatus,
    /// Für invalide Präfixe: was die deckende ROA tatsächlich erlaubt.
    reason: Option<String>,
}

struct AsnReport {
    asn: u32,
    announced: usize,
    valid: usize,
    not_found: Vec<String>,
    invalid: Vec<PrefixFinding>,
}

impl AsnReport {
    fn coverage_pct(&self) -> f64 {
        if self.announced == 0 {
            return 0.0;
        }
        100.0 * self.valid as f64 / self.announced as f64
    }

    /// Ob der Bericht überhaupt etwas zu berichten hat.
    fn has_findings(&self) -> bool {
        !self.not_found.is_empty() || !self.invalid.is_empty()
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let mut asns: Vec<u32> = Vec::new();
    for raw in &args.asns {
        asns.push(parse_asn(raw)?);
    }
    if let Some(path) = &args.file {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("ASN-Liste nicht lesbar: {}", path.display()))?;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            asns.push(parse_asn(line)?);
        }
    }
    if asns.is_empty() {
        anyhow::bail!("Keine ASN angegeben — als Argument oder über --file");
    }

    let cache = load_vrps(&args.vrp_url).await?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .user_agent("asn-report (RPKI-Statusbericht)")
        .build()?;

    if let Some(dir) = &args.out {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("Ausgabeverzeichnis nicht anlegbar: {}", dir.display()))?;
    }

    let mut reports = Vec::new();
    for (i, asn) in asns.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(Duration::from_millis(args.delay_ms)).await;
        }
        match build_report(&client, &cache, *asn).await {
            Ok(report) => reports.push(report),
            Err(e) => eprintln!("AS{asn}: übersprungen — {e}"),
        }
    }

    if args.survey {
        print_survey(&reports);
        return Ok(());
    }

    for report in &reports {
        let text = match args.format {
            Format::Text => render_text(report),
            Format::Markdown => render_markdown(report),
        };
        match &args.out {
            Some(dir) => {
                let ext = if args.format == Format::Markdown {
                    "md"
                } else {
                    "txt"
                };
                let path = dir.join(format!("AS{}.{ext}", report.asn));
                std::fs::write(&path, &text)
                    .with_context(|| format!("Bericht nicht schreibbar: {}", path.display()))?;
                println!("geschrieben: {}", path.display());
            }
            None => println!("{text}"),
        }
    }
    Ok(())
}

fn parse_asn(raw: &str) -> Result<u32> {
    let trimmed = raw.trim().trim_start_matches("AS").trim_start_matches("as");
    trimmed
        .parse()
        .with_context(|| format!("keine gültige ASN: {raw}"))
}

async fn load_vrps(url: &str) -> Result<RpkiCache> {
    let cache = RpkiCache::new(url.to_string());
    cache.start_refresh_loop().await;
    eprint!("Lade ROA-Daten ({url}) ");
    for _ in 0..180 {
        // 1.1.1.0/24 aus AS13335 ist eine der stabilsten ROAs überhaupt und
        // dient hier nur als Anzeiger, dass der Index befüllt ist.
        if cache.validate("1.1.1.0/24", 13335) != RpkiStatus::Unavailable {
            eprintln!(" ok");
            return Ok(cache);
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        eprint!(".");
    }
    anyhow::bail!("VRP-Feed konnte nicht geladen werden: {url}")
}

async fn build_report(client: &reqwest::Client, cache: &RpkiCache, asn: u32) -> Result<AsnReport> {
    let url = format!(
        "https://stat.ripe.net/data/announced-prefixes/data.json?resource=AS{asn}&sourceapp=asn-report"
    );
    let response = client
        .get(&url)
        .send()
        .await
        .context("RIPEstat nicht erreichbar")?;
    if !response.status().is_success() {
        anyhow::bail!("RIPEstat antwortete mit {}", response.status());
    }
    let body: serde_json::Value = response.json().await.context("RIPEstat-Antwort unlesbar")?;
    let empty = Vec::new();
    let prefixes = body["data"]["prefixes"].as_array().unwrap_or(&empty);

    let mut report = AsnReport {
        asn,
        announced: 0,
        valid: 0,
        not_found: Vec::new(),
        invalid: Vec::new(),
    };

    for entry in prefixes {
        let Some(prefix) = entry["prefix"].as_str() else {
            continue;
        };
        report.announced += 1;
        let status = cache.validate(prefix, asn);
        match status {
            RpkiStatus::Valid => report.valid += 1,
            RpkiStatus::NotFound => report.not_found.push(prefix.to_string()),
            RpkiStatus::InvalidAsn | RpkiStatus::InvalidLength => {
                let reason = explain_invalid(cache, prefix, asn, &status);
                report.invalid.push(PrefixFinding {
                    prefix: prefix.to_string(),
                    status,
                    reason,
                });
            }
            // Sollte nach load_vrps() nicht vorkommen; als NotFound zu zählen
            // wäre eine stille Falschaussage, deshalb ausgewiesen.
            RpkiStatus::Unavailable => {
                report.invalid.push(PrefixFinding {
                    prefix: prefix.to_string(),
                    status: RpkiStatus::Unavailable,
                    reason: Some("ROA-Daten waren während der Prüfung nicht verfügbar".into()),
                });
            }
        }
    }
    Ok(report)
}

/// Formuliert aus den deckenden ROAs, warum ein Präfix invalid ist.
fn explain_invalid(
    cache: &RpkiCache,
    prefix: &str,
    asn: u32,
    status: &RpkiStatus,
) -> Option<String> {
    let covering = cache.covering_vrps(prefix);
    if covering.is_empty() {
        return None;
    }
    match status {
        RpkiStatus::InvalidAsn => {
            let mut by_asn: BTreeMap<u32, u8> = BTreeMap::new();
            for (_len, max_len, vrp_asn) in &covering {
                by_asn.entry(*vrp_asn).or_insert(*max_len);
            }
            let asns: Vec<String> = by_asn.keys().map(|a| format!("AS{a}")).collect();
            Some(format!(
                "die deckende ROA berechtigt {} — nicht AS{asn}",
                asns.join(", ")
            ))
        }
        RpkiStatus::InvalidLength => {
            let max_len = covering
                .iter()
                .filter(|(_, _, vrp_asn)| *vrp_asn == asn)
                .map(|(_, max_len, _)| *max_len)
                .max();
            max_len.map(|m| {
                format!("die ROA für AS{asn} erlaubt höchstens /{m}, angekündigt wird länger")
            })
        }
        _ => None,
    }
}

fn status_label(status: &RpkiStatus) -> &'static str {
    match status {
        RpkiStatus::InvalidAsn => "falsches Origin-AS",
        RpkiStatus::InvalidLength => "Präfix länger als erlaubt",
        RpkiStatus::Unavailable => "nicht prüfbar",
        RpkiStatus::Valid => "gedeckt",
        RpkiStatus::NotFound => "keine ROA",
    }
}

fn render_text(r: &AsnReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("\n=== RPKI-Statusbericht AS{} ===\n", r.asn));
    if r.announced == 0 {
        out.push_str("  Dieses AS kündigt derzeit keine Präfixe an (laut RIPEstat).\n");
        return out;
    }
    out.push_str(&format!("  angekündigte Präfixe : {}\n", r.announced));
    out.push_str(&format!(
        "  durch ROA gedeckt    : {:>5}  ({:.1} %)\n",
        r.valid,
        r.coverage_pct()
    ));
    out.push_str(&format!(
        "  ohne ROA             : {:>5}\n",
        r.not_found.len()
    ));
    out.push_str(&format!(
        "  RPKI-invalid         : {:>5}\n",
        r.invalid.len()
    ));
    if !r.invalid.is_empty() {
        out.push_str("\n  Invalide Präfixe:\n");
        for f in &r.invalid {
            out.push_str(&format!("    {} — {}", f.prefix, status_label(&f.status)));
            if let Some(reason) = &f.reason {
                out.push_str(&format!(" ({reason})"));
            }
            out.push('\n');
        }
    }
    if !r.not_found.is_empty() {
        out.push_str(&format!("\n  Präfixe ohne ROA ({}):\n", r.not_found.len()));
        for p in &r.not_found {
            out.push_str(&format!("    {p}\n"));
        }
    }
    out
}

fn render_markdown(r: &AsnReport) -> String {
    let today = chrono::Utc::now().format("%d.%m.%Y");
    let mut out = String::new();
    out.push_str(&format!("# RPKI-Statusbericht — AS{}\n\n", r.asn));
    out.push_str(&format!("Stand: {today}\n\n"));

    if r.announced == 0 {
        out.push_str(
            "Für dieses AS sind derzeit keine angekündigten Präfixe sichtbar. \
             Damit lässt sich von außen nichts über den RPKI-Status sagen.\n",
        );
        return out;
    }

    out.push_str("## Zusammenfassung\n\n");
    out.push_str(&format!(
        "{} angekündigte Präfixe, davon {} durch eine ROA gedeckt ({:.0} %), \
         {} ohne ROA, {} RPKI-invalid.\n\n",
        r.announced,
        r.valid,
        r.coverage_pct(),
        r.not_found.len(),
        r.invalid.len()
    ));

    if !r.has_findings() {
        out.push_str(
            "Alle angekündigten Präfixe sind durch eine gültige ROA gedeckt. \
             Aus RPKI-Sicht gibt es hier nichts zu tun.\n\n",
        );
    }

    if !r.invalid.is_empty() {
        out.push_str("## RPKI-invalide Präfixe\n\n");
        out.push_str(
            "Diese Präfixe werden angekündigt, die zugehörige ROA deckt die \
             Ankündigung aber nicht. Netze, die RPKI-Filterung einsetzen, \
             verwerfen solche Routen — das kann Erreichbarkeit kosten, ohne dass \
             es im eigenen Monitoring auffällt.\n\n",
        );
        out.push_str("| Präfix | Befund |\n|---|---|\n");
        for f in &r.invalid {
            let reason = f
                .reason
                .clone()
                .unwrap_or_else(|| status_label(&f.status).to_string());
            out.push_str(&format!("| `{}` | {} |\n", f.prefix, reason));
        }
        out.push('\n');
    }

    if !r.not_found.is_empty() {
        out.push_str(&format!("## Präfixe ohne ROA ({})\n\n", r.not_found.len()));
        out.push_str(
            "Für diese Präfixe existiert keine ROA. Das ist kein Fehler und kein \
             Vorfall — die Ankündigung wird überall akzeptiert. Es bedeutet nur, \
             dass eine fremde Ankündigung derselben Adressen sich nicht \
             automatisch als unberechtigt erkennen lässt.\n\n",
        );
        for p in &r.not_found {
            out.push_str(&format!("- `{p}`\n"));
        }
        out.push('\n');
    }

    out.push_str("## Datenquellen und Überprüfung\n\n");
    out.push_str(&format!(
        "Dieser Bericht beruht ausschließlich auf öffentlichen Daten. \
         Jede Angabe ist unabhängig nachprüfbar:\n\n\
         - Angekündigte Präfixe: RIPEstat, \
         <https://stat.ripe.net/data/announced-prefixes/data.json?resource=AS{}>\n\
         - ROA-Daten: öffentlicher VRP-Feed unter <{}>\n\
         - Einzelne Präfixe prüfbar über RIPEstat RPKI Validation oder den \
         eigenen Validator\n\n\
         Die Daten sind eine Momentaufnahme; BGP-Ankündigungen und ROAs ändern \
         sich laufend.\n",
        r.asn, VRP_DEFAULT
    ));
    out
}

fn print_survey(reports: &[AsnReport]) {
    let mut with_findings: Vec<&AsnReport> = reports.iter().filter(|r| r.has_findings()).collect();
    // Nach Anzahl invalider Präfixe, dann nach fehlenden ROAs sortieren —
    // invalide Routen sind ein bestehendes Problem, fehlende ROAs eine
    // unterlassene Vorsorge.
    with_findings.sort_by(|a, b| {
        b.invalid
            .len()
            .cmp(&a.invalid.len())
            .then(b.not_found.len().cmp(&a.not_found.len()))
    });

    let aktiv = reports.iter().filter(|r| r.announced > 0).count();
    let inaktiv = reports.len() - aktiv;
    println!("\n=== Übersicht: {} ASNs geprüft ===", reports.len());
    println!("  ohne angekündigte Präfixe : {inaktiv}");
    println!("  aktiv                     : {aktiv}");
    println!("  davon mit Befund          : {}", with_findings.len());
    if aktiv > 0 {
        println!(
            "                              ({:.1} % der aktiven)",
            100.0 * with_findings.len() as f64 / aktiv as f64
        );
    }
    if with_findings.is_empty() {
        return;
    }
    println!(
        "\n  {:<10} {:>9} {:>7} {:>9} {:>8}",
        "ASN", "Präfixe", "gedeckt", "ohne ROA", "invalid"
    );
    for r in &with_findings {
        println!(
            "  AS{:<8} {:>9} {:>6.0} % {:>9} {:>8}",
            r.asn,
            r.announced,
            r.coverage_pct(),
            r.not_found.len(),
            r.invalid.len()
        );
    }
}

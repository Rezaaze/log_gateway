# Backtest cases

Case definitions for `tools/backtest` — TRUSTWAVE_ROADMAP.md Abschnitt 3.2.1.
Each `.toml` describes one known historical BGP hijack: prefix, legitimate
and hijacker origin AS, hijack window, and where to find the MRT archive
data and pre-incident baseline. See `tools/backtest/src/main.rs`'s module
doc comment for the full field reference and why this data isn't hardcoded
in the tool itself.

## Status

| Case | Prefix/ASN/timestamps | MRT data | Baseline | Real backtest result |
|---|---|---|---|---|
| `pakistan-telecom-youtube-2008.toml` | ✅ verified (RIPE NCC's own case study — same data source this project reads) | ✅ downloaded once, real run (01.09.2026) — not kept in repo/committed (~1.8GB), re-download from `data.ris.ripe.net` to reproduce | ✅ built once (60,127 entries, min_samples=30) — not kept in repo | ✅ real run done — see `TRUSTWAVE_ROADMAP.md` Abschnitt 3.2 for full numbers. Wave-physics: TPR 0.0%, FPR 33.3% (structural baseline-lookup gap, not calibration). Simple `HijackDetector` layer: did fire, with a documented caveat (cold-start on a never-before-seen sub-prefix) |
| `myetherwallet-route53-2018.toml` | ✅ cross-verified (3 independent sources) — see the file's header note on the sub-prefix nuance | ❌ not downloaded | ❌ not built | not yet run |
| Rostelecom, 1 April 2020 | ⚠️ **not usable yet** — see below | — | — | — |

## Rostelecom, 1 April 2020 — why there's no case file yet

Every secondary source found (ThousandEyes, SecurityWeek, Security Affairs,
dig.watch, MANRS' own summary post) traces back to the same underlying
report and agrees on:
- Hijacker: AS12389 (Rostelecom)
- Start: ~19:30 UTC
- Cloudflare's normally-announced covering prefix: 104.16.48.0/20 (AS13335)
- Rostelecom announced a more-specific `/21` within it

But none of them state the **exact `/21` CIDR** that was hijacked, and the
**end timestamp is inconsistent across sources** (variously "12:35 PM",
which doesn't parse as being after a 19:30 UTC start on the same page).
MANRS' original detailed writeup (https://manrs.org/2020/04/not-just-another-bgp-hijack/)
blocks automated fetching (403) and needs to be read directly, or check
Qrator Labs' original incident report, to get the precise prefix and
timestamps before this case is usable — filling in a guessed `/21` or
end-time would silently corrupt any backtest run against it.

## Filling in `mrt_data_dir` and `baseline_path`

1. Download MRT archive files for the incident window (± a few hours,
   more collectors = more reliable detection) from
   `https://data.ris.ripe.net/rrcXX/YYYY.MM/updates.YYYYMMDD.HHMM.gz`,
   laid out as `data/mrt/rrcXX/YYYY.MM/...` (see `tools/baseline_builder`'s
   doc comment for the exact layout).
2. Separately download **weeks** of MRT data ending strictly before the
   hijack start, for the same prefix/collectors, and run
   `baseline-builder --data-dir <that dir> --output <baseline_path>` to
   build a baseline with enough samples (>=30) for the target prefix to be
   "reliable" — this is the actually large download, not the ±2h hijack
   window itself.
3. Run `backtest --case tools/backtest/cases/<name>.toml`.

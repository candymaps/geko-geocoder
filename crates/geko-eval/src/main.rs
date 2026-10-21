//! Reference queries and engine comparison (PROJECT.md §13).

mod boxes;
mod buildreport;
mod data;
mod fuzz;
mod golden;
mod load;
mod run;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::golden::Case;
use crate::run::Report;

#[derive(Parser)]
#[command(name = "geko-eval", about = "Reference queries and engine comparison")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Generate reference queries from a canonical dataset.
    Generate {
        #[arg(long)]
        canonical: PathBuf,
        #[arg(long, default_value_t = 100)]
        per_category: usize,
        #[arg(long, default_value_t = 42)]
        seed: u64,
        /// Only entities this source also has (fair engine comparison against an engine fed
        /// with that source alone).
        #[arg(long)]
        only_source: Option<String>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Run reference queries against a Pelias-compatible API and save the report.
    Run {
        #[arg(long)]
        name: String,
        /// Base URL, e.g. http://127.0.0.1:8080 or https://host/pelias.
        #[arg(long)]
        url: String,
        #[arg(long = "cases", required = true)]
        cases: Vec<PathBuf>,
        /// Pause between requests, out of courtesy to shared services.
        #[arg(long, default_value_t = 0)]
        delay_ms: u64,
        #[arg(long, default_value_t = 5)]
        size: usize,
        /// The API spoken: Pelias's, or Photon's (`--url` is then the base of `/api`).
        #[arg(long, value_enum, default_value_t = run::Api::Pelias)]
        api: run::Api,
        #[arg(long)]
        out: PathBuf,
    },
    /// Generate reference queries in English from a canonical dataset.
    English {
        #[arg(long)]
        canonical: PathBuf,
        /// The English words of the street types of the pack (`golden/english.toml`).
        #[arg(long)]
        words: PathBuf,
        #[arg(long, default_value_t = 100)]
        per_category: usize,
        #[arg(long, default_value_t = 42)]
        seed: u64,
        #[arg(long)]
        out: PathBuf,
    },
    /// Generate reverse queries from a canonical dataset: points next to real addresses.
    Reverse {
        #[arg(long)]
        canonical: PathBuf,
        #[arg(long, default_value_t = 100)]
        per_category: usize,
        #[arg(long, default_value_t = 42)]
        seed: u64,
        #[arg(long)]
        out: PathBuf,
    },
    /// Derive cases with the focus an app would send from the ambiguous ones of `cases`.
    Focus {
        #[arg(long)]
        canonical: PathBuf,
        #[arg(long = "cases", required = true)]
        cases: Vec<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Derive autocomplete requests from the queries of `cases`, one per keystroke.
    Keystrokes {
        #[arg(long = "cases", required = true)]
        cases: Vec<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Load test: throughput and latency percentiles of a route at each concurrency.
    Load {
        #[arg(long)]
        url: String,
        #[arg(long = "cases", required = true)]
        cases: Vec<PathBuf>,
        /// search, autocomplete or reverse.
        #[arg(long, default_value = "search")]
        route: load::Route,
        #[arg(long, value_delimiter = ',', default_value = "1,4,16")]
        concurrency: Vec<usize>,
        #[arg(long, default_value_t = 10)]
        seconds: u64,
        /// Save the results, for `check-load`.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Fail if any request fails (a reload of the bundle under load must drop none).
        #[arg(long)]
        no_errors: bool,
    },
    /// The same load on two servers, measured in turns at each concurrency, for `check-load` to
    /// compare them as measured in the same moments: the median of five slices of each.
    LoadAb {
        #[arg(long)]
        base_url: String,
        #[arg(long)]
        url: String,
        #[arg(long = "cases", required = true)]
        cases: Vec<PathBuf>,
        #[arg(long, default_value = "search")]
        route: load::Route,
        #[arg(long, value_delimiter = ',', default_value = "1,4,16")]
        concurrency: Vec<usize>,
        /// Seconds each server is measured at each concurrency, in all its slices.
        #[arg(long, default_value_t = 10)]
        seconds: u64,
        #[arg(long)]
        out_base: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Fail if the p99, the throughput or the errors at any concurrency of `report` are clearly
    /// worse than in `baseline`, measured on the same machine, or miss the objectives given.
    CheckLoad {
        #[arg(long)]
        baseline: Option<PathBuf>,
        #[arg(long)]
        report: PathBuf,
        /// Objective for the p99 with one request at a time, in milliseconds.
        #[arg(long)]
        max_p99_ms: Option<f64>,
        /// Objective for the throughput at some concurrency that keeps `max_p99_ms`.
        #[arg(long)]
        min_qps: Option<f64>,
    },
    /// Counts and integrity problems of a canonical dataset (its fusion report and §13.4).
    Data {
        #[arg(long)]
        canonical: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Fail if records or entities dropped, or duplicates or integrity problems grew, against
    /// the data report of an earlier build of the same data.
    CheckData {
        #[arg(long)]
        baseline: PathBuf,
        #[arg(long)]
        report: PathBuf,
    },
    /// Fail if a build stage got clearly slower or hungrier, or the bundle clearly larger, than
    /// in an earlier build of the same data on this machine (`build_report.json`).
    CheckBuild {
        #[arg(long)]
        baseline: PathBuf,
        #[arg(long)]
        report: PathBuf,
        /// Objective for the hot part of the bundle, in GB (PROJECT.md §1.1).
        #[arg(long)]
        max_hot_gb: Option<f64>,
        /// Do not compare the time of the stages: the builds did not run in the same moments.
        #[arg(long)]
        ignore_time: bool,
        /// Do not compare the peak memory of the stages: the builds ran on different machines,
        /// whose number of cores changes it.
        #[arg(long)]
        ignore_memory: bool,
    },
    /// Print saved reports side by side.
    Compare { reports: Vec<PathBuf> },
    /// Fail if any category of `report` is worse than in `baseline` (top-1, top-5, errors or
    /// duplicated results), or the calibration of `confidence`; lists every query that changed.
    Check {
        #[arg(long)]
        baseline: PathBuf,
        #[arg(long)]
        report: PathBuf,
        /// Allowed drop, in percentage points.
        #[arg(long, default_value_t = 0.0)]
        tolerance: f64,
        /// Do not compare latencies: the report comes from another machine than the baseline.
        #[arg(long)]
        ignore_latency: bool,
        /// Compare latencies with this report, measured on this machine, instead of `baseline`.
        #[arg(long)]
        latency_baseline: Option<PathBuf>,
        /// The total top-1 of `report` must be at least this many points above the baseline's:
        /// the objective against another engine (PROJECT.md §1.1).
        #[arg(long)]
        total_lead: Option<f64>,
        /// Compare hit rates only, as against another engine: its errors, duplicated results and
        /// calibration are its own.
        #[arg(long)]
        hits_only: bool,
    },
    /// Write `data/region-boxes.tsv` of a country pack: a box that holds each of its partitions,
    /// from the boundaries of a canonical dataset of that country.
    RegionBoxes {
        /// `<data>/canonical/<COUNTRY>`.
        #[arg(long)]
        canonical: PathBuf,
        /// The folder of the pack, e.g. countries/es.
        #[arg(long)]
        pack: PathBuf,
        /// Widening of each box, in degrees (0.01 is about 1 km).
        #[arg(long, default_value_t = 0.01)]
        margin: f64,
    },
    /// Random and mutated requests against a server; fails on any 5xx or dropped connection.
    Fuzz {
        #[arg(long)]
        url: String,
        /// Reference queries to mutate.
        #[arg(long = "cases")]
        cases: Vec<PathBuf>,
        #[arg(long, default_value_t = 20_000)]
        requests: usize,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        #[arg(long)]
        out: PathBuf,
    },
}

fn read_cases(paths: &[PathBuf]) -> Result<Vec<Case>> {
    let mut cases = Vec::new();
    for path in paths {
        let file = fs::File::open(path).with_context(|| path.display().to_string())?;
        for line in BufReader::new(file).lines() {
            let line = line?;
            if !line.trim().is_empty() && !line.starts_with("//") {
                cases.push(serde_json::from_str(&line).with_context(|| format!("{}: {line}", path.display()))?);
            }
        }
    }
    Ok(cases)
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Generate { canonical, per_category, seed, only_source, out } => {
            let cases = golden::generate(&canonical, per_category, seed, only_source.as_deref())?;
            let mut file = fs::File::create(&out)?;
            for c in &cases {
                writeln!(file, "{}", serde_json::to_string(c)?)?;
            }
            println!("{} cases written to {}", cases.len(), out.display());
        }
        Command::Run { name, url, cases, delay_ms, size, api, out } => {
            let cases = read_cases(&cases)?;
            let report = run::run(&name, &url, api, &cases, Duration::from_millis(delay_ms), size);
            fs::write(&out, serde_json::to_vec_pretty(&report)?)?;
            print!("{}", run::table(std::slice::from_ref(&report)));
        }
        Command::English { canonical, words, per_category, seed, out } => {
            let text = fs::read_to_string(&words).with_context(|| words.display().to_string())?;
            let words: golden::EnglishWords = toml::from_str(&text).with_context(|| words.display().to_string())?;
            let cases = golden::english(&canonical, &words, per_category, seed)?;
            let mut file = fs::File::create(&out)?;
            for c in &cases {
                writeln!(file, "{}", serde_json::to_string(c)?)?;
            }
            println!("{} cases written to {}", cases.len(), out.display());
        }
        Command::Reverse { canonical, per_category, seed, out } => {
            let cases = golden::reverse(&canonical, per_category, seed)?;
            let mut file = fs::File::create(&out)?;
            for c in &cases {
                writeln!(file, "{}", serde_json::to_string(c)?)?;
            }
            println!("{} cases written to {}", cases.len(), out.display());
        }
        Command::Focus { canonical, cases, out } => {
            let focused = golden::with_focus(&read_cases(&cases)?, &canonical)?;
            let mut file = fs::File::create(&out)?;
            for c in &focused {
                writeln!(file, "{}", serde_json::to_string(c)?)?;
            }
            println!("{} cases written to {}", focused.len(), out.display());
        }
        Command::Keystrokes { cases, out } => {
            let typed = golden::keystrokes(&read_cases(&cases)?);
            let mut file = fs::File::create(&out)?;
            for c in &typed {
                writeln!(file, "{}", serde_json::to_string(c)?)?;
            }
            println!("{} keystrokes written to {}", typed.len(), out.display());
        }
        Command::Load { url, cases, route, concurrency, seconds, out, no_errors } => {
            let cases = read_cases(&cases)?;
            anyhow::ensure!(!cases.is_empty(), "no cases");
            println!("{}", load::HEADER);
            let mut reports = Vec::new();
            for c in concurrency {
                let r = load::run(&url, &cases, route, c, Duration::from_secs(2), Duration::from_secs(seconds));
                println!("{}", r.row());
                reports.push(r);
            }
            if let Some(out) = out {
                fs::write(&out, serde_json::to_vec_pretty(&reports)?)?;
            }
            let errors: usize = reports.iter().map(|r| r.errors).sum();
            anyhow::ensure!(!no_errors || errors == 0, "{errors} failed requests");
        }
        Command::LoadAb { base_url, url, cases, route, concurrency, seconds, out_base, out } => {
            let cases = read_cases(&cases)?;
            anyhow::ensure!(!cases.is_empty(), "no cases");
            println!("{}", load::HEADER);
            let (mut bases, mut reports) = (Vec::new(), Vec::new());
            for c in concurrency {
                let (b, r) = load::run_ab(&base_url, &url, &cases, route, c, Duration::from_secs(2), Duration::from_secs(seconds));
                println!("{}  (base)\n{}", b.row(), r.row());
                bases.push(b);
                reports.push(r);
            }
            fs::write(&out_base, serde_json::to_vec_pretty(&bases)?)?;
            fs::write(&out, serde_json::to_vec_pretty(&reports)?)?;
        }
        Command::CheckLoad { baseline, report, max_p99_ms, min_qps } => {
            let read = |p: &PathBuf| -> Result<Vec<load::LoadReport>> { Ok(serde_json::from_slice(&fs::read(p).with_context(|| p.display().to_string())?)?) };
            let report = read(&report)?;
            println!("{}", load::HEADER);
            let mut worse = load::objectives(&report, max_p99_ms, min_qps);
            if let Some(baseline) = baseline {
                let baseline = read(&baseline)?;
                for (b, r) in baseline.iter().zip(&report) {
                    println!("{}  (baseline)\n{}", b.row(), r.row());
                }
                worse.extend(load::regressions(&baseline, &report));
            } else {
                for r in &report {
                    println!("{}", r.row());
                }
            }
            for w in &worse {
                println!("WORSE {w}");
            }
            anyhow::ensure!(worse.is_empty(), "{} regressions against the baseline", worse.len());
        }
        Command::Data { canonical, out } => {
            let report = data::report(&canonical)?;
            fs::write(&out, serde_json::to_vec_pretty(&report)?)?;
            for (key, n) in report.problems.iter().filter(|(_, n)| **n > 0) {
                println!("{key}: {n}");
            }
        }
        Command::CheckData { baseline, report } => {
            let read = |p: &PathBuf| -> Result<data::DataReport> { Ok(serde_json::from_slice(&fs::read(p).with_context(|| p.display().to_string())?)?) };
            let (baseline, report) = (read(&baseline)?, read(&report)?);
            print!("{}", data::table(&baseline, &report));
            let worse = data::regressions(&baseline, &report);
            for w in &worse {
                println!("WORSE {w}");
            }
            anyhow::ensure!(worse.is_empty(), "{} regressions against the baseline", worse.len());
        }
        Command::CheckBuild { baseline, report, max_hot_gb, ignore_time, ignore_memory } => {
            let read = |p: &PathBuf| -> Result<buildreport::BuildReport> {
                Ok(serde_json::from_slice(&fs::read(p).with_context(|| p.display().to_string())?)?)
            };
            let (baseline, report) = (read(&baseline)?, read(&report)?);
            print!("{}", buildreport::table(&baseline, &report));
            let compare = buildreport::Compare { time: !ignore_time, memory: !ignore_memory };
            let worse = buildreport::regressions(&baseline, &report, max_hot_gb.map(|gb| (gb * 1e9) as u64), compare);
            for w in &worse {
                println!("WORSE {w}");
            }
            anyhow::ensure!(worse.is_empty(), "{} regressions against the baseline", worse.len());
        }
        Command::Compare { reports } => {
            let reports: Vec<Report> = reports
                .iter()
                .map(|p| Ok(serde_json::from_slice(&fs::read(p).with_context(|| p.display().to_string())?)?))
                .collect::<Result<_>>()?;
            print!("{}", run::table(&reports));
        }
        Command::Check { baseline, report, tolerance, ignore_latency, latency_baseline, total_lead, hits_only } => {
            let read = |p: &PathBuf| -> Result<Report> { Ok(serde_json::from_slice(&fs::read(p).with_context(|| p.display().to_string())?)?) };
            let (baseline, report) = (read(&baseline)?, read(&report)?);
            print!("{}", run::table(&[baseline.clone(), report.clone()]));
            if let (Some(b), Some(r)) = (&baseline.calibration, &report.calibration) {
                println!("confidence of the first result: ECE {:.3} -> {:.3}, Brier {:.3} -> {:.3}", b.ece, r.ece, b.brier, r.brier);
            }
            let (lost, gained) = run::changes(&baseline, &report);
            if !lost.is_empty() || !gained.is_empty() {
                println!("{} queries lost and {} gained against the baseline:", lost.len(), gained.len());
            }
            const SHOWN: usize = 40;
            for l in lost.iter().take(SHOWN) {
                println!("  LOST   {l}");
            }
            for g in gained.iter().take(SHOWN) {
                println!("  GAINED {g}");
            }
            if lost.len().max(gained.len()) > SHOWN {
                println!("  … the first {SHOWN} of each");
            }
            let mut worse = if hits_only { run::hit_regressions(&baseline, &report, tolerance) } else { run::regressions(&baseline, &report, tolerance) };
            if let Some(lead) = total_lead {
                let (b, r) = (baseline.total.top1 * 100.0, report.total.top1 * 100.0);
                if r < b + lead {
                    worse.push(format!("TOTAL: top1 {r:.1}% is not {lead} points above {b:.1}%"));
                }
            }
            if !ignore_latency {
                let latency_baseline = match latency_baseline {
                    Some(p) => read(&p)?,
                    None => baseline,
                };
                worse.extend(run::latency_regressions(&latency_baseline, &report));
            }
            for w in &worse {
                println!("WORSE {w}");
            }
            anyhow::ensure!(worse.is_empty(), "{} regressions against the baseline", worse.len());
        }
        Command::RegionBoxes { canonical, pack, margin } => {
            let text = boxes::region_boxes(&canonical, &pack, margin)?;
            let out = pack.join(geko_country_pack::BOXES_FILE);
            fs::write(&out, &text)?;
            println!("{} boxes written to {}", text.lines().filter(|l| !l.starts_with('#')).count(), out.display());
        }
        Command::Fuzz { url, cases, requests, seed, out } => {
            let report = fuzz::run(&url, &read_cases(&cases)?, requests, seed);
            fs::write(&out, serde_json::to_vec_pretty(&report)?)?;
            println!(
                "{} requests: {} ok, {} client errors, {} failures; p50 {:.1} ms, p99 {:.1} ms, max {:.1} ms",
                report.requests,
                report.ok,
                report.client_errors,
                report.failures.len(),
                report.p50_ms,
                report.p99_ms,
                report.max_ms
            );
            for (url, error) in report.failures.iter().take(10) {
                println!("FAIL {url}: {error}");
            }
            anyhow::ensure!(report.failures.is_empty(), "{} failed requests (see {})", report.failures.len(), out.display());
        }
    }
    Ok(())
}

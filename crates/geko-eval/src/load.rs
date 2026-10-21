//! Load test (PROJECT.md §13.4): `concurrency` threads send the reference queries in turn for a
//! while, each over its own keep-alive connection, after a warm-up. Reports throughput and the
//! latency percentiles seen by the client.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::golden::Case;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Route {
    Search,
    Autocomplete,
    Reverse,
}

impl std::str::FromStr for Route {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "search" => Ok(Route::Search),
            "autocomplete" => Ok(Route::Autocomplete),
            "reverse" => Ok(Route::Reverse),
            _ => Err(format!("unknown route `{s}`: search, autocomplete or reverse")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadReport {
    pub route: Route,
    pub concurrency: usize,
    pub requests: usize,
    pub errors: usize,
    pub qps: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

pub const HEADER: &str = "      route concurrency  requests      QPS   p50 ms   p95 ms   p99 ms   max ms  errors";

impl LoadReport {
    pub fn row(&self) -> String {
        format!(
            "{:>11} {:>11} {:>9} {:>8.0} {:>8.2} {:>8.2} {:>8.2} {:>8.1} {:>7}",
            format!("{:?}", self.route).to_lowercase(),
            self.concurrency,
            self.requests,
            self.qps,
            self.p50_ms,
            self.p95_ms,
            self.p99_ms,
            self.max_ms,
            self.errors
        )
    }
}

/// Load runs on the same machine still vary by ±20 % (AGENTS.md): a p99 or a throughput is worse
/// only past this factor, and a p99 also by this many milliseconds.
pub const LOAD_FACTOR: f64 = 1.5;
pub const LOAD_MARGIN_MS: f64 = 1.0;

/// Concurrencies of `report` with a clearly higher p99, a clearly lower throughput or more
/// errors than the same route and concurrency in `baseline`.
pub fn regressions(baseline: &[LoadReport], report: &[LoadReport]) -> Vec<String> {
    let mut out = Vec::new();
    for b in baseline {
        let name = format!("{} ×{}", format!("{:?}", b.route).to_lowercase(), b.concurrency);
        let Some(r) = report.iter().find(|r| r.route == b.route && r.concurrency == b.concurrency) else {
            out.push(format!("{name}: missing from the report"));
            continue;
        };
        if r.p99_ms > b.p99_ms * LOAD_FACTOR && r.p99_ms > b.p99_ms + LOAD_MARGIN_MS {
            out.push(format!("{name}: p99 {:.2} -> {:.2} ms", b.p99_ms, r.p99_ms));
        }
        if r.qps * LOAD_FACTOR < b.qps {
            out.push(format!("{name}: throughput {:.0} -> {:.0} QPS", b.qps, r.qps));
        }
        if r.errors > b.errors {
            out.push(format!("{name}: errors {} -> {}", b.errors, r.errors));
        }
    }
    out
}

fn request(agent: &ureq::Agent, base: &str, route: Route, case: &Case) -> Result<(), String> {
    let base = base.trim_end_matches('/');
    let req = match route {
        Route::Search => agent.get(&format!("{base}/v1/search")).query("text", &case.query),
        Route::Autocomplete => agent.get(&format!("{base}/v1/autocomplete")).query("text", &case.query),
        Route::Reverse => agent.get(&format!("{base}/v1/reverse")).query("lat", &case.lat.to_string()).query("lon", &case.lon.to_string()),
    };
    let response = req.call().map_err(|e| e.to_string())?;
    // Read the whole body, as a real client would.
    std::io::copy(&mut response.into_reader(), &mut std::io::sink()).map_err(|e| e.to_string())?;
    Ok(())
}

/// The latencies (µs) and failed requests of one measure.
struct Samples {
    latencies: Vec<u32>,
    errors: usize,
    seconds: f64,
}

/// Sends `cases` to `route` from `concurrency` threads for `duration`, after `warmup`.
pub fn run(base: &str, cases: &[Case], route: Route, concurrency: usize, warmup: Duration, duration: Duration) -> LoadReport {
    summarize(route, concurrency, vec![sample(base, cases, route, concurrency, warmup, duration)])
}

/// Slices of each side in an A/B load.
const AB_SLICES: u32 = 5;

/// The same load on two servers measured in turns, in `AB_SLICES` slices each (base and other,
/// then other and base, and so on), so that whatever else the machine does in that time weighs the
/// same on both (AGENTS.md: measure A/B in the same run). Each side is measured for `duration` in
/// all, and each of its figures is the median of its slices: a burst of the machine that spoils
/// one or two of them does not move it.
pub fn run_ab(base_url: &str, url: &str, cases: &[Case], route: Route, concurrency: usize, warmup: Duration, duration: Duration) -> (LoadReport, LoadReport) {
    let slice = duration / AB_SLICES;
    for u in [base_url, url] {
        sample(u, cases, route, concurrency, warmup, Duration::ZERO);
    }
    let (mut base, mut other) = (Vec::new(), Vec::new());
    for i in 0..AB_SLICES {
        for on_base in [i % 2 == 0, i % 2 == 1] {
            let s = sample(if on_base { base_url } else { url }, cases, route, concurrency, Duration::ZERO, slice);
            if on_base { base.push(s) } else { other.push(s) }
        }
    }
    (median_of_slices(route, concurrency, base), median_of_slices(route, concurrency, other))
}

/// One report of `slices`: the requests and errors of all of them, the slowest request, and the
/// median of their throughputs and percentiles.
fn median_of_slices(route: Route, concurrency: usize, slices: Vec<Samples>) -> LoadReport {
    let reports: Vec<LoadReport> = slices.into_iter().map(|s| summarize(route, concurrency, vec![s])).collect();
    let median = |f: fn(&LoadReport) -> f64| {
        let mut v: Vec<f64> = reports.iter().map(f).collect();
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    LoadReport {
        route,
        concurrency: concurrency.max(1),
        requests: reports.iter().map(|r| r.requests).sum(),
        errors: reports.iter().map(|r| r.errors).sum(),
        qps: median(|r| r.qps),
        p50_ms: median(|r| r.p50_ms),
        p95_ms: median(|r| r.p95_ms),
        p99_ms: median(|r| r.p99_ms),
        max_ms: reports.iter().map(|r| r.max_ms).fold(0.0, f64::max),
    }
}

fn sample(base: &str, cases: &[Case], route: Route, concurrency: usize, warmup: Duration, duration: Duration) -> Samples {
    let concurrency = concurrency.max(1);
    let results: Vec<(Vec<u32>, usize)> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..concurrency)
            .map(|t| {
                s.spawn(move || {
                    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(10)).build();
                    let mut latencies = Vec::new();
                    let mut errors = 0;
                    let mut i = t;
                    let start = Instant::now();
                    let (measure_from, end) = (start + warmup, start + warmup + duration);
                    loop {
                        let sent = Instant::now();
                        if sent >= end {
                            break;
                        }
                        let ok = request(&agent, base, route, &cases[i % cases.len()]).is_ok();
                        if sent >= measure_from {
                            if ok {
                                latencies.push(sent.elapsed().as_micros().min(u32::MAX as u128) as u32);
                            } else {
                                errors += 1;
                            }
                        }
                        i += concurrency;
                    }
                    (latencies, errors)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("load thread")).collect()
    });
    let errors = results.iter().map(|r| r.1).sum();
    Samples { latencies: results.into_iter().flat_map(|r| r.0).collect(), errors, seconds: duration.as_secs_f64() }
}

fn summarize(route: Route, concurrency: usize, samples: Vec<Samples>) -> LoadReport {
    let errors = samples.iter().map(|s| s.errors).sum();
    let seconds: f64 = samples.iter().map(|s| s.seconds).sum();
    let mut all: Vec<u32> = samples.into_iter().flat_map(|s| s.latencies).collect();
    all.sort_unstable();
    let pct = |p: f64| -> f64 {
        if all.is_empty() {
            return 0.0;
        }
        all[((all.len() - 1) as f64 * p).round() as usize] as f64 / 1000.0
    };
    LoadReport {
        route,
        concurrency: concurrency.max(1),
        requests: all.len(),
        errors,
        qps: all.len() as f64 / seconds,
        p50_ms: pct(0.50),
        p95_ms: pct(0.95),
        p99_ms: pct(0.99),
        max_ms: all.last().map_or(0.0, |&m| m as f64 / 1000.0),
    }
}


/// The objectives of PROJECT.md §1.1, only meaningful on the reference hardware: a p99 under
/// `max_p99_ms` with one request at a time, and a throughput of `min_qps` at some concurrency
/// that still keeps that p99.
pub fn objectives(report: &[LoadReport], max_p99_ms: Option<f64>, min_qps: Option<f64>) -> Vec<String> {
    let mut out = Vec::new();
    let Some(first) = report.iter().min_by_key(|r| r.concurrency) else { return out };
    let route = format!("{:?}", first.route).to_lowercase();
    if let Some(max) = max_p99_ms
        && first.p99_ms > max
    {
        out.push(format!("{route} ×{}: p99 {:.2} ms, objective < {max} ms", first.concurrency, first.p99_ms));
    }
    if let Some(min) = min_qps {
        let best = report.iter().filter(|r| max_p99_ms.is_none_or(|max| r.p99_ms <= max)).map(|r| r.qps).fold(0.0, f64::max);
        if best < min {
            out.push(format!("{route}: {best:.0} QPS keeping the p99, objective ≥ {min}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(p99_ms: f64, qps: f64) -> LoadReport {
        LoadReport { route: Route::Search, concurrency: 16, requests: 1000, errors: 0, qps, p50_ms: 1.0, p95_ms: 2.0, p99_ms, max_ms: 9.0 }
    }

    #[test]
    fn only_a_clearly_worse_load_is_a_regression() {
        let base = [report(4.0, 2000.0)];
        assert!(regressions(&base, &[report(5.0, 1700.0)]).is_empty(), "noise");
        assert_eq!(regressions(&base, &[report(7.0, 2000.0)]), ["search ×16: p99 4.00 -> 7.00 ms"]);
        assert_eq!(regressions(&base, &[report(4.0, 1200.0)]), ["search ×16: throughput 2000 -> 1200 QPS"]);
        assert_eq!(regressions(&base, &[]), ["search ×16: missing from the report"]);
    }

    #[test]
    fn the_halves_of_a_side_count_as_one_measure() {
        let half = |latencies: Vec<u32>| Samples { latencies, errors: 1, seconds: 5.0 };
        let r = summarize(Route::Reverse, 16, vec![half(vec![1_000; 99]), half(vec![1_000, 9_000])]);
        assert_eq!((r.requests, r.errors), (101, 2));
        assert!((r.qps - 10.1).abs() < 1e-9, "101 requests in 10 s");
        assert_eq!(r.p50_ms, 1.0);
        assert_eq!(r.max_ms, 9.0);
    }

    #[test]
    fn a_burst_in_one_slice_does_not_move_the_median() {
        let slice = |ms: u32| Samples { latencies: vec![ms; 1000], errors: 0, seconds: 1.0 };
        let r = median_of_slices(Route::Search, 1, vec![slice(1_000), slice(1_100), slice(30_000), slice(900), slice(1_000)]);
        assert_eq!(r.p99_ms, 1.0);
        assert_eq!(r.qps, 1000.0);
        assert_eq!((r.requests, r.max_ms), (5000, 30.0), "but the slowest request is kept");
    }

    #[test]
    fn objectives_hold_at_one_request_and_at_the_best_concurrency() {
        let mut one = report(3.0, 900.0);
        one.concurrency = 1;
        assert!(objectives(&[one.clone(), report(4.5, 2500.0)], Some(5.0), Some(2000.0)).is_empty());
        assert_eq!(objectives(&[one.clone(), report(9.0, 2500.0)], Some(5.0), Some(2000.0)).len(), 1, "2500 QPS but not within the p99");
        one.p99_ms = 6.0;
        assert_eq!(objectives(&[one], Some(5.0), None).len(), 1);
    }
}

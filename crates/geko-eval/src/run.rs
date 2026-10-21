//! Runs reference queries against any Pelias-compatible HTTP API (geko or Pelias), or a
//! Photon-compatible one, and scores them (PROJECT.md §13.1–13.2). Engines name entities
//! differently, so a result is correct when it lies within the case's tolerance of the expected
//! point.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::golden::Case;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outcome {
    pub category: String,
    pub query: String,
    pub rank: Option<usize>,
    pub latency_ms: f64,
    pub first_label: Option<String>,
    pub first_distance_m: Option<f64>,
    /// `confidence` the engine gave its first result.
    #[serde(default)]
    pub first_confidence: Option<f64>,
    /// Two of the results are the same entity (PROJECT.md §13.6).
    #[serde(default)]
    pub duplicates: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CategoryScore {
    pub cases: usize,
    pub top1: f64,
    pub top5: f64,
    pub mrr: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    #[serde(default)]
    pub p99_ms: f64,
    pub errors: usize,
    /// Cases whose results hold the same entity twice.
    #[serde(default)]
    pub duplicates: usize,
}

/// How well the `confidence` of the first result predicts that it is the one asked for.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Calibration {
    pub cases: usize,
    /// Expected calibration error: mean gap between confidence and hit rate, in ten bins.
    pub ece: f64,
    pub brier: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub name: String,
    pub url: String,
    pub categories: BTreeMap<String, CategoryScore>,
    pub total: CategoryScore,
    #[serde(default)]
    pub calibration: Option<Calibration>,
    pub outcomes: Vec<Outcome>,
}

fn distance_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    geko_geom::distance_m(((lon1 * 1e7) as i32, (lat1 * 1e7) as i32), ((lon2 * 1e7) as i32, (lat2 * 1e7) as i32))
}

struct Found {
    lat: f64,
    lon: f64,
    label: Option<String>,
    gid: Option<String>,
    confidence: Option<f64>,
}

/// Results closer than this with the same label are the same entity, as the safety net of
/// PROJECT.md §6.5.6 would see them.
const SAME_ENTITY_M: f64 = 100.0;

fn has_duplicates(results: &[Found]) -> bool {
    results.iter().enumerate().any(|(i, a)| {
        results[i + 1..].iter().any(|b| {
            let same_gid = a.gid.is_some() && a.gid == b.gid;
            let same_label = matches!((&a.label, &b.label), (Some(x), Some(y)) if x.trim().to_lowercase() == y.trim().to_lowercase());
            same_gid || (same_label && distance_m(a.lat, a.lon, b.lat, b.lon) < SAME_ENTITY_M)
        })
    })
}

struct Answer {
    results: Vec<Found>,
    /// Names of the municipalities among the administrative units of a reverse answer.
    municipalities: Vec<String>,
}

/// The API a run speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Api {
    /// Pelias's `/v1/search`, `/v1/autocomplete` and `/v1/reverse`, which geko also answers.
    Pelias,
    /// Photon's `/api` and `/reverse`. Layers of reverse cases go as geko names, which geko's
    /// Photon mode passes through.
    Photon,
}

fn ask(agent: &ureq::Agent, url: &str, api: Api, case: &Case, size: usize) -> Result<Answer> {
    let base = url.trim_end_matches('/');
    let mut request = if api == Api::Photon {
        let request = if let Some(layers) = &case.reverse {
            let (lat, lon) = case.query.split_once(',').ok_or_else(|| anyhow::anyhow!("a reverse query is \"lat,lon\""))?;
            let request = agent.get(&format!("{base}/reverse")).query("lat", lat).query("lon", lon);
            if layers.is_empty() { request } else { request.query("layer", layers) }
        } else {
            let mut request = agent.get(&format!("{base}/api")).query("q", &case.query);
            if let (Some(lat), Some(lon)) = (case.focus_lat, case.focus_lon) {
                request = request.query("lat", &lat.to_string()).query("lon", &lon.to_string());
            }
            request
        };
        request.query("limit", &size.to_string())
    } else if let Some(layers) = &case.reverse {
        let (lat, lon) = case.query.split_once(',').ok_or_else(|| anyhow::anyhow!("a reverse query is \"lat,lon\""))?;
        let request = agent.get(&format!("{base}/v1/reverse")).query("point.lat", lat).query("point.lon", lon);
        if layers.is_empty() { request } else { request.query("layers", layers) }
    } else {
        let path = if case.autocomplete { "autocomplete" } else { "search" };
        agent.get(&format!("{base}/v1/{path}")).query("text", &case.query)
    };
    if api == Api::Pelias {
        request = request.query("size", &size.to_string());
        if let (Some(lat), Some(lon)) = (case.focus_lat, case.focus_lon) {
            request = request.query("focus.point.lat", &lat.to_string()).query("focus.point.lon", &lon.to_string());
        }
    }
    let response = request.call()?;
    let body: serde_json::Value = serde_json::from_reader(response.into_reader())?;
    let municipalities = body["geocoding"]["admin"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| a["kind"] == "municipality")
        .filter_map(|a| a["name"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    // Pelias and Photon give the units of each result, not of the point: those of the first.
    let municipalities = if municipalities.is_empty() {
        let first = &body["features"][0]["properties"];
        ["localadmin", "locality", "city"].iter().filter_map(|k| first[*k].as_str().map(str::to_owned)).collect()
    } else {
        municipalities
    };
    let results = body["features"]
        .as_array()
        .map(|features| {
            features
                .iter()
                .filter_map(|f| {
                    let c = f["geometry"]["coordinates"].as_array()?;
                    let p = &f["properties"];
                    Some(Found {
                        lat: c.get(1)?.as_f64()?,
                        lon: c.first()?.as_f64()?,
                        label: p["label"].as_str().map(str::to_owned),
                        gid: p["gid"].as_str().map(str::to_owned),
                        confidence: p["confidence"].as_f64(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Answer { results, municipalities })
}

/// Position of the result asked for: the municipality of a reverse query among its
/// administrative units, the result with the case's id for a reverse query, and otherwise the
/// first result within the case's tolerance (engines name entities differently).
fn rank(case: &Case, answer: &Answer) -> Option<usize> {
    if let Some(name) = &case.admin {
        return answer.municipalities.iter().any(|m| m == name).then_some(0);
    }
    if case.reverse.is_some()
        && let Some(gid) = &case.gid
    {
        return answer.results.iter().position(|r| r.gid.as_ref() == Some(gid));
    }
    answer.results.iter().position(|r| distance_m(r.lat, r.lon, case.lat, case.lon) <= case.tolerance_m)
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn score(outcomes: &[&Outcome]) -> CategoryScore {
    let n = outcomes.len().max(1) as f64;
    let mut latencies: Vec<f64> = outcomes.iter().map(|o| o.latency_ms).collect();
    latencies.sort_by(f64::total_cmp);
    CategoryScore {
        cases: outcomes.len(),
        top1: outcomes.iter().filter(|o| o.rank == Some(0)).count() as f64 / n,
        top5: outcomes.iter().filter(|o| o.rank.is_some_and(|r| r < 5)).count() as f64 / n,
        mrr: outcomes.iter().map(|o| o.rank.map_or(0.0, |r| 1.0 / (r + 1) as f64)).sum::<f64>() / n,
        p50_ms: percentile(&latencies, 0.5),
        p95_ms: percentile(&latencies, 0.95),
        p99_ms: percentile(&latencies, 0.99),
        errors: outcomes.iter().filter(|o| o.error.is_some()).count(),
        duplicates: outcomes.iter().filter(|o| o.duplicates).count(),
    }
}

fn calibration(outcomes: &[Outcome]) -> Option<Calibration> {
    let scored: Vec<(f64, f64)> =
        outcomes.iter().filter_map(|o| Some((o.first_confidence?.clamp(0.0, 1.0), if o.rank == Some(0) { 1.0 } else { 0.0 }))).collect();
    if scored.is_empty() {
        return None;
    }
    let n = scored.len() as f64;
    let mut bins = [(0.0, 0.0, 0usize); 10];
    for &(c, hit) in &scored {
        let b = &mut bins[((c * 10.0) as usize).min(9)];
        b.0 += c;
        b.1 += hit;
        b.2 += 1;
    }
    // Σ (k/n)·|mean confidence − hit rate| over the bins, with the sums of each bin.
    let ece = bins.iter().map(|&(c, hit, _)| (c - hit).abs() / n).sum();
    let brier = scored.iter().map(|(c, hit)| (c - hit).powi(2)).sum::<f64>() / n;
    Some(Calibration { cases: scored.len(), ece, brier })
}

pub fn run(name: &str, url: &str, api: Api, cases: &[Case], delay: Duration, size: usize) -> Report {
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(30)).build();
    let mut outcomes = Vec::with_capacity(cases.len());
    for (i, case) in cases.iter().enumerate() {
        let started = Instant::now();
        let result = ask(&agent, url, api, case, size);
        let latency_ms = started.elapsed().as_secs_f64() * 1000.0;
        let outcome = match result {
            Ok(answer) => Outcome {
                category: case.category.clone(),
                query: case.query.clone(),
                rank: rank(case, &answer),
                latency_ms,
                first_label: answer.results.first().and_then(|r| r.label.clone()),
                first_distance_m: answer.results.first().map(|r| distance_m(r.lat, r.lon, case.lat, case.lon)),
                // The confidence of a reverse result is relative to the nearest, not a probability.
                first_confidence: answer.results.first().and_then(|r| r.confidence).filter(|_| case.reverse.is_none()),
                duplicates: has_duplicates(&answer.results),
                error: None,
            },
            Err(e) => Outcome {
                category: case.category.clone(),
                query: case.query.clone(),
                rank: None,
                latency_ms,
                first_label: None,
                first_distance_m: None,
                first_confidence: None,
                duplicates: false,
                error: Some(e.to_string()),
            },
        };
        outcomes.push(outcome);
        if (i + 1) % 100 == 0 {
            eprintln!("{name}: {}/{}", i + 1, cases.len());
        }
        std::thread::sleep(delay);
    }
    let mut by_category: BTreeMap<String, Vec<&Outcome>> = BTreeMap::new();
    for o in &outcomes {
        by_category.entry(o.category.clone()).or_default().push(o);
    }
    let categories = by_category.iter().map(|(k, v)| (k.clone(), score(v))).collect();
    let total = score(&outcomes.iter().collect::<Vec<_>>());
    let calibration = calibration(&outcomes);
    Report { name: name.into(), url: url.into(), categories, total, calibration, outcomes }
}

pub fn table(reports: &[Report]) -> String {
    let mut out = String::new();
    let mut categories: Vec<&String> = reports.iter().flat_map(|r| r.categories.keys()).collect();
    categories.sort();
    categories.dedup();
    out.push_str(&format!("{:<28} {:>5}", "category", "cases"));
    for r in reports {
        out.push_str(&format!(" | {:>22}", format!("{} top1/top5/p50ms", r.name)));
    }
    out.push('\n');
    let row = |out: &mut String, name: &str, pick: &dyn Fn(&Report) -> Option<&CategoryScore>| {
        let cases = reports.iter().find_map(pick).map_or(0, |s| s.cases);
        out.push_str(&format!("{name:<28} {cases:>5}"));
        for r in reports {
            match pick(r) {
                Some(s) => out.push_str(&format!(" | {:>6.1}% {:>6.1}% {:>6.1}", s.top1 * 100.0, s.top5 * 100.0, s.p50_ms)),
                None => out.push_str(&format!(" | {:>22}", "-")),
            }
        }
        out.push('\n');
    };
    for c in categories {
        row(&mut out, c, &|r| r.categories.get(c));
    }
    row(&mut out, "TOTAL", &|r| Some(&r.total));
    out
}

/// A median latency is worse only if it grows by both this factor and this many milliseconds
/// (×1.5 failed twice on single-run noise of +50–60 %; the 3b slowdown it must catch was ×5):
/// sub-millisecond categories move by a few tenths between runs.
pub const LATENCY_FACTOR: f64 = 2.0;
pub const LATENCY_MARGIN_MS: f64 = 1.0;
/// The p99 of all the queries is one of the slowest few: it moves more between runs.
pub const P99_MARGIN_MS: f64 = 2.0;
/// Allowed rise of the calibration error: a ranking change that keeps the hit rates still moves
/// the confidences a little.
pub const ECE_MARGIN: f64 = 0.02;

fn rows(report: &Report) -> Vec<(&str, &CategoryScore)> {
    let mut rows: Vec<(&str, &CategoryScore)> = report.categories.iter().map(|(k, v)| (k.as_str(), v)).collect();
    rows.push(("TOTAL", &report.total));
    rows
}

fn row<'a>(report: &'a Report, name: &str) -> Option<&'a CategoryScore> {
    if name == "TOTAL" { Some(&report.total) } else { report.categories.get(name) }
}

/// Categories of `report` worse than in `baseline` by more than `tolerance` percentage points
/// in top-1 or top-5 (CLAUDE.md: no category may get worse). A category of the baseline missing
/// from the report is a regression too.
pub fn hit_regressions(baseline: &Report, report: &Report, tolerance: f64) -> Vec<String> {
    let mut out = Vec::new();
    for (name, base) in rows(baseline) {
        let Some(now) = row(report, name) else {
            out.push(format!("{name}: missing from the report"));
            continue;
        };
        for (metric, b, n) in [("top1", base.top1, now.top1), ("top5", base.top5, now.top5)] {
            if (b - n) * 100.0 > tolerance + 1e-9 {
                out.push(format!("{name}: {metric} {:.1}% -> {:.1}%", b * 100.0, n * 100.0));
            }
        }
    }
    out
}

/// The hit regressions, and also categories with more errors or more duplicated results, and a
/// calibration of `confidence` clearly worse.
pub fn regressions(baseline: &Report, report: &Report, tolerance: f64) -> Vec<String> {
    let mut out = hit_regressions(baseline, report, tolerance);
    for (name, base) in rows(baseline) {
        let Some(now) = row(report, name) else { continue };
        if now.errors > base.errors {
            out.push(format!("{name}: errors {} -> {}", base.errors, now.errors));
        }
        if now.duplicates > base.duplicates {
            out.push(format!("{name}: cases with a duplicated result {} -> {}", base.duplicates, now.duplicates));
        }
    }
    if let (Some(b), Some(n)) = (&baseline.calibration, &report.calibration)
        && n.ece > b.ece + ECE_MARGIN
    {
        out.push(format!("calibration error of confidence {:.3} -> {:.3}", b.ece, n.ece));
    }
    out
}

/// Categories with a clearly slower median, and a clearly slower p99 of all the queries. Only
/// meaningful against a baseline measured on the same machine.
pub fn latency_regressions(baseline: &Report, report: &Report) -> Vec<String> {
    let mut out = Vec::new();
    for (name, base) in rows(baseline) {
        let Some(now) = row(report, name) else { continue };
        if now.p50_ms > base.p50_ms * LATENCY_FACTOR && now.p50_ms > base.p50_ms + LATENCY_MARGIN_MS {
            out.push(format!("{name}: median latency {:.1} -> {:.1} ms", base.p50_ms, now.p50_ms));
        }
    }
    let (b, n) = (baseline.total.p99_ms, report.total.p99_ms);
    if b > 0.0 && n > b * LATENCY_FACTOR && n > b + P99_MARGIN_MS {
        out.push(format!("TOTAL: p99 latency {b:.1} -> {n:.1} ms"));
    }
    out
}

/// Queries that changed between the two runs of the same cases, lost first: a category can keep
/// its rate while some of its queries break and others get fixed.
pub fn changes(baseline: &Report, report: &Report) -> (Vec<String>, Vec<String>) {
    let mut before: BTreeMap<(&str, &str), Vec<Option<usize>>> = BTreeMap::new();
    for o in &baseline.outcomes {
        before.entry((&o.category, &o.query)).or_default().push(o.rank);
    }
    let rank = |r: Option<usize>| r.map_or("missing".to_owned(), |r| format!("#{}", r + 1));
    let (mut lost, mut gained) = (Vec::new(), Vec::new());
    for o in &report.outcomes {
        let Some(old) = before.get_mut(&(o.category.as_str(), o.query.as_str())).and_then(|v| (!v.is_empty()).then(|| v.remove(0))) else {
            continue;
        };
        let key = |r: Option<usize>| r.unwrap_or(usize::MAX);
        if key(o.rank) == key(old) {
            continue;
        }
        let line = format!("{} «{}»: {} -> {}", o.category, o.query, rank(old), rank(o.rank));
        if key(o.rank) > key(old) { lost.push(line) } else { gained.push(line) }
    }
    (lost, gained)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(top1: f64, top5: f64, errors: usize) -> Report {
        timed(top1, top5, errors, 0.4)
    }

    fn timed(top1: f64, top5: f64, errors: usize, p50_ms: f64) -> Report {
        let score = CategoryScore { cases: 100, top1, top5, errors, p50_ms, ..Default::default() };
        Report {
            name: "r".into(),
            url: String::new(),
            categories: BTreeMap::from([("street".to_string(), score.clone())]),
            total: score,
            calibration: None,
            outcomes: vec![],
        }
    }

    fn outcome(query: &str, rank: Option<usize>, confidence: f64) -> Outcome {
        Outcome {
            category: "street".into(),
            query: query.into(),
            rank,
            latency_ms: 1.0,
            first_label: None,
            first_distance_m: None,
            first_confidence: Some(confidence),
            duplicates: false,
            error: None,
        }
    }

    #[test]
    fn any_worse_category_is_a_regression() {
        let base = report(0.95, 0.99, 0);
        assert!(regressions(&base, &report(0.95, 0.99, 0), 0.0).is_empty());
        assert!(regressions(&base, &report(0.97, 1.0, 0), 0.0).is_empty(), "better is fine");
        assert_eq!(regressions(&base, &report(0.94, 0.99, 0), 0.0).len(), 2, "category and total");
        assert!(regressions(&base, &report(0.94, 0.99, 0), 1.0).is_empty(), "within tolerance");
        assert!(!regressions(&base, &report(0.95, 0.99, 1), 0.0).is_empty(), "more errors");
        let mut missing = report(0.95, 0.99, 0);
        missing.categories.clear();
        assert_eq!(regressions(&base, &missing, 0.0), ["street: missing from the report"]);
    }

    #[test]
    fn more_duplicated_results_are_a_regression() {
        let base = report(0.95, 0.99, 0);
        let mut now = report(0.95, 0.99, 0);
        now.total.duplicates = 1;
        assert_eq!(regressions(&base, &now, 0.0), ["TOTAL: cases with a duplicated result 0 -> 1"]);
    }

    #[test]
    fn a_clearly_slower_category_is_a_regression() {
        let base = timed(0.95, 0.99, 0, 0.5);
        assert!(latency_regressions(&base, &timed(0.95, 0.99, 0, 0.9)).is_empty(), "noise below a millisecond");
        assert!(latency_regressions(&base, &timed(0.95, 0.99, 0, 1.4)).is_empty(), "under the margin");
        assert_eq!(latency_regressions(&base, &timed(0.95, 0.99, 0, 2.5)).len(), 2, "five times slower, as 3b once was");
        let slow = timed(0.95, 0.99, 0, 8.0);
        assert!(latency_regressions(&slow, &timed(0.95, 0.99, 0, 10.0)).is_empty(), "under the factor");
        let mut tail = timed(0.95, 0.99, 0, 0.5);
        tail.total.p99_ms = 3.0;
        let mut slower_tail = tail.clone();
        slower_tail.total.p99_ms = 9.0;
        assert_eq!(latency_regressions(&tail, &slower_tail), ["TOTAL: p99 latency 3.0 -> 9.0 ms"]);
    }

    #[test]
    fn calibration_compares_confidence_with_hits() {
        let perfect: Vec<Outcome> = (0..10).map(|i| outcome("q", (i < 9).then_some(0), 0.9)).collect();
        let c = calibration(&perfect).unwrap();
        assert!(c.ece < 1e-9, "nine in ten right at 0.9: {}", c.ece);
        let overconfident: Vec<Outcome> = (0..10).map(|i| outcome("q", (i < 5).then_some(0), 0.9)).collect();
        assert!((calibration(&overconfident).unwrap().ece - 0.4).abs() < 1e-9);
        let mut base = report(0.95, 0.99, 0);
        base.calibration = Some(Calibration { cases: 10, ece: 0.05, brier: 0.1 });
        let mut now = base.clone();
        now.calibration = Some(Calibration { cases: 10, ece: 0.10, brier: 0.1 });
        assert_eq!(regressions(&base, &now, 0.0).len(), 1);
    }

    #[test]
    fn changes_list_each_query_lost_or_gained() {
        let mut base = report(0.5, 0.5, 0);
        base.outcomes = vec![outcome("a", Some(0), 0.9), outcome("b", None, 0.9), outcome("c", Some(1), 0.9)];
        let mut now = base.clone();
        now.outcomes = vec![outcome("a", Some(2), 0.9), outcome("b", Some(0), 0.9), outcome("c", Some(1), 0.9)];
        let (lost, gained) = changes(&base, &now);
        assert_eq!(lost, ["street «a»: #1 -> #3"]);
        assert_eq!(gained, ["street «b»: missing -> #1"]);
    }

    fn found(lat: f64, label: &str, gid: &str) -> Found {
        Found { lat, lon: 2.65, label: Some(label.into()), gid: Some(gid.into()), confidence: None }
    }

    #[test]
    fn same_entity_twice_is_a_duplicate() {
        assert!(!has_duplicates(&[found(39.57, "Plaça Major, Palma", "a"), found(39.58, "Plaça Major, Palma", "b")]), "homonyms 1 km apart");
        assert!(has_duplicates(&[found(39.57, "Plaça Major, Palma", "a"), found(39.5701, "plaça major, palma", "b")]));
        assert!(has_duplicates(&[found(39.57, "x", "a"), found(39.60, "y", "a")]), "same gid");
    }
}

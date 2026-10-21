//! Random and mutated requests against a running server: any 5xx or dropped connection is a
//! bug, and the slowest queries are the candidates for the latency work (PROJECT.md §11).

use std::time::{Duration, Instant};

use serde::Serialize;

use crate::golden::{Case, Rng};

const ALPHABET: &[&str] = &[
    "a", "e", "o", "s", "r", "n", "l", "1", "7", "0", " ", " ", ",", ".", "-", "'", "/", "º", "ª", "°", "ñ", "ç", "à", "é", "í", "ò",
    "ú", "ü", "Ñ", "Ç", "·", "l·l", "\u{301}", "ß", "İ", "ﬁ", "日本", "🙂", "\t", "\u{200b}", "\u{0}", "%", "&", "+", "\"", "\\", "*",
    "?", "#", "N", "W", "UTM", "30S", "bis", "s/n", "km",
];

#[derive(Debug, Serialize)]
pub struct Slow {
    pub url: String,
    pub ms: f64,
}

#[derive(Debug, Default, Serialize)]
pub struct FuzzReport {
    pub requests: usize,
    pub ok: usize,
    pub client_errors: usize,
    pub failures: Vec<(String, String)>,
    pub p50_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
    pub slowest: Vec<Slow>,
}

fn pick<'a>(rng: &mut Rng, items: &[&'a str]) -> &'a str {
    items[rng.below(items.len())]
}

fn random_text(rng: &mut Rng) -> String {
    let max = if rng.below(20) == 0 { 400 } else { 12 };
    let len = 1 + rng.below(max);
    (0..len).map(|_| pick(rng, ALPHABET)).collect()
}

fn mutate(text: &str, rng: &mut Rng) -> String {
    let mut chars: Vec<String> = text.chars().map(String::from).collect();
    for _ in 0..1 + rng.below(4) {
        let at = rng.below(chars.len() + 1);
        match rng.below(6) {
            0 if at < chars.len() => {
                chars.remove(at);
            }
            1 if at + 1 < chars.len() => chars.swap(at, at + 1),
            2 => chars.insert(at, pick(rng, ALPHABET).to_owned()),
            3 => chars.truncate(at),
            4 => chars.extend(std::iter::repeat_n(" calle".to_owned(), 1 + rng.below(30))),
            _ => chars = chars.iter().map(|c| c.to_uppercase()).collect(),
        }
    }
    chars.concat()
}

fn coordinate(rng: &mut Rng, max: f64) -> String {
    match rng.below(10) {
        0 => pick(rng, &["NaN", "inf", "-0", "", "1e400", "abc", "39,5", "999999999999"]).to_owned(),
        1 => format!("{:.6}", (rng.next() as f64 / u64::MAX as f64) * 4.0 * max - 2.0 * max),
        // Around the Balearic Islands, where the data is.
        _ if max == 90.0 => format!("{:.6}", 38.6 + rng.below(1_500_000) as f64 / 1e6),
        _ => format!("{:.6}", 1.1 + rng.below(3_300_000) as f64 / 1e6),
    }
}

fn request(rng: &mut Rng, seeds: &[Case]) -> (String, Vec<(&'static str, String)>) {
    match rng.below(10) {
        0..=1 => {
            let (lat, lon) = if rng.below(2) == 0 { ("lat", "lon") } else { ("point.lat", "point.lon") };
            ("reverse".into(), vec![(lat, coordinate(rng, 90.0)), (lon, coordinate(rng, 180.0))])
        }
        k => {
            let text = if seeds.is_empty() || rng.below(3) == 0 {
                random_text(rng)
            } else {
                mutate(&seeds[rng.below(seeds.len())].query, rng)
            };
            let mut params = vec![("text", text)];
            if rng.below(4) == 0 {
                let (lat, lon) = if rng.below(2) == 0 { ("focus.lat", "focus.lon") } else { ("focus.point.lat", "focus.point.lon") };
                params.push((lat, coordinate(rng, 90.0)));
                params.push((lon, coordinate(rng, 180.0)));
            }
            if rng.below(8) == 0 {
                params.push(("size", pick(rng, &["0", "1", "40", "1000", "-1", "x"]).to_owned()));
            }
            let path = if k < 6 { "search" } else { "autocomplete" };
            (path.into(), params)
        }
    }
}

pub fn run(base: &str, seeds: &[Case], requests: usize, seed: u64) -> FuzzReport {
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(10)).build();
    let base = base.trim_end_matches('/');
    let mut rng = Rng::new(seed);
    let mut report = FuzzReport { requests, ..Default::default() };
    let mut timings: Vec<(f64, String)> = Vec::with_capacity(requests);
    for i in 0..requests {
        let (path, params) = request(&mut rng, seeds);
        let mut req = agent.get(&format!("{base}/v1/{path}"));
        for (k, v) in &params {
            req = req.query(k, v);
        }
        let url = req.url().to_owned();
        let started = Instant::now();
        let result = req.call();
        let ms = started.elapsed().as_secs_f64() * 1e3;
        match result {
            Ok(r) => {
                // Reading the body catches a connection dropped mid-response.
                match r.into_string() {
                    Ok(_) => report.ok += 1,
                    Err(e) => report.failures.push((url.clone(), e.to_string())),
                }
            }
            Err(ureq::Error::Status(code, _)) if code < 500 => report.client_errors += 1,
            Err(e) => report.failures.push((url.clone(), e.to_string())),
        }
        timings.push((ms, url));
        if (i + 1) % 5000 == 0 {
            eprintln!("fuzz: {}/{requests}", i + 1);
        }
    }
    timings.sort_by(|a, b| a.0.total_cmp(&b.0));
    let at = |p: f64| timings.get(((timings.len().max(1) - 1) as f64 * p) as usize).map_or(0.0, |t| t.0);
    report.p50_ms = at(0.5);
    report.p99_ms = at(0.99);
    report.max_ms = at(1.0);
    report.slowest = timings.iter().rev().take(25).map(|(ms, url)| Slow { url: url.clone(), ms: *ms }).collect();
    report
}

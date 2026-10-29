//! What the geocoder has answered, for `v1/stats`: requests, errors and latency of every route,
//! the last hour minute by minute and the last minute second by second, and how many searches ended empty or cut by the time budget.
//! The host measures the same per route for `/metrics`, but a module cannot read it; these are
//! its own counters, a few atomics per request and one short lock for the minute it falls in.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use geko_core::model::PlaceKind;
use serde_json::{Value, json};

/// Upper bounds of the latency buckets, in microseconds; the last bucket has none.
const BOUNDS_US: [u64; 11] = [100, 250, 500, 1_000, 2_500, 5_000, 10_000, 25_000, 50_000, 100_000, 250_000];
const BUCKETS: usize = BOUNDS_US.len() + 1;
const MINUTES: usize = 60;
const SECONDS: usize = 60;
/// Tenths of confidence, 0.0 to 1.0, in which the first result of a text search is counted.
const CONFIDENCE_BUCKETS: usize = 10;
/// How long a slow query stays in the list, in seconds.
const SLOW_KEPT_SECS: u64 = 3600;
/// Longest anonymized query kept, in characters.
const SLOW_TEXT_CHARS: usize = 60;

/// The routes that are counted, as `Request::route` names them.
const ROUTES: [&str; 6] = ["v1/search", "v1/autocomplete", "v1/search/structured", "v1/reverse", "v1/place/{id}", "v1/batch"];

fn bucket(us: u64) -> usize {
    BOUNDS_US.iter().position(|&b| us <= b).unwrap_or(BOUNDS_US.len())
}

/// The upper bound, in milliseconds, of the bucket that holds the `q` quantile of the counts: the
/// real value is at most that. `None` without counts, or when it falls past the last bound.
fn quantile_ms(counts: &[u64; BUCKETS], q: f64) -> Option<f64> {
    let total: u64 = counts.iter().sum();
    if total == 0 {
        return None;
    }
    let wanted = (total as f64 * q).ceil().max(1.0) as u64;
    let mut seen = 0;
    for (i, &n) in counts.iter().enumerate() {
        seen += n;
        if seen >= wanted {
            return BOUNDS_US.get(i).map(|&b| b as f64 / 1000.0);
        }
    }
    None
}

#[derive(Default)]
struct Route {
    requests: AtomicU64,
    errors: AtomicU64,
    latency: [AtomicU64; BUCKETS],
}

#[derive(Clone, Copy, Default)]
struct Minute {
    /// Minutes (seconds, in the ring of seconds) since the epoch: an older slot is stale and starts over.
    at: u64,
    requests: u64,
    latency: [u64; BUCKETS],
}

impl Minute {
    fn add(&mut self, at: u64, bucket: usize) {
        if self.at != at {
            *self = Minute { at, ..Minute::default() };
        }
        self.requests += 1;
        self.latency[bucket] += 1;
    }
}

#[derive(Clone, Copy)]
struct Rings {
    minutes: [Minute; MINUTES],
    seconds: [Minute; SECONDS],
}

/// The slots that end at `now`, oldest first; one with no requests counts zero.
fn window<const N: usize>(ring: &[Minute; N], now: u64) -> Vec<Minute> {
    (0..N as u64)
        .map(|i| {
            let at = now.saturating_sub(N as u64 - 1 - i);
            let slot = ring[(at % N as u64) as usize];
            if slot.at == at { slot } else { Minute { at, ..Minute::default() } }
        })
        .collect()
}

pub struct Stats {
    started: u64,
    routes: [Route; ROUTES.len()],
    rings: Mutex<Rings>,
    /// Searches and autocompletes answered, those with no result and those the time budget cut short.
    searches: AtomicU64,
    empty: AtomicU64,
    cut_short: AtomicU64,
    /// The first result of each text search that found one: its confidence by tenths, and its layer.
    confidence: [AtomicU64; CONFIDENCE_BUCKETS],
    layers: [AtomicU64; PlaceKind::ALL.len()],
    /// How many of the slowest queries of the last hour to keep (0: none), the time in microseconds
    /// of the least slow of them once there are that many (so that the rest cost one atomic read),
    /// and the queries, anonymized.
    keep: usize,
    floor: AtomicU64,
    slow: Mutex<Vec<SlowQuery>>,
}

struct SlowQuery {
    text: String,
    route: &'static str,
    micros: u64,
    at: u64,
}

/// A query as it may be shown: no digit (house numbers, floors, postcodes, phone numbers), no word
/// that looks like an e-mail address or a link, and short. Names that someone typed in words stay,
/// which is why the list is off unless the operator asks for it.
pub fn anonymize(text: &str) -> String {
    let mut out = String::new();
    for word in text.split_whitespace() {
        if word.contains('@') || word.contains("://") || word.starts_with("www.") {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        let mut digits = false;
        for c in word.chars() {
            if c.is_numeric() {
                if !digits {
                    out.push('#');
                }
                digits = true;
            } else {
                digits = false;
                out.push(c);
            }
        }
    }
    out.chars().take(SLOW_TEXT_CHARS).collect::<String>().trim_end().to_owned()
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl Default for Stats {
    fn default() -> Self {
        Self {
            started: now_secs(),
            routes: Default::default(),
            rings: Mutex::new(Rings { minutes: [Minute::default(); MINUTES], seconds: [Minute::default(); SECONDS] }),
            searches: AtomicU64::new(0),
            empty: AtomicU64::new(0),
            cut_short: AtomicU64::new(0),
            confidence: Default::default(),
            layers: Default::default(),
            keep: 0,
            floor: AtomicU64::new(0),
            slow: Mutex::new(Vec::new()),
        }
    }
}

impl Stats {
    pub fn record(&self, route: &str, status: u16, micros: u64) {
        self.record_at(now_secs(), route, status, micros);
    }

    fn record_at(&self, second: u64, route: &str, status: u16, micros: u64) {
        let Some(i) = ROUTES.iter().position(|r| *r == route) else { return };
        let b = bucket(micros);
        let r = &self.routes[i];
        r.requests.fetch_add(1, Ordering::Relaxed);
        if status >= 400 {
            r.errors.fetch_add(1, Ordering::Relaxed);
        }
        r.latency[b].fetch_add(1, Ordering::Relaxed);
        let minute = second / 60;
        let mut rings = self.rings.lock().unwrap_or_else(|e| e.into_inner());
        rings.minutes[(minute % MINUTES as u64) as usize].add(minute, b);
        rings.seconds[(second % SECONDS as u64) as usize].add(second, b);
    }

    /// A search or autocomplete that found `empty` nothing, or that the time budget cut short.
    pub fn searched(&self, empty: bool, cut_short: bool) {
        self.searches.fetch_add(1, Ordering::Relaxed);
        if empty {
            self.empty.fetch_add(1, Ordering::Relaxed);
        }
        if cut_short {
            self.cut_short.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Keeps the `n` slowest queries of the last hour, anonymized; 0 keeps none.
    pub fn keeping_slow(mut self, n: usize) -> Self {
        self.keep = n;
        self
    }

    /// A text search that took `micros`: kept, anonymized, if it is among the slowest of the last hour.
    pub fn slow_query(&self, text: &str, route: &'static str, micros: u64) {
        self.slow_query_at(now_secs(), text, route, micros);
    }

    fn slow_query_at(&self, now: u64, text: &str, route: &'static str, micros: u64) {
        if self.keep == 0 || micros < self.floor.load(Ordering::Relaxed) {
            return;
        }
        let text = anonymize(text);
        if text.is_empty() {
            return;
        }
        let mut slow = self.slow.lock().unwrap_or_else(|e| e.into_inner());
        slow.retain(|q| q.at + SLOW_KEPT_SECS > now);
        match slow.iter_mut().find(|q| q.text == text && q.route == route) {
            Some(q) if q.micros < micros => (q.micros, q.at) = (micros, now),
            Some(_) => {}
            None => slow.push(SlowQuery { text, route, micros, at: now }),
        }
        slow.sort_by_key(|q| std::cmp::Reverse(q.micros));
        slow.truncate(self.keep);
        let floor = if slow.len() == self.keep { slow.last().map_or(0, |q| q.micros) } else { 0 };
        self.floor.store(floor, Ordering::Relaxed);
    }

    /// The slow queries of the last hour, slowest first; `null` when none is kept.
    fn slow_snapshot(&self, now: u64) -> Value {
        if self.keep == 0 {
            return Value::Null;
        }
        let mut slow = self.slow.lock().unwrap_or_else(|e| e.into_inner());
        slow.retain(|q| q.at + SLOW_KEPT_SECS > now);
        if slow.len() < self.keep {
            self.floor.store(0, Ordering::Relaxed);
        }
        slow.iter().map(|q| json!({ "text": q.text, "route": q.route, "ms": (q.micros as f64 / 100.0).round() / 10.0, "at_unix": q.at })).collect()
    }

    /// The first result of a text search: how sure the geocoder is of it, and what it is.
    pub fn first_result(&self, confidence: f32, kind: PlaceKind) {
        let tenth = ((confidence.clamp(0.0, 1.0) * CONFIDENCE_BUCKETS as f32) as usize).min(CONFIDENCE_BUCKETS - 1);
        self.confidence[tenth].fetch_add(1, Ordering::Relaxed);
        if let Some(i) = PlaceKind::ALL.iter().position(|k| *k == kind) {
            self.layers[i].fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn empty(&self) -> u64 {
        self.empty.load(Ordering::Relaxed)
    }

    pub fn cut_short(&self) -> u64 {
        self.cut_short.load(Ordering::Relaxed)
    }

    pub fn snapshot(&self) -> Value {
        self.snapshot_at(now_secs())
    }

    fn snapshot_at(&self, now_second: u64) -> Value {
        let now = now_second / 60;
        let routes: Vec<Value> = ROUTES
            .iter()
            .zip(&self.routes)
            .map(|(name, r)| {
                let counts: [u64; BUCKETS] = std::array::from_fn(|i| r.latency[i].load(Ordering::Relaxed));
                json!({
                    "route": name,
                    "requests": r.requests.load(Ordering::Relaxed),
                    "errors": r.errors.load(Ordering::Relaxed),
                    "p50_ms": quantile_ms(&counts, 0.5),
                    "p95_ms": quantile_ms(&counts, 0.95),
                    "p99_ms": quantile_ms(&counts, 0.99),
                })
            })
            .collect();
        let rings = *self.rings.lock().unwrap_or_else(|e| e.into_inner());
        let series = window(&rings.minutes, now);
        let recent = window(&rings.seconds, now_second);
        json!({
            "since_unix": self.started,
            "routes": routes,
            "searches": self.searches.load(Ordering::Relaxed),
            "empty": self.empty(),
            "cut_short": self.cut_short(),
            "slow_queries": self.slow_snapshot(now_second),
            "confidence": self.confidence.iter().map(|c| c.load(Ordering::Relaxed)).collect::<Vec<_>>(),
            "layers": PlaceKind::ALL
                .iter()
                .zip(&self.layers)
                .map(|(kind, n)| (kind.as_str(), n.load(Ordering::Relaxed)))
                .filter(|(_, n)| *n > 0)
                .map(|(layer, n)| json!({ "layer": layer, "count": n }))
                .collect::<Vec<_>>(),
            "last_hour": {
                "end_unix": (now + 1) * 60,
                "requests": series.iter().map(|m| m.requests).collect::<Vec<_>>(),
                "p99_ms": series.iter().map(|m| quantile_ms(&m.latency, 0.99)).collect::<Vec<_>>(),
            },
            "last_minute": {
                "end_unix": now_second + 1,
                "requests": recent.iter().map(|m| m.requests).collect::<Vec<_>>(),
                "p99_ms": recent.iter().map(|m| quantile_ms(&m.latency, 0.99)).collect::<Vec<_>>(),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantiles_are_the_bound_of_their_bucket() {
        let mut counts = [0; BUCKETS];
        assert_eq!(quantile_ms(&counts, 0.5), None);
        counts[bucket(300)] = 90;
        counts[bucket(4_000)] = 9;
        counts[bucket(40_000)] = 1;
        assert_eq!(quantile_ms(&counts, 0.5), Some(0.5));
        assert_eq!(quantile_ms(&counts, 0.95), Some(5.0));
        assert_eq!(quantile_ms(&counts, 0.99), Some(5.0));
        assert_eq!(quantile_ms(&counts, 1.0), Some(50.0));
        counts[bucket(900_000)] = 100;
        assert_eq!(quantile_ms(&counts, 1.0), None, "past the last bound");
    }

    #[test]
    fn counts_routes_errors_and_the_last_hour() {
        let stats = Stats::default();
        stats.record_at(60_000, "v1/search", 200, 800);
        stats.record_at(60_000, "v1/search", 400, 900);
        stats.record_at(60_600, "v1/reverse", 200, 200);
        stats.record_at(60_600, "nope", 200, 200);
        let s = stats.snapshot_at(60_600);
        let search = &s["routes"][0];
        assert_eq!((search["route"].as_str(), search["requests"].as_u64(), search["errors"].as_u64()), (Some("v1/search"), Some(2), Some(1)));
        assert_eq!(search["p99_ms"].as_f64(), Some(1.0));
        let hour = s["last_hour"]["requests"].as_array().unwrap();
        assert_eq!(hour.len(), MINUTES);
        assert_eq!((hour[MINUTES - 1].as_u64(), hour[MINUTES - 11].as_u64(), hour[0].as_u64()), (Some(1), Some(2), Some(0)));
    }

    #[test]
    fn the_last_minute_goes_second_by_second() {
        let stats = Stats::default();
        stats.record_at(5_000, "v1/search", 200, 800);
        stats.record_at(5_058, "v1/search", 200, 800);
        stats.record_at(5_058, "v1/search", 200, 800);
        stats.record_at(5_059, "v1/reverse", 200, 200);
        let s = stats.snapshot_at(5_059);
        let secs = s["last_minute"]["requests"].as_array().unwrap();
        assert_eq!(secs.len(), SECONDS);
        assert_eq!((secs[SECONDS - 1].as_u64(), secs[SECONDS - 2].as_u64(), secs[0].as_u64()), (Some(1), Some(2), Some(1)), "5,000 is 59 seconds ago, still inside");
        assert_eq!(s["last_minute"]["end_unix"].as_u64(), Some(5_060));
        let later = stats.snapshot_at(5_070);
        let total: u64 = later["last_minute"]["requests"].as_array().unwrap().iter().filter_map(Value::as_u64).sum();
        assert_eq!(total, 3, "the second that fell out of the window is gone");
    }

    #[test]
    fn a_minute_an_hour_old_starts_over() {
        let stats = Stats::default();
        stats.record_at(60_000, "v1/search", 200, 800);
        stats.record_at(60_000 + 60 * MINUTES as u64, "v1/search", 200, 800);
        let s = stats.snapshot_at(60_000 + 60 * MINUTES as u64);
        let total: u64 = s["last_hour"]["requests"].as_array().unwrap().iter().filter_map(Value::as_u64).sum();
        assert_eq!(total, 1);
    }

    #[test]
    fn first_results_by_confidence_and_layer() {
        let stats = Stats::default();
        stats.first_result(0.0, PlaceKind::Street);
        stats.first_result(0.95, PlaceKind::Address);
        stats.first_result(1.0, PlaceKind::Address);
        let s = stats.snapshot();
        let tenths: Vec<u64> = s["confidence"].as_array().unwrap().iter().filter_map(Value::as_u64).collect();
        assert_eq!((tenths[0], tenths[9], tenths.iter().sum::<u64>()), (1, 2, 3), "1.0 falls in the last tenth");
        assert_eq!(s["layers"], json!([{ "layer": "street", "count": 1 }, { "layer": "address", "count": 2 }]));
    }

    #[test]
    fn queries_lose_digits_links_and_length() {
        assert_eq!(anonymize("Calle Mayor 12 3º B 28013 Madrid"), "Calle Mayor # #º B # Madrid");
        assert_eq!(anonymize("  juan@example.com   600123456 plaza  "), "# plaza");
        assert_eq!(anonymize("https://x.es/a 1"), "#");
        assert_eq!(anonymize(&"a ".repeat(100)).chars().count(), SLOW_TEXT_CHARS - 1);
    }

    #[test]
    fn keeps_the_slowest_queries_of_the_last_hour() {
        let stats = Stats::default().keeping_slow(2);
        stats.slow_query_at(1_000, "calle mayor 1", "search", 9_000);
        stats.slow_query_at(1_001, "calle mayor 2", "search", 12_000);
        stats.slow_query_at(1_002, "plaza", "search", 3_000);
        stats.slow_query_at(1_003, "gran via", "autocomplete", 20_000);
        let texts = |s: &Stats, now| s.slow_snapshot(now).as_array().unwrap().iter().map(|q| q["text"].as_str().unwrap().to_owned()).collect::<Vec<_>>();
        assert_eq!(texts(&stats, 1_004), ["gran via", "calle mayor #"], "the same query without digits is one, with its slowest time");
        assert_eq!(stats.slow_snapshot(1_004)[1]["ms"].as_f64(), Some(12.0));
        assert!(texts(&stats, 1_004 + SLOW_KEPT_SECS).is_empty(), "an hour later they are gone");
        assert_eq!(Stats::default().slow_snapshot(0), Value::Null, "off by default");
    }

    #[test]
    fn empty_and_cut_short_searches() {
        let stats = Stats::default();
        stats.searched(false, false);
        stats.searched(true, false);
        stats.searched(true, true);
        let s = stats.snapshot();
        assert_eq!((s["searches"].as_u64(), s["empty"].as_u64(), s["cut_short"].as_u64()), (Some(3), Some(2), Some(1)));
    }
}

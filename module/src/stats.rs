//! What the geocoder has answered, for `v1/stats`: requests, errors and latency of every route,
//! the last hour minute by minute, and how many searches ended empty or cut by the time budget.
//! The host measures the same per route for `/metrics`, but a module cannot read it; these are
//! its own counters, a few atomics per request and one short lock for the minute it falls in.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// Upper bounds of the latency buckets, in microseconds; the last bucket has none.
const BOUNDS_US: [u64; 11] = [100, 250, 500, 1_000, 2_500, 5_000, 10_000, 25_000, 50_000, 100_000, 250_000];
const BUCKETS: usize = BOUNDS_US.len() + 1;
const MINUTES: usize = 60;

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
    /// Minutes since the epoch: a slot of an older minute is stale and starts over.
    at: u64,
    requests: u64,
    latency: [u64; BUCKETS],
}

pub struct Stats {
    started: u64,
    routes: [Route; ROUTES.len()],
    minutes: Mutex<[Minute; MINUTES]>,
    /// Searches and autocompletes answered, those with no result and those the time budget cut short.
    searches: AtomicU64,
    empty: AtomicU64,
    cut_short: AtomicU64,
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl Default for Stats {
    fn default() -> Self {
        Self {
            started: now_secs(),
            routes: Default::default(),
            minutes: Mutex::new([Minute::default(); MINUTES]),
            searches: AtomicU64::new(0),
            empty: AtomicU64::new(0),
            cut_short: AtomicU64::new(0),
        }
    }
}

impl Stats {
    pub fn record(&self, route: &str, status: u16, micros: u64) {
        self.record_at(now_secs() / 60, route, status, micros);
    }

    fn record_at(&self, minute: u64, route: &str, status: u16, micros: u64) {
        let Some(i) = ROUTES.iter().position(|r| *r == route) else { return };
        let b = bucket(micros);
        let r = &self.routes[i];
        r.requests.fetch_add(1, Ordering::Relaxed);
        if status >= 400 {
            r.errors.fetch_add(1, Ordering::Relaxed);
        }
        r.latency[b].fetch_add(1, Ordering::Relaxed);
        let mut minutes = self.minutes.lock().unwrap_or_else(|e| e.into_inner());
        let slot = &mut minutes[(minute % MINUTES as u64) as usize];
        if slot.at != minute {
            *slot = Minute { at: minute, ..Minute::default() };
        }
        slot.requests += 1;
        slot.latency[b] += 1;
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

    pub fn empty(&self) -> u64 {
        self.empty.load(Ordering::Relaxed)
    }

    pub fn cut_short(&self) -> u64 {
        self.cut_short.load(Ordering::Relaxed)
    }

    pub fn snapshot(&self) -> Value {
        self.snapshot_at(now_secs() / 60)
    }

    fn snapshot_at(&self, now: u64) -> Value {
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
        let minutes = *self.minutes.lock().unwrap_or_else(|e| e.into_inner());
        // The last hour, oldest first; a minute with no requests counts zero.
        let series: Vec<Minute> = (0..MINUTES as u64)
            .map(|ago| {
                let at = now.saturating_sub(MINUTES as u64 - 1 - ago);
                let slot = minutes[(at % MINUTES as u64) as usize];
                if slot.at == at { slot } else { Minute { at, ..Minute::default() } }
            })
            .collect();
        json!({
            "since_unix": self.started,
            "routes": routes,
            "searches": self.searches.load(Ordering::Relaxed),
            "empty": self.empty(),
            "cut_short": self.cut_short(),
            "last_hour": {
                "end_unix": (now + 1) * 60,
                "requests": series.iter().map(|m| m.requests).collect::<Vec<_>>(),
                "p99_ms": series.iter().map(|m| quantile_ms(&m.latency, 0.99)).collect::<Vec<_>>(),
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
        stats.record_at(1000, "v1/search", 200, 800);
        stats.record_at(1000, "v1/search", 400, 900);
        stats.record_at(1010, "v1/reverse", 200, 200);
        stats.record_at(1010, "nope", 200, 200);
        let s = stats.snapshot_at(1010);
        let search = &s["routes"][0];
        assert_eq!((search["route"].as_str(), search["requests"].as_u64(), search["errors"].as_u64()), (Some("v1/search"), Some(2), Some(1)));
        assert_eq!(search["p99_ms"].as_f64(), Some(1.0));
        let hour = s["last_hour"]["requests"].as_array().unwrap();
        assert_eq!(hour.len(), MINUTES);
        assert_eq!((hour[MINUTES - 1].as_u64(), hour[MINUTES - 11].as_u64(), hour[0].as_u64()), (Some(1), Some(2), Some(0)));
    }

    #[test]
    fn a_minute_an_hour_old_starts_over() {
        let stats = Stats::default();
        stats.record_at(1000, "v1/search", 200, 800);
        stats.record_at(1000 + MINUTES as u64, "v1/search", 200, 800);
        let s = stats.snapshot_at(1000 + MINUTES as u64);
        let total: u64 = s["last_hour"]["requests"].as_array().unwrap().iter().filter_map(Value::as_u64).sum();
        assert_eq!(total, 1);
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

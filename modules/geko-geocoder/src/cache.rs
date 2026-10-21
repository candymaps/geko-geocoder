//! Cache of search and autocomplete answers (PROJECT.md §10): many clients type the same
//! beginnings ("Cal", "Calle"), and an answer is the same until the bundle changes.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

const SHARDS: usize = 16;

/// Two generations per shard: a full current one becomes the previous, and an entry read from
/// the previous moves back to the current. The least used half goes at once, without a list to
/// keep in order on every read.
#[derive(Default)]
struct Generations {
    current: HashMap<String, std::sync::Arc<str>>,
    previous: HashMap<String, std::sync::Arc<str>>,
}

pub struct Cache {
    shards: Vec<Mutex<Generations>>,
    per_generation: usize,
    pub hits: AtomicU64,
    pub misses: AtomicU64,
}

impl Cache {
    /// About `entries` answers kept; 0 keeps none.
    pub fn new(entries: usize) -> Self {
        Self {
            shards: (0..SHARDS).map(|_| Mutex::new(Generations::default())).collect(),
            per_generation: entries / SHARDS / 2,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    pub fn enabled(&self) -> bool {
        self.per_generation > 0
    }

    fn shard(&self, key: &str) -> std::sync::MutexGuard<'_, Generations> {
        let mut h = DefaultHasher::new();
        key.hash(&mut h);
        self.shards[h.finish() as usize % SHARDS].lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn get(&self, key: &str) -> Option<std::sync::Arc<str>> {
        let mut g = self.shard(key);
        let found = match g.current.get(key) {
            Some(v) => Some(v.clone()),
            None => g.previous.remove(key).inspect(|v| {
                g.current.insert(key.to_owned(), v.clone());
            }),
        };
        let counter = if found.is_some() { &self.hits } else { &self.misses };
        counter.fetch_add(1, Ordering::Relaxed);
        found
    }

    pub fn put(&self, key: String, value: std::sync::Arc<str>) {
        let mut g = self.shard(&key);
        if g.current.len() >= self.per_generation {
            g.previous = std::mem::take(&mut g.current);
        }
        g.current.insert(key, value);
    }

    pub fn clear(&self) {
        for s in &self.shards {
            *s.lock().unwrap_or_else(|e| e.into_inner()) = Generations::default();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_what_is_read_and_drops_the_rest() {
        let cache = Cache::new(SHARDS * 2 * 2);
        let key = |i: usize| format!("k{i}");
        for i in 0..1000 {
            cache.put(key(i), format!("v{i}").into());
            assert_eq!(cache.get(&key(0)).as_deref(), Some("v0"), "read on every round, kept");
        }
        assert!(cache.get(&key(1)).is_none(), "never read again, dropped");
        cache.clear();
        assert!(cache.get(&key(0)).is_none());
    }

    #[test]
    fn off_with_no_entries() {
        assert!(!Cache::new(0).enabled());
    }
}

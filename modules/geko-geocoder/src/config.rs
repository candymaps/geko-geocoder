//! `geko-geocoder.toml`: the configuration of the geocoder, beside `geko.toml` (docs/MODULES_PLAN.md).
//! The host keeps the scope, the data directory and how data are downloaded; this file, the
//! sources, where the country packs come from and how the geocoder answers.

use std::collections::BTreeMap;

use geko_core::country::CountryCode;
use geko_core::{GekoError, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Each provider parses its own section; only `enabled` is common.
    pub sources: BTreeMap<String, toml::Table>,
    pub packs: PacksConfig,
    pub serve: ServeConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PacksConfig {
    /// Where the country packs are published: `<registry>/<country>:format-<n>` (PROJECT.md §2.4).
    pub registry: String,
}

impl Default for PacksConfig {
    fn default() -> Self {
        Self { registry: geko_country_pack::registry::DEFAULT_REGISTRY.to_owned() }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServeConfig {
    /// Countries this node serves; empty means every country of the bundle.
    pub countries: Vec<String>,
    /// Load hot index files into RAM at startup (PROJECT.md §9.6).
    pub preload: bool,
    /// Time budget of a search and of an autocomplete request, in milliseconds (0: none). Past
    /// it the engine returns what it found; its likeliest reading always completes.
    pub search_budget_ms: u64,
    pub autocomplete_budget_ms: u64,
    /// Requests slower than this, in milliseconds, are logged with their query (0: none).
    pub slow_query_ms: u64,
    /// Results with the same name closer than this, in metres, are merged in the answer
    /// (PROJECT.md §6.5.6); 0 merges only the same id.
    pub same_entity_m: f64,
    /// Most requests in one `v1/batch`.
    pub batch_max: usize,
    /// Search and autocomplete answers kept for repeated queries (0: none), cleared when the
    /// bundle changes.
    pub cache_entries: usize,
}

impl Default for ServeConfig {
    fn default() -> Self {
        Self {
            countries: Vec::new(),
            preload: true,
            search_budget_ms: 15,
            autocomplete_budget_ms: 5,
            slow_query_ms: 50,
            same_entity_m: 100.0,
            batch_max: 100,
            cache_entries: 0,
        }
    }
}

impl Config {
    /// `file` names the file in errors.
    pub fn parse(text: &str, file: &str) -> Result<Self> {
        toml::from_str(text).map_err(|e| GekoError::Config(if file.is_empty() { e.to_string() } else { format!("{file}: {e}") }))
    }

    pub fn served_countries(&self) -> Result<Vec<CountryCode>> {
        self.serve.countries.iter().map(|c| CountryCode::new(c).map_err(|e| GekoError::Config(e.to_string()))).collect()
    }

    pub fn source_enabled(&self, name: &str) -> Result<bool> {
        match self.sources.get(name).and_then(|t| t.get("enabled")) {
            None => Ok(true),
            Some(toml::Value::Boolean(b)) => Ok(*b),
            Some(other) => Err(GekoError::Config(format!("sources.{name}.enabled must be a boolean, got {other}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let c = Config::parse("", "").unwrap();
        assert!(c.serve.preload);
        assert_eq!((c.serve.search_budget_ms, c.serve.autocomplete_budget_ms), (15, 5));
        assert_eq!(c.serve.cache_entries, 0);
        assert_eq!(c.packs.registry, geko_country_pack::registry::DEFAULT_REGISTRY);
    }

    #[test]
    fn an_unknown_option_names_the_file() {
        let err = Config::parse("[serve]\nlisten = \"0.0.0.0:80\"\n", "/etc/geko/geko-geocoder.toml").unwrap_err().to_string();
        assert!(err.contains("/etc/geko/geko-geocoder.toml") && err.contains("listen"), "{err}");
        assert!(Config::parse("[data]\noffline = true\n", "").is_err(), "the host's section");
    }

    #[test]
    fn source_sections() {
        let c = Config::parse("[sources.osm]\nmirror = \"auto\"\n\n[sources.cartociudad]\nenabled = false\n", "").unwrap();
        assert!(c.source_enabled("osm").unwrap());
        assert!(!c.source_enabled("cartociudad").unwrap());
        let bad = Config::parse("[sources.osm]\nenabled = \"yes\"\n", "").unwrap();
        assert!(bad.source_enabled("osm").is_err());
    }
}

use std::collections::BTreeMap;
use std::sync::Arc;

use geko_build::selection::{SkipReason, SkippedSource};
use geko_core::country::{CountryCode, RegionCode};
use geko_core::model::SourceId;
use geko_core::ports::SourceProvider;
use geko_core::scope::Scope;
use geko_core::{GekoError, Result};

use crate::config::Config;

/// Code version of a workspace crate and the workspace crates it depends on (see `build.rs`).
macro_rules! code {
    ($krate:literal) => {
        env!(concat!("GEKO_CODE_", $krate))
    };
}

/// The packs of the countries of the scope (PROJECT.md §2.4).
pub type Packs = BTreeMap<CountryCode, &'static geko_country_pack::CountryPack>;

/// The packs of the countries of `scope`, read from the folder of the packs.
pub fn packs(scope: &Scope) -> Result<Packs> {
    scope.countries().map(|c| Ok((c, geko_country_pack::get(c)?))).collect()
}

pub struct Factory {
    /// Creates the provider from its `[sources.<name>]` section and what the packs of the scope
    /// tell it about their countries.
    pub create: fn(&toml::Table, &Packs) -> Result<Arc<dyn SourceProvider>>,
    /// Code version of the provider: its staging reruns when it changes (PROJECT.md §8.3).
    pub code: &'static str,
    /// The base of every country (OSM): always used, and `enabled = false` is an error.
    pub required: bool,
}

/// Source providers compiled into this binary, by configuration name.
pub fn registry() -> BTreeMap<&'static str, Factory> {
    #[allow(unused_mut)]
    let mut registry: BTreeMap<&'static str, Factory> = BTreeMap::new();
    #[cfg(feature = "source-osm")]
    registry.insert(source_osm::ID, Factory {
        create: |table, packs| {
            let config: source_osm::OsmConfig = parse_section(source_osm::ID, table)?;
            let countries = packs
                .iter()
                .map(|(&country, pack)| {
                    let extracts = match pack.sources.get(source_osm::ID) {
                        Some(table) => parse_pack_section(country, source_osm::ID, table)?,
                        None => source_osm::CountryExtracts::default(),
                    };
                    Ok((country, source_osm::Country { extracts, regions: pack.regions.clone() }))
                })
                .collect::<Result<_>>()?;
            Ok(Arc::new(source_osm::OsmSource::new(config, countries)))
        },
        code: code!("SOURCE_OSM"),
        required: true,
    });
    #[cfg(feature = "source-overture")]
    registry.insert(source_overture::ID, Factory {
        create: |table, packs| {
            let config: source_overture::OvertureConfig = parse_section(source_overture::ID, table)?;
            let regions = packs.iter().map(|(&country, pack)| (country, pack.regions.clone())).collect();
            Ok(Arc::new(source_overture::OvertureSource::new(config, regions)))
        },
        code: code!("SOURCE_OVERTURE"),
        required: false,
    });
    registry
}

/// A generic provider for the sources that country packs declare (`sources/<name>.toml` with
/// `kind = "<kind>"`): it is created from the declaration, with the `[sources.<name>]` section of
/// `geko.toml` over it.
pub struct Kind {
    pub create: CreateDeclared,
    /// Code version of the provider; that of a source adds the hash of its declaration.
    pub code: &'static str,
}

/// Creates the source named by its declaration in the pack of a country.
pub type CreateDeclared = fn(&str, CountryCode, &toml::Table, &geko_country_pack::CountryPack) -> Result<Arc<dyn SourceProvider>>;

/// The generic providers compiled into this binary, by kind.
pub fn kinds() -> BTreeMap<&'static str, Kind> {
    #[allow(unused_mut)]
    let mut kinds: BTreeMap<&'static str, Kind> = BTreeMap::new();
    #[cfg(feature = "source-ogc-features")]
    kinds.insert(source_ogc_features::KIND, Kind {
        create: |name, country, table, pack| {
            let config: source_ogc_features::OgcFeaturesConfig = parse_pack_section(country, name, table)?;
            Ok(Arc::new(source_ogc_features::OgcFeaturesSource::new(name, country, config, pack.regions.clone())?))
        },
        code: code!("SOURCE_OGC_FEATURES"),
    });
    kinds
}

/// The scope with every country split into its fusion partitions (PROJECT.md §6.5.8): the
/// regions of its pack that contain no other (the provinces of Spain), so that staging and fusion
/// hold one at a time whatever the size of the country. A country whose pack lists no regions
/// stays as configured.
pub fn partitioned(scope: &Scope, packs: &Packs) -> Result<Scope> {
    let mut out = Scope::default();
    for country in scope.countries() {
        let regions: Vec<RegionCode> = scope.partitions().into_iter().filter(|p| p.country == country).filter_map(|p| p.region).collect();
        let partitions = match packs.get(&country).map(|p| &p.regions) {
            Some(table) if !table.is_empty() => {
                let parts = if regions.is_empty() {
                    table.partitions().collect()
                } else {
                    regions.iter().map(|r| table.partitions_in(r)).collect::<Result<Vec<_>>>()?.concat()
                };
                parts.into_iter().map(|p| p.code.clone()).collect()
            }
            _ => regions,
        };
        out.add(country, partitions)?;
    }
    Ok(out)
}

/// Text rules of a country, from its pack (PROJECT.md §2.4).
pub fn normalizer(country: CountryCode) -> Result<Box<dyn geko_core::ports::TextNormalizer>> {
    Ok(Box::new(geko_country_pack::get(country)?.normalizer()))
}

/// The OSM admin levels of a country, from its pack.
pub fn admin_levels(country: CountryCode) -> Result<geko_core::model::AdminLevels> {
    Ok(geko_country_pack::get(country)?.text.admin_levels.clone())
}

pub fn geocode_builder() -> Result<Box<dyn geko_core::ports::GeocodeIndexBuilder>> {
    #[cfg(feature = "geocode-fst")]
    return Ok(Box::new(geocode_fst::FstIndexBuilder));
    #[allow(unreachable_code)]
    Err(GekoError::Config("no geocode index provider is compiled into this binary".into()))
}

pub fn geocode_engine(
    dir: &std::path::Path,
    normalizer: Box<dyn geko_core::ports::TextNormalizer>,
    preload: bool,
) -> Result<Box<dyn geko_core::ports::GeocodeEngine>> {
    #[cfg(feature = "geocode-fst")]
    return Ok(Box::new(geocode_fst::FstEngine::open(dir, normalizer, preload)?));
    #[allow(unreachable_code)]
    {
        let _ = (dir, normalizer, preload);
        Err(GekoError::Config("no geocode index provider is compiled into this binary".into()))
    }
}

pub fn build_reverse(canonical: &std::path::Path, country: geko_core::country::CountryCode, dir: &std::path::Path) -> Result<u64> {
    #[cfg(feature = "reverse-rstar")]
    return reverse_rstar::build(canonical, country, dir);
    #[allow(unreachable_code)]
    {
        let _ = (canonical, country, dir);
        Err(GekoError::Config("no reverse index provider is compiled into this binary".into()))
    }
}

pub fn reverse_index(dir: &std::path::Path) -> Result<Box<dyn geko_core::ports::ReverseIndex>> {
    #[cfg(feature = "reverse-rstar")]
    return Ok(Box::new(reverse_rstar::RstarReverse::open(dir)?));
    #[allow(unreachable_code)]
    {
        let _ = dir;
        Err(GekoError::Config("no reverse index provider is compiled into this binary".into()))
    }
}

/// Opens a bundle segment with the providers compiled into this binary. Each provider checks
/// that the artifacts were written in a format it reads.
pub fn segment_opener(preload: bool) -> crate::serve::SegmentOpener {
    Arc::new(move |dir: &std::path::Path, segment: &geko_core::segment::Segment| {
        let country = geko_core::country::CountryCode::new(&segment.country)?;
        // The pack the segment was built with, which the bundle carries: on a machine that only
        // serves, there is no other. Loaded on every reload, since a new bundle may bring another.
        let pack: &geko_country_pack::CountryPack = match dir.join("pack") {
            bundled if bundled.is_dir() => Box::leak(Box::new(geko_country_pack::CountryPack::load(&bundled)?)),
            _ => geko_country_pack::get(country)?,
        };
        Ok(crate::serve::Segment {
            country,
            engine: geocode_engine(&dir.join("geocode"), Box::new(pack.normalizer()), preload)?,
            reverse: reverse_index(&dir.join("reverse"))?,
            parser: Box::new(pack.parser()),
        })
    })
}

/// Code version of the fusion and of the canonical dataset it writes.
pub const FUSION_CODE: &str = code!("GEKO_BUILD");

/// Code version of the text rules of `country`, their code and the files of its pack: fusion
/// and the geocode index use them.
pub fn normalizer_code(country: CountryCode) -> Result<String> {
    Ok(format!("{} {}", code!("TEXT_RULES"), geko_country_pack::get(country)?.code))
}

pub fn geocode_code() -> &'static str {
    #[cfg(feature = "geocode-fst")]
    return code!("GEOCODE_FST");
    #[allow(unreachable_code)]
    ""
}

pub fn reverse_code() -> &'static str {
    #[cfg(feature = "reverse-rstar")]
    return code!("REVERSE_RSTAR");
    #[allow(unreachable_code)]
    ""
}

pub fn index_formats() -> (geko_core::segment::ProviderFormat, geko_core::segment::ProviderFormat) {
    let format = |provider: &str, version: u32| geko_core::segment::ProviderFormat { provider: provider.into(), version };
    #[cfg(all(feature = "geocode-fst", feature = "reverse-rstar"))]
    return (format(geocode_fst::PROVIDER, geocode_fst::VERSION), format(reverse_rstar::PROVIDER, reverse_rstar::VERSION));
    #[allow(unreachable_code)]
    (format("none", 0), format("none", 0))
}

/// Query parser of a country, from its pack.
pub fn parser(country: CountryCode) -> Result<Box<dyn geko_core::ports::AddressParser>> {
    Ok(Box::new(geko_country_pack::get(country)?.parser()))
}

/// Deserializes a `[sources.<name>]` section into the provider's own config type.
/// `enabled` is common to every source and handled here, so providers never see it.
#[allow(dead_code)]
fn parse_section<T: serde::de::DeserializeOwned>(name: &str, table: &toml::Table) -> Result<T> {
    let mut table = table.clone();
    table.remove("enabled");
    toml::Value::Table(table).try_into().map_err(|e| GekoError::Config(format!("[sources.{name}]: {e}")))
}

/// Deserializes what the pack of `country` tells a source (`sources/<name>.toml`).
#[allow(dead_code)]
fn parse_pack_section<T: serde::de::DeserializeOwned>(country: CountryCode, name: &str, table: &toml::Table) -> Result<T> {
    toml::Value::Table(table.clone())
        .try_into()
        .map_err(|e| GekoError::Config(format!("pack of {country}, sources/{name}.toml: {e}")))
}

pub struct Instantiated {
    pub enabled: Vec<Arc<dyn SourceProvider>>,
    pub skipped: Vec<SkippedSource>,
    /// Code version of each enabled source, by name.
    pub codes: BTreeMap<String, String>,
}

/// A source that the pack of a country of the scope declares.
struct Declared<'a> {
    name: &'a str,
    country: CountryCode,
    declaration: &'a toml::Table,
    pack: &'static geko_country_pack::CountryPack,
}

/// Instantiates every source compiled into this binary and every source that the packs of the
/// scope declare. A source is used by default, and its `[sources.<name>]` section is optional:
/// without one, the provider's defaults (or the declaration) apply; with one, its fields replace
/// them, and `enabled = false` turns the source off. Which sources cover the countries of the
/// scope is decided afterwards, from each provider's coverage. A required source (OSM) cannot be
/// disabled. A section naming no source is a configuration error, unless the pack of a country
/// outside the scope declares it: the same `geko.toml` then serves another scope.
pub fn instantiate(config: &Config, registry: &BTreeMap<&'static str, Factory>, packs: &Packs) -> Result<Instantiated> {
    let mut declared: Vec<Declared> = Vec::new();
    for (&country, pack) in packs {
        for (name, declaration) in pack.sources.iter().filter(|(_, t)| t.contains_key("kind")) {
            if registry.contains_key(name.as_str()) || declared.iter().any(|d| d.name == name) {
                return Err(GekoError::Config(format!(
                    "the pack of {country} declares the source `{name}`, which another source already is"
                )));
            }
            declared.push(Declared { name, country, declaration, pack });
        }
    }
    for (name, section) in &config.sources {
        if section.contains_key("max_age_days") {
            return Err(GekoError::Config(format!(
                "[sources.{name}] max_age_days: it is no longer an option of a source; it is [data] max_age_days, for all of them at once"
            )));
        }
        if registry.contains_key(name.as_str()) || declared.iter().any(|d| d.name == name) {
            continue;
        }
        if geko_country_pack::declares(name) {
            tracing::warn!("[sources.{name}] is ignored: {name} is a source of no country of the scope");
            continue;
        }
        let mut known: Vec<&str> = registry.keys().copied().chain(declared.iter().map(|d| d.name)).collect();
        known.sort();
        return Err(GekoError::Config(format!(
            "unknown source `{name}`: not a source of this binary or of the packs of the scope (available: {})",
            if known.is_empty() { "none".to_string() } else { known.join(", ") }
        )));
    }
    let mut enabled = Vec::new();
    let mut skipped = Vec::new();
    let mut codes = BTreeMap::new();
    let no_section = toml::Table::new();
    for (&name, factory) in registry {
        if factory.required && !config.source_enabled(name)? {
            return Err(GekoError::Config(format!(
                "[sources.{name}] enabled = false: {name} is the base of every country and cannot be disabled; its section only changes its options"
            )));
        }
        if config.source_enabled(name)? {
            enabled.push((factory.create)(config.sources.get(name).unwrap_or(&no_section), packs)?);
            codes.insert(name.to_owned(), factory.code.to_owned());
        } else {
            skipped.push(SkippedSource { id: SourceId(name.to_owned()), reason: SkipReason::Disabled });
        }
    }
    let kinds = kinds();
    for d in declared {
        if !config.source_enabled(d.name)? {
            skipped.push(SkippedSource { id: SourceId(d.name.to_owned()), reason: SkipReason::Disabled });
            continue;
        }
        let kind = d.declaration.get("kind").and_then(|k| k.as_str()).unwrap_or_default();
        let Some(provider) = kinds.get(kind) else {
            return Err(GekoError::Config(format!(
                "the pack of {} declares the source `{}` of kind `{kind}`, which this binary has no provider for",
                d.country, d.name
            )));
        };
        let mut table = d.declaration.clone();
        for (key, value) in config.sources.get(d.name).unwrap_or(&no_section).iter().filter(|(k, _)| *k != "enabled") {
            table.insert(key.clone(), value.clone());
        }
        enabled.push((provider.create)(d.name, d.country, &table, d.pack)?);
        let declaration_code = d.pack.source_codes.get(d.name).map(String::as_str).unwrap_or_default();
        codes.insert(d.name.to_owned(), format!("{} {declaration_code}", provider.code));
    }
    Ok(Instantiated { enabled, skipped, codes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use geko_core::model::License;
    use geko_core::ports::{DatasetRequest, Downloader, FetchReport, RawDir, SourceStats, StagingDir};
    use geko_core::scope::{Coverage, Partition, Scope};

    struct Fake;

    impl SourceProvider for Fake {
        fn id(&self) -> SourceId {
            SourceId("fake".into())
        }
        fn license(&self) -> License {
            License { name: "test".into(), attribution: "test".into(), url: None }
        }
        fn coverage(&self) -> Coverage {
            Coverage::Global
        }
        fn plan(&self, _: &Scope, _: &dyn Downloader, _: &RawDir) -> Result<Vec<DatasetRequest>> {
            Ok(vec![])
        }
        fn fetch(&self, _: &Scope, _: &dyn Downloader, _: &RawDir) -> Result<FetchReport> {
            Ok(FetchReport::default())
        }
        fn stage(&self, _: &Partition, _: &RawDir, _: &StagingDir) -> Result<SourceStats> {
            Ok(SourceStats::default())
        }
    }

    fn fake_registry() -> BTreeMap<&'static str, Factory> {
        let factory = Factory { create: |_, _| Ok(Arc::new(Fake)), code: "test", required: false };
        BTreeMap::from([("fake", factory)])
    }

    #[test]
    fn a_required_source_cannot_be_disabled() {
        let registry = BTreeMap::from([("base", Factory { create: |_, _| Ok(Arc::new(Fake)), code: "test", required: true })]);
        assert_eq!(instantiate(&Config::parse("", "").unwrap(), &registry, &Packs::new()).unwrap().enabled.len(), 1);
        assert_eq!(instantiate(&Config::parse("[sources.base]\nenabled = true\n", "").unwrap(), &registry, &Packs::new()).unwrap().enabled.len(), 1);
        let err = instantiate(&Config::parse("[sources.base]\nenabled = false\n", "").unwrap(), &registry, &Packs::new()).err().unwrap();
        assert!(err.to_string().contains("cannot be disabled"), "{err}");
    }

    #[cfg(feature = "source-osm")]
    #[test]
    fn osm_mirror_overrides_auto() {
        let parse = |text: &str| -> source_osm::OsmConfig {
            let config = Config::parse(text, "").unwrap();
            parse_section("osm", config.sources.get("osm").unwrap_or(&toml::Table::new())).unwrap()
        };
        assert_eq!(parse("").mirror, "auto");
        assert_eq!(parse("[sources.osm]\nmirror = \"https://mirror.example/\"\n").mirror, "https://mirror.example/");
    }

    #[test]
    fn sources_are_used_by_default_unless_disabled() {
        let config = Config::parse("[sources.fake]\nenabled = false\n", "").unwrap();
        let result = instantiate(&config, &fake_registry(), &Packs::new()).unwrap();
        assert!(result.enabled.is_empty());
        assert_eq!(result.skipped, [SkippedSource { id: SourceId("fake".into()), reason: SkipReason::Disabled }]);

        for text in ["[sources.fake]\n", ""] {
            let result = instantiate(&Config::parse(text, "").unwrap(), &fake_registry(), &Packs::new()).unwrap();
            assert_eq!(result.enabled.len(), 1, "{text:?}: a source needs no section");
            assert!(result.skipped.is_empty());
        }
    }

    #[cfg(feature = "source-ogc-features")]
    #[test]
    fn a_declared_source_takes_the_section_over_its_declaration() {
        let es = CountryCode::new("ES").unwrap();
        let packs = Packs::from([(es, geko_country_pack::repo("ES"))]);
        let config = Config::parse("[sources.cartociudad]\nmax_unchanged_days = 90\n", "").unwrap();
        let result = instantiate(&config, &fake_registry(), &packs).unwrap();
        let ids: Vec<String> = result.enabled.iter().map(|s| s.id().0).collect();
        assert_eq!(ids, ["fake", "cartociudad"]);
        assert!(result.codes["cartociudad"].contains(&packs[&es].source_codes["cartociudad"]));

        let off = Config::parse("[sources.cartociudad]\nenabled = false\n", "").unwrap();
        assert_eq!(instantiate(&off, &fake_registry(), &packs).unwrap().enabled.len(), 1);
        let bad = Config::parse("[sources.cartociudad]\nmax_age = 7\n", "").unwrap();
        assert!(instantiate(&bad, &fake_registry(), &packs).is_err());
        let moved = Config::parse("[sources.cartociudad]\nmax_age_days = 7\n", "").unwrap();
        let error = instantiate(&moved, &fake_registry(), &packs).err().unwrap().to_string();
        assert!(error.contains("[data] max_age_days"), "{error}");
    }

    #[derive(Debug, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct SectionConfig {
        mirror: String,
    }

    #[test]
    fn provider_sections_never_see_the_enabled_key() {
        let config = Config::parse("[sources.x]\nenabled = true\nmirror = \"m\"\n", "").unwrap();
        let parsed: SectionConfig = parse_section("x", &config.sources["x"]).unwrap();
        assert_eq!(parsed.mirror, "m");
        let bad = Config::parse("[sources.x]\nmirorr = \"m\"\n", "").unwrap();
        let err = parse_section::<SectionConfig>("x", &bad.sources["x"]).unwrap_err();
        assert!(err.to_string().contains("[sources.x]"), "{err}");
    }

    #[test]
    fn unknown_source_is_an_error() {
        let config = Config::parse("[sources.osmm]\n", "").unwrap();
        let err = instantiate(&config, &fake_registry(), &Packs::new()).err().unwrap();
        assert!(err.to_string().contains("unknown source `osmm`"), "{err}");
        assert!(err.to_string().contains("available: fake"), "{err}");
    }
}

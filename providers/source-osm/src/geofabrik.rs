//! Choice of Geofabrik extracts for a scope (PROJECT.md §7.4).

use std::collections::{BTreeMap, BTreeSet};

use geko_core::country::{CountryCode, RegionCode};
use geko_core::regions::Regions;
use geko_core::scope::Partition;
use geko_core::{GekoError, Result};
use serde::Deserialize;

pub const DEFAULT_BASE: &str = "https://download.geofabrik.de";
pub const INDEX_FILE: &str = "index-v1-nogeom.json";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Extract {
    pub id: String,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(rename = "iso3166-1:alpha2", default)]
    pub iso3166_1: Vec<String>,
    #[serde(rename = "iso3166-2", default)]
    pub iso3166_2: Vec<String>,
    pub urls: Urls,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Urls {
    pub pbf: String,
}

#[derive(Deserialize)]
struct Index {
    features: Vec<Feature>,
}

#[derive(Deserialize)]
struct Feature {
    properties: Extract,
}

pub struct Catalog {
    extracts: Vec<Extract>,
}

/// What a country pack tells the OSM source (`sources/osm.toml`), for a country whose regions
/// carry no ISO 3166-2 code in the Geofabrik index, or that is split across extracts.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CountryExtracts {
    /// The extracts whose union is the whole country; empty: those the index gives its ISO code.
    #[serde(default)]
    pub country: Vec<String>,
    /// The extract of each region, by ISO 3166-2 code. A region not listed takes that of the
    /// region that contains it.
    #[serde(default)]
    pub regions: BTreeMap<String, String>,
}

/// What the OSM source knows of a country from its pack.
#[derive(Debug, Clone, Default)]
pub struct Country {
    pub extracts: CountryExtracts,
    pub regions: Regions,
}

pub type Countries = BTreeMap<CountryCode, Country>;

impl Catalog {
    pub fn parse(json: &[u8]) -> Result<Self> {
        let index: Index = serde_json::from_slice(json)
            .map_err(|e| GekoError::Data(format!("invalid Geofabrik index: {e}")))?;
        Ok(Self { extracts: index.features.into_iter().map(|f| f.properties).collect() })
    }

    pub fn get(&self, id: &str) -> Option<&Extract> {
        self.extracts.iter().find(|e| e.id == id)
    }

    /// Extracts whose union covers the partition.
    pub fn for_partition(&self, partition: &Partition, countries: &Countries) -> Result<Vec<&Extract>> {
        let ids = match &partition.region {
            Some(region) => self.region_ids(region, countries)?,
            None => self.country_ids(partition.country, countries)?,
        };
        ids.iter()
            .map(|id| self.get(id).ok_or_else(|| GekoError::Data(format!("Geofabrik extract `{id}` not found in the index"))))
            .collect()
    }

    fn country_ids(&self, country: CountryCode, countries: &Countries) -> Result<Vec<String>> {
        if let Some(c) = countries.get(&country)
            && !c.extracts.country.is_empty()
        {
            return Ok(c.extracts.country.clone());
        }
        let matching: Vec<&Extract> =
            self.extracts.iter().filter(|e| e.iso3166_1.iter().any(|c| c == country.as_str())).collect();
        let matching_ids: BTreeSet<&str> = matching.iter().map(|e| e.id.as_str()).collect();
        let top: Vec<String> = matching
            .iter()
            .filter(|e| e.parent.as_deref().is_none_or(|p| !matching_ids.contains(p)))
            .map(|e| e.id.clone())
            .collect();
        if top.is_empty() {
            return Err(GekoError::Data(format!("no Geofabrik extract for country {country}")));
        }
        Ok(top)
    }

    /// The extract the pack names for the region or the nearest region that contains it, else
    /// those the index gives the ISO code of either, else the whole country.
    fn region_ids(&self, region: &RegionCode, countries: &Countries) -> Result<Vec<String>> {
        let country = countries.get(&region.country());
        let chain: Vec<&RegionCode> = match country {
            Some(c) if c.regions.get(region).is_some() => c.regions.ancestors(region).map(|r| &r.code).collect(),
            _ => vec![region],
        };
        if let Some(c) = country
            && let Some(id) = chain.iter().find_map(|code| c.extracts.regions.get(code.as_str()))
        {
            return Ok(vec![id.clone()]);
        }
        for code in &chain {
            let by_iso: Vec<String> = self
                .extracts
                .iter()
                .filter(|e| e.iso3166_2.iter().any(|c| c.eq_ignore_ascii_case(code.as_str())))
                .map(|e| e.id.clone())
                .collect();
            if !by_iso.is_empty() {
                return Ok(by_iso);
            }
        }
        tracing::warn!(%region, "no regional Geofabrik extract; using the whole country");
        self.country_ids(region.country(), countries)
    }
}

pub fn file_name(extract: &Extract) -> String {
    format!("{}-latest.osm.pbf", extract.id)
}

/// Replaces the Geofabrik host with a mirror, keeping the path.
pub fn with_base(url: &str, base: &str) -> String {
    match url.strip_prefix(DEFAULT_BASE) {
        Some(path) => format!("{}{path}", base.trim_end_matches('/')),
        None => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INDEX: &str = r#"{ "type": "FeatureCollection", "features": [
        { "type": "Feature", "properties": { "id": "europe", "urls": { "pbf": "https://download.geofabrik.de/europe-latest.osm.pbf" } } },
        { "type": "Feature", "properties": { "id": "spain", "parent": "europe", "iso3166-1:alpha2": ["ES"],
            "urls": { "pbf": "https://download.geofabrik.de/europe/spain-latest.osm.pbf" } } },
        { "type": "Feature", "properties": { "id": "cataluna", "parent": "spain",
            "urls": { "pbf": "https://download.geofabrik.de/europe/spain/cataluna-latest.osm.pbf" } } },
        { "type": "Feature", "properties": { "id": "canary-islands", "parent": "africa",
            "urls": { "pbf": "https://download.geofabrik.de/africa/canary-islands-latest.osm.pbf" } } },
        { "type": "Feature", "properties": { "id": "france", "parent": "europe", "iso3166-1:alpha2": ["FR"],
            "urls": { "pbf": "https://download.geofabrik.de/europe/france-latest.osm.pbf" } } },
        { "type": "Feature", "properties": { "id": "guyane", "parent": "france", "iso3166-1:alpha2": ["GF"], "iso3166-2": ["FR-GF"],
            "urls": { "pbf": "https://download.geofabrik.de/europe/france/guyane-latest.osm.pbf" } } },
        { "type": "Feature", "properties": { "id": "ile-de-france", "parent": "france", "iso3166-1:alpha2": ["FR"],
            "urls": { "pbf": "https://download.geofabrik.de/europe/france/ile-de-france-latest.osm.pbf" } } }
    ]}"#;

    fn catalog() -> Catalog {
        Catalog::parse(INDEX.as_bytes()).unwrap()
    }

    /// Spain as its pack describes it: regions without ISO codes in the index, and the Canary
    /// Islands under Africa.
    fn countries() -> Countries {
        let es = CountryCode::new("ES").unwrap();
        let region = |code: &str, parent: Option<&str>| geko_core::regions::Region {
            code: RegionCode::new(code).unwrap(),
            name: code.into(),
            parent: parent.map(|p| RegionCode::new(p).unwrap()),
            attributes: BTreeMap::new(),
            bbox: None,
        };
        let regions = Regions::new(
            es,
            vec![region("ES-CT", None), region("ES-B", Some("ES-CT")), region("ES-CN", None), region("ES-TF", Some("ES-CN"))],
        )
        .unwrap();
        let extracts = CountryExtracts {
            country: vec!["spain".into(), "canary-islands".into()],
            regions: BTreeMap::from([("ES-CT".into(), "cataluna".into()), ("ES-CN".into(), "canary-islands".into())]),
        };
        BTreeMap::from([(es, Country { extracts, regions })])
    }

    fn ids(partition: Partition) -> Vec<String> {
        catalog().for_partition(&partition, &countries()).unwrap().into_iter().map(|e| e.id.clone()).collect()
    }

    fn country(code: &str) -> Partition {
        Partition { country: CountryCode::new(code).unwrap(), region: None }
    }

    fn region(code: &str) -> Partition {
        let r = RegionCode::new(code).unwrap();
        Partition { country: r.country(), region: Some(r) }
    }

    #[test]
    fn spanish_province_uses_its_community_extract() {
        assert_eq!(ids(region("ES-B")), ["cataluna"]);
        assert_eq!(ids(region("ES-CT")), ["cataluna"]);
    }

    #[test]
    fn spain_as_a_whole_includes_the_canary_islands() {
        assert_eq!(ids(country("ES")), ["spain", "canary-islands"]);
        assert_eq!(ids(region("ES-TF")), ["canary-islands"]);
    }

    #[test]
    fn country_uses_top_level_extracts_only() {
        assert_eq!(ids(country("FR")), ["france"]);
    }

    #[test]
    fn region_found_by_iso_code_in_the_index() {
        assert_eq!(ids(region("FR-GF")), ["guyane"]);
    }

    #[test]
    fn unknown_region_falls_back_to_the_country() {
        assert_eq!(ids(region("FR-75")), ["france"]);
    }

    #[test]
    fn unknown_country_is_an_error() {
        assert!(catalog().for_partition(&country("PT"), &countries()).is_err());
    }

    #[test]
    fn mirror_replaces_the_host() {
        let url = "https://download.geofabrik.de/europe/spain/cataluna-latest.osm.pbf";
        assert_eq!(with_base(url, "https://mirror.example/osm/"), "https://mirror.example/osm/europe/spain/cataluna-latest.osm.pbf");
        assert_eq!(with_base(url, DEFAULT_BASE), url);
    }
}

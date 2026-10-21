//! Fusion of every staged source into one canonical dataset per country: one entity per
//! real-world thing, enriched by all sources (PROJECT.md §6.5).

pub mod addresses;
pub mod admin;
mod ids;
pub mod io;
pub mod merge;
pub mod metrics;
mod output;
pub mod places;
pub mod postcodes;
pub mod records;
pub mod relocate;
pub mod streets;
mod uf;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use geko_core::country::CountryCode;
use geko_core::model::AdminLevels;
use geko_core::ports::TextNormalizer;
use geko_core::{GekoError, Result};
use geko_geom::{Pt, distance_m};
use rstar::RTree;
use rstar::primitives::GeomWithData;
use serde::Serialize;

use self::addresses::{AddressEntity, AddressParams, AddressStats, AttrSource, Conflict};
use self::admin::AdminIndex;
use crate::incremental::{self, Key};
use self::metrics::{Metrics, Thresholds};
use self::places::{PlaceEntity, PlaceStats, PoiEntity};
use self::postcodes::Zones;
use self::records::AdminRec;
use self::streets::{StreetEntity, StreetParams, StreetStats};

const POI_MERGE_M: f64 = 30.0;
const POI_ADDRESS_M: f64 = 25.0;

#[derive(Debug, Clone)]
pub struct PostcodeEntity {
    pub postcode: String,
    pub point: Pt,
    pub municipality: Option<usize>,
    pub address_count: u32,
}

/// One postcode of an entity that spans several (a street, a municipality): which numbers it
/// covers, how many addresses have it and where those come from (PROJECT.md §6.5.4).
#[derive(Debug, Clone, Serialize)]
pub struct PostcodeShare {
    pub postcode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub addresses: Option<u32>,
    pub sources: BTreeMap<&'static str, u32>,
}

impl PostcodeShare {
    fn new(postcode: &str) -> Self {
        Self { postcode: postcode.to_owned(), from: None, to: None, addresses: None, sources: BTreeMap::new() }
    }

    fn count(&mut self, source: &'static str, number: Option<&geko_core::model::HouseNumber>) {
        *self.addresses.get_or_insert(0) += 1;
        *self.sources.entry(source).or_default() += 1;
        if let Some(n) = number {
            self.from = Some(self.from.map_or(n.number, |f| f.min(n.number)));
            let last = n.number.max(n.range_end.unwrap_or(0));
            self.to = Some(self.to.map_or(last, |t| t.max(last)));
        }
    }
}

#[derive(Default)]
struct PostcodeAcc {
    sum_x: i64,
    sum_y: i64,
    count: u32,
    municipalities: BTreeMap<Option<usize>, u32>,
}

pub struct Fused {
    pub country: CountryCode,
    pub admins: Vec<AdminRec>,
    pub streets: Vec<StreetEntity>,
    pub addresses: Vec<AddressEntity>,
    pub places: Vec<PlaceEntity>,
    pub pois: Vec<PoiEntity>,
    pub postcodes: Vec<PostcodeEntity>,
    /// Per street, its postcodes.
    pub street_postcodes: Vec<Vec<PostcodeShare>>,
    pub zones: Zones,
    pub conflicts: Vec<Conflict>,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct FusionReport {
    pub country: String,
    pub partition: String,
    pub records: BTreeMap<String, usize>,
    pub entities: BTreeMap<String, usize>,
    pub streets: StreetStats,
    pub addresses: AddressStats,
    pub places: PlaceStats,
    pub inferred: BTreeMap<String, usize>,
    pub metrics: Metrics,
    pub thresholds: Thresholds,
    pub failures: Vec<String>,
    pub elapsed_ms: u128,
}

pub const REPORT: &str = "fusion_report.json";

/// What the fusion of a partition uses besides its staging: the enabled sources, the rules of
/// the country (from its pack) and the quality thresholds.
pub struct Inputs<'a> {
    pub sources: &'a [String],
    pub normalizer: &'a dyn TextNormalizer,
    pub admin_levels: &'a AdminLevels,
    pub thresholds: &'a Thresholds,
}

/// Fuses `partition` unless the fusion code (`code`), its `inputs`, the staged data of the
/// partition and the records relocated into or out of it are unchanged since it was last fused
/// (PROJECT.md §8.3). Returns the report and whether the earlier fusion was reused.
pub fn fuse_partition_incremental(
    data_dir: &Path,
    partition: &geko_core::scope::Partition,
    inputs: &Inputs,
    code: &str,
    force: bool,
) -> Result<(FusionReport, bool)> {
    let Inputs { sources, admin_levels, thresholds, .. } = inputs;
    let out_dir = merge::partition_dir(data_dir, partition);
    let mut key = Key::new("fusion")
        .add("code", code)
        .add("thresholds", &serde_json::to_string(thresholds).map_err(|e| GekoError::Other(e.into()))?)
        .add("admin_levels", &format!("{admin_levels:?}"));
    for (source, dir) in staged_dirs(data_dir, partition, sources) {
        key = key.add(&source, &incremental::output_of(&dir, &[])?);
    }
    key = key.add("relocated", &relocate::digest_of(data_dir, partition)?);
    let key = key.finish();
    if !force
        && incremental::up_to_date(&out_dir, &key).is_some()
        && let Some(report) = std::fs::read(out_dir.join(REPORT)).ok().and_then(|b| serde_json::from_slice(&b).ok())
    {
        return Ok((report, true));
    }
    incremental::invalidate(&out_dir)?;
    let report = fuse_partition(data_dir, partition, inputs)?;
    incremental::seal(&out_dir, key, &[REPORT], serde_json::Value::Null)?;
    Ok((report, false))
}

/// Staging directories of the enabled `sources` for `partition`, by source name. A source that
/// was disabled leaves its staging on disk; it must not reach the canonical dataset.
fn staged_dirs(data_dir: &Path, partition: &geko_core::scope::Partition, sources: &[String]) -> Vec<(String, std::path::PathBuf)> {
    let mut out: Vec<(String, std::path::PathBuf)> = sources
        .iter()
        .map(|s| (s.clone(), crate::fetch::staging_dir(data_dir, &geko_core::model::SourceId(s.clone()), partition).path))
        .filter(|(_, dir)| dir.is_dir())
        .collect();
    out.sort();
    out
}

/// Fuses one partition (a province) into `merge::partition_dir`; `merge::merge` then joins the
/// partitions of a country. OSM is staged clipped to the boundary of each partition and other
/// sources are relocated to it (`relocate`), so every street, address and POI belongs to one.
pub fn fuse_partition(data_dir: &Path, partition: &geko_core::scope::Partition, inputs: &Inputs) -> Result<FusionReport> {
    let Inputs { sources, normalizer, admin_levels, thresholds } = inputs;
    let staged = records::load(data_dir, std::slice::from_ref(partition), sources)?;
    if staged.streets.is_empty() && staged.addresses.is_empty() {
        return Err(GekoError::Data(format!("nothing staged for {}; run `geko build` with at least one source", merge::partition_name(partition))));
    }
    fuse_staged(staged, partition, *normalizer, admin_levels, thresholds, &merge::partition_dir(data_dir, partition))
}

/// Fuses records already loaded from staging and writes the canonical dataset of the partition
/// into `out_dir`. Conformance suites use it to build canonical fixtures from synthetic records.
pub fn fuse_staged(
    staged: records::Staged,
    partition: &geko_core::scope::Partition,
    normalizer: &dyn TextNormalizer,
    levels: &AdminLevels,
    thresholds: &Thresholds,
    out_dir: &Path,
) -> Result<FusionReport> {
    let started = Instant::now();
    let country = partition.country;
    let mut records = BTreeMap::new();
    for a in &staged.addresses {
        *records.entry(format!("addresses.{}", a.source)).or_default() += 1;
    }
    records.insert("streets.osm".into(), staged.streets.len());
    records.insert("pois.osm".into(), staged.pois.len());
    records.insert("places.osm".into(), staged.places.len());
    records.insert("admins.osm".into(), staged.admins.len());

    let admin_index = AdminIndex::new(&staged.admins, levels);
    let streets = streets::build(&staged.streets, &staged.addresses, &admin_index, normalizer, &StreetParams::default());
    tracing::info!(entities = streets.entities.len(), elapsed = ?started.elapsed(), "streets fused");

    let (mut addresses, conflicts, address_stats) =
        addresses::build(&staged.addresses, &streets.address_street, normalizer, &AddressParams::default());
    let zones = Zones::build(&addresses);
    let mut inferred = BTreeMap::new();
    inferred.insert("address_postcodes".to_string(), postcodes::infer_for_addresses(&mut addresses, &zones));
    tracing::info!(entities = addresses.len(), elapsed = ?started.elapsed(), "addresses fused");

    let (mut places, mut place_stats) =
        places::build_places(&staged.admins, &staged.places, &admin_index, &streets.entities, normalizer);
    tracing::info!(places = places.len(), elapsed = ?started.elapsed(), "places fused");
    let mut pois = places::build_pois(&staged.pois, &admin_index, normalizer, POI_MERGE_M, &mut place_stats);
    tracing::info!(pois = pois.len(), elapsed = ?started.elapsed(), "POIs fused");

    let mut poi_postcodes = 0;
    for p in &mut pois {
        if let Some(pc) = zones.lookup(p.point) {
            p.postcode = Some(pc.to_owned());
            p.postcode_source = Some(AttrSource::Inferred);
            poi_postcodes += 1;
        }
    }
    inferred.insert("poi_postcodes".into(), poi_postcodes);

    let tree: RTree<GeomWithData<[f64; 2], usize>> = RTree::bulk_load(
        addresses.iter().enumerate().map(|(i, a)| GeomWithData::new([a.point.0 as f64, a.point.1 as f64], i)).collect(),
    );
    let mut poi_addresses = 0;
    for p in &mut pois {
        if let Some(near) = tree.nearest_neighbor(&[p.point.0 as f64, p.point.1 as f64]) {
            let a = &addresses[near.data];
            if distance_m(a.point, p.point) <= POI_ADDRESS_M && streets.entities[a.street].municipality == p.chain.municipality {
                p.address = Some(near.data);
                poi_addresses += 1;
            }
        }
    }
    inferred.insert("poi_addresses".into(), poi_addresses);
    tracing::info!(elapsed = ?started.elapsed(), "POI postcodes and addresses inferred");

    let mut street_postcodes: Vec<BTreeMap<String, PostcodeShare>> = vec![BTreeMap::new(); streets.entities.len()];
    let mut by_postcode: BTreeMap<&str, PostcodeAcc> = BTreeMap::new();
    let mut municipality_postcodes: BTreeMap<usize, BTreeMap<String, PostcodeShare>> = BTreeMap::new();
    for a in &addresses {
        let Some(pc) = a.postcode.as_deref() else { continue };
        let source = a.postcode_source.map_or("inferred", AttrSource::as_str);
        let muni = streets.entities[a.street].municipality;
        street_postcodes[a.street].entry(pc.to_owned()).or_insert_with(|| PostcodeShare::new(pc)).count(source, a.number.as_ref());
        if let Some(m) = muni {
            municipality_postcodes.entry(m).or_default().entry(pc.to_owned()).or_insert_with(|| PostcodeShare::new(pc)).count(source, None);
        }
        if !matches!(a.postcode_source, Some(AttrSource::Inferred)) {
            let acc = by_postcode.entry(pc).or_default();
            acc.sum_x += a.point.0 as i64;
            acc.sum_y += a.point.1 as i64;
            acc.count += 1;
            *acc.municipalities.entry(muni).or_default() += 1;
        }
    }
    // A street with no address gets the postcode of the zone around its representative point.
    for (i, s) in streets.entities.iter().enumerate() {
        if street_postcodes[i].is_empty()
            && let Some(pc) = s.representative_point().and_then(|p| zones.lookup(p))
        {
            let mut share = PostcodeShare::new(pc);
            share.sources.insert("inferred", 1);
            street_postcodes[i].insert(pc.to_owned(), share);
        }
    }
    // Municipalities span many postcodes: only the list. Point-like places (localities,
    // neighbourhoods, toponyms) get the one of their zone. Provinces and regions get none.
    for p in &mut places {
        match p.kind {
            geko_core::model::PlaceKind::Municipality => {
                p.postcodes = p.admin.and_then(|a| municipality_postcodes.remove(&a)).map(|m| m.into_values().collect()).unwrap_or_default();
            }
            geko_core::model::PlaceKind::Province | geko_core::model::PlaceKind::Region => {}
            _ => p.postcode = zones.lookup(p.point).map(str::to_owned),
        }
    }
    let postcode_entities = by_postcode
        .into_iter()
        .map(|(pc, acc)| PostcodeEntity {
            postcode: pc.to_owned(),
            point: ((acc.sum_x / acc.count as i64) as i32, (acc.sum_y / acc.count as i64) as i32),
            municipality: acc.municipalities.into_iter().max_by_key(|(m, c)| (*c, std::cmp::Reverse(*m))).and_then(|(m, _)| m),
            address_count: acc.count,
        })
        .collect();

    let fused = Fused {
        country,
        admins: staged.admins,
        streets: streets.entities,
        addresses,
        places,
        pois,
        postcodes: postcode_entities,
        street_postcodes: street_postcodes.into_iter().map(|m| m.into_values().collect()).collect(),
        zones,
        conflicts,
    };

    tracing::info!(elapsed = ?started.elapsed(), "postcodes of streets and places summed");
    let ids = ids::assign(&fused, partition);
    tracing::info!(elapsed = ?started.elapsed(), "ids assigned");
    let mut seen = std::collections::HashSet::new();
    if let Some(dup) = ids.places.iter().chain(&ids.streets).chain(&ids.addresses).chain(&ids.pois).find(|id| !seen.insert(id.as_str())) {
        return Err(GekoError::Data(format!("public id {dup} assigned twice")));
    }
    let importance = ids::importance(&fused, normalizer.languages());
    tracing::info!(elapsed = ?started.elapsed(), "importance computed");
    let metrics = metrics::compute(&fused, &ids, normalizer);
    tracing::info!(elapsed = ?started.elapsed(), "metrics computed");
    let failures = metrics.failures(thresholds);
    let local = ids::local_ids(&fused, &ids, &importance);
    output::write(out_dir, &fused, &ids, &importance, &local)?;
    tracing::info!(elapsed = ?started.elapsed(), "canonical written");

    let entities = BTreeMap::from([
        ("places".to_string(), fused.places.len()),
        ("streets".to_string(), fused.streets.len()),
        ("addresses".to_string(), fused.addresses.len()),
        ("pois".to_string(), fused.pois.len()),
        ("postcodes".to_string(), fused.postcodes.len()),
        ("conflicts".to_string(), fused.conflicts.len()),
    ]);
    let report = FusionReport {
        country: country.to_string(),
        partition: merge::partition_name(partition),
        records,
        entities,
        streets: streets.stats,
        addresses: address_stats,
        places: place_stats,
        inferred,
        metrics,
        thresholds: thresholds.clone(),
        failures,
        elapsed_ms: started.elapsed().as_millis(),
    };
    std::fs::write(
        out_dir.join(REPORT),
        serde_json::to_vec_pretty(&report).map_err(|e| GekoError::Other(e.into()))?,
    )?;
    if !report.failures.is_empty() {
        return Err(GekoError::Data(format!(
            "fusion quality check failed for {}: {} (see {})",
            report.partition,
            report.failures.join("; "),
            out_dir.join(REPORT).display()
        )));
    }
    Ok(report)
}

//! Readers of the staging into the records fusion works on (PROJECT.md §5.2, §6.5): OSM's own
//! schema, and the address schema every other source of addresses writes
//! (`schemas/staging/addresses.md`), so that a national source needs no code here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use geko_core::model::SourceId;
use geko_core::scope::Partition;
use geko_core::Result;
use geko_geom::{Pt, wkb};
use rustc_hash::FxHashSet;

use super::io::{Columns, read_batches};
use crate::fetch::staging_dir;

pub const OSM: &str = "osm";
/// The staged file of an address source (`schemas/staging/addresses.md`).
pub const ADDRESSES: &str = "addresses.parquet";
/// POIs of sources other than OSM, in the common schema (`schemas/staging/pois.md`).
pub const POIS: &str = "pois.parquet";

/// Source names live as long as the build: records and entities refer to them by `&'static str`.
fn source_name(name: &str) -> &'static str {
    static NAMES: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
    let mut names = NAMES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(n) = names.iter().find(|n| **n == name) {
        return n;
    }
    let n: &'static str = Box::leak(name.to_owned().into_boxed_str());
    names.push(n);
    n
}

pub type Names = BTreeMap<String, String>;

#[derive(Debug, Clone)]
pub struct AdminRec {
    pub osm_id: i64,
    pub level: u8,
    pub name: Option<String>,
    pub names: Names,
    pub iso3166_2: Option<String>,
    pub official_ref: Option<String>,
    pub population: Option<i64>,
    pub wikidata: Option<String>,
    pub point: Pt,
    pub rings: Vec<Vec<Pt>>,
}

#[derive(Debug, Clone)]
pub struct StreetSeg {
    pub source: &'static str,
    pub source_id: String,
    pub name: String,
    pub names: Names,
    pub highway: Option<String>,
    pub line: Vec<Pt>,
    pub point: Pt,
}

#[derive(Debug, Clone)]
pub struct AddrRec {
    pub source: &'static str,
    pub source_id: String,
    pub street: Option<String>,
    pub place: Option<String>,
    pub number: Option<String>,
    pub extension: Option<String>,
    pub postcode: Option<String>,
    pub point: Pt,
}

#[derive(Debug, Clone)]
pub struct PoiRec {
    pub source: &'static str,
    pub source_id: String,
    pub name: String,
    pub names: Names,
    pub category: String,
    pub wikidata: Option<String>,
    pub phone: Option<String>,
    pub website: Option<String>,
    pub point: Pt,
}

#[derive(Debug, Clone)]
pub struct PlaceRec {
    pub source: &'static str,
    pub source_id: String,
    pub name: String,
    pub names: Names,
    pub place: String,
    pub population: Option<i64>,
    pub wikidata: Option<String>,
    pub point: Pt,
}

#[derive(Default)]
pub struct Staged {
    pub admins: Vec<AdminRec>,
    pub streets: Vec<StreetSeg>,
    pub addresses: Vec<AddrRec>,
    pub pois: Vec<PoiRec>,
    pub places: Vec<PlaceRec>,
}

/// Staging directories of the partitions in scope only: data staged for regions that left the
/// scope stays on disk but must not reach the canonical dataset.
fn partition_dirs(data_dir: &Path, source: &str, partitions: &[Partition]) -> Vec<PathBuf> {
    partitions
        .iter()
        .map(|p| staging_dir(data_dir, &SourceId(source.into()), p).path)
        .filter(|d| d.is_dir())
        .collect()
}

fn names(json: Option<&str>) -> Names {
    json.and_then(|j| serde_json::from_str(j).ok()).unwrap_or_default()
}

fn osm_id(osm_type: Option<&str>, id: Option<i64>) -> String {
    format!("{}{}", osm_type.unwrap_or("?"), id.unwrap_or_default())
}

/// Loads the staged partitions of the scope, from OSM and the other `sources` that are enabled,
/// in name order. Records repeated across partitions (e.g. the boundaries that contain several
/// regions) are kept once.
pub fn load(data_dir: &Path, partitions: &[Partition], sources: &[String]) -> Result<Staged> {
    let mut staged = Staged::default();
    let mut seen: FxHashSet<(&'static str, &'static str, String)> = FxHashSet::default();
    for dir in partition_dirs(data_dir, OSM, partitions) {
        load_osm(&dir, &mut staged, &mut seen)?;
    }
    let mut others: Vec<&str> = sources.iter().map(String::as_str).filter(|s| *s != OSM).collect();
    others.sort_unstable();
    for source in others {
        let name = source_name(source);
        for p in partitions {
            let dir = staging_dir(data_dir, &SourceId(source.into()), p).path;
            for (file, load) in [(ADDRESSES, load_addresses as Loader), (POIS, load_pois as Loader)] {
                let relocated = super::relocate::of(data_dir, p, source, file)?;
                if dir.join(file).is_file() {
                    load(&dir.join(file), name, &relocated.moved, &mut staged, &mut seen)?;
                }
                if let Some(incoming) = &relocated.incoming {
                    load(incoming, name, &FxHashSet::default(), &mut staged, &mut seen)?;
                }
            }
        }
    }
    Ok(staged)
}

type Seen = FxHashSet<(&'static str, &'static str, String)>;

fn load_osm(dir: &Path, s: &mut Staged, seen: &mut Seen) -> Result<()> {
    let path = dir.join("admins.parquet");
    for batch in read_batches(&path)? {
        let c = Columns::new(&batch, &path);
        let (ty, id, lat, lon) = (c.str("osm_type")?, c.i64("osm_id")?, c.i32("lat_e7")?, c.i32("lon_e7")?);
        let (name, nm, wd) = (c.str("name")?, c.str("names")?, c.str("wikidata")?);
        let (level, iso2, oref, pop, geom) =
            (c.u8("admin_level")?, c.str("iso3166_2")?, c.str("official_ref")?, c.i64("population")?, c.bin("geom")?);
        for i in 0..c.rows() {
            if !seen.insert((OSM, "admin", osm_id(ty(i), id(i)))) {
                continue;
            }
            let Some(wkb::Geometry::Polygons(polygons)) = wkb::parse(geom(i)) else { continue };
            s.admins.push(AdminRec {
                osm_id: id(i).unwrap_or_default(),
                level: level(i),
                name: name(i).map(str::to_owned),
                names: names(nm(i)),
                iso3166_2: iso2(i).map(str::to_owned),
                official_ref: oref(i).map(str::to_owned),
                population: pop(i),
                wikidata: wd(i).map(str::to_owned),
                point: (lon(i), lat(i)),
                rings: polygons.into_iter().flatten().collect(),
            });
        }
    }

    let path = dir.join("streets.parquet");
    for batch in read_batches(&path)? {
        let c = Columns::new(&batch, &path);
        let (ty, id, lat, lon, name, nm) =
            (c.str("osm_type")?, c.i64("osm_id")?, c.i32("lat_e7")?, c.i32("lon_e7")?, c.str("name")?, c.str("names")?);
        let (highway, geom) = (c.str("highway")?, c.bin("geom")?);
        for i in 0..c.rows() {
            let source_id = osm_id(ty(i), id(i));
            let Some(n) = name(i) else { continue };
            if !seen.insert((OSM, "street", source_id.clone())) {
                continue;
            }
            let Some(wkb::Geometry::Lines(mut lines)) = wkb::parse(geom(i)) else { continue };
            s.streets.push(StreetSeg {
                source: OSM,
                source_id,
                name: n.to_owned(),
                names: names(nm(i)),
                highway: highway(i).map(str::to_owned),
                line: lines.pop().unwrap_or_default(),
                point: (lon(i), lat(i)),
            });
        }
    }

    let path = dir.join("addresses.parquet");
    for batch in read_batches(&path)? {
        let c = Columns::new(&batch, &path);
        let (ty, id, lat, lon) = (c.str("osm_type")?, c.i64("osm_id")?, c.i32("lat_e7")?, c.i32("lon_e7")?);
        let (hn, street, place, pc) = (c.str("housenumber")?, c.str("street")?, c.str("place")?, c.str("postcode")?);
        for i in 0..c.rows() {
            let source_id = osm_id(ty(i), id(i));
            if !seen.insert((OSM, "address", source_id.clone())) {
                continue;
            }
            s.addresses.push(AddrRec {
                source: OSM,
                source_id,
                street: street(i).map(str::to_owned),
                place: place(i).map(str::to_owned),
                number: hn(i).map(str::to_owned),
                extension: None,
                postcode: pc(i).map(str::to_owned),
                point: (lon(i), lat(i)),
            });
        }
    }

    let path = dir.join("pois.parquet");
    for batch in read_batches(&path)? {
        let c = Columns::new(&batch, &path);
        let (ty, id, lat, lon, name, nm, wd) = (
            c.str("osm_type")?,
            c.i64("osm_id")?,
            c.i32("lat_e7")?,
            c.i32("lon_e7")?,
            c.str("name")?,
            c.str("names")?,
            c.str("wikidata")?,
        );
        let category = c.str("category")?;
        let (phone, website) = (c.opt_str("phone"), c.opt_str("website"));
        for i in 0..c.rows() {
            let source_id = osm_id(ty(i), id(i));
            let Some(n) = name(i) else { continue };
            if !seen.insert((OSM, "poi", source_id.clone())) {
                continue;
            }
            s.pois.push(PoiRec {
                source: OSM,
                source_id,
                name: n.to_owned(),
                names: names(nm(i)),
                category: category(i).unwrap_or_default().to_owned(),
                wikidata: wd(i).map(str::to_owned),
                phone: phone(i).map(str::to_owned),
                website: website(i).map(str::to_owned),
                point: (lon(i), lat(i)),
            });
        }
    }

    let path = dir.join("places.parquet");
    for batch in read_batches(&path)? {
        let c = Columns::new(&batch, &path);
        let (ty, id, lat, lon, name, nm, wd) = (
            c.str("osm_type")?,
            c.i64("osm_id")?,
            c.i32("lat_e7")?,
            c.i32("lon_e7")?,
            c.str("name")?,
            c.str("names")?,
            c.str("wikidata")?,
        );
        let (place, pop) = (c.str("place")?, c.i64("population")?);
        for i in 0..c.rows() {
            let source_id = osm_id(ty(i), id(i));
            let Some(n) = name(i) else { continue };
            if !seen.insert((OSM, "place", source_id.clone())) {
                continue;
            }
            s.places.push(PlaceRec {
                source: OSM,
                source_id,
                name: n.to_owned(),
                names: names(nm(i)),
                place: place(i).unwrap_or_default().to_owned(),
                population: pop(i),
                wikidata: wd(i).map(str::to_owned),
                point: (lon(i), lat(i)),
            });
        }
    }
    Ok(())
}

/// Loads the addresses of `source` staged in `path`, except the row numbers in `skip`.
type Loader = fn(&Path, &'static str, &FxHashSet<u64>, &mut Staged, &mut Seen) -> Result<()>;

/// POIs of a source in the common schema; `skip` holds the rows relocated to another partition.
fn load_pois(path: &Path, source: &'static str, skip: &FxHashSet<u64>, s: &mut Staged, seen: &mut Seen) -> Result<()> {
    let mut row = 0u64;
    for batch in read_batches(path)? {
        let c = Columns::new(&batch, path);
        let (id, name, nm, category, wd) = (c.str("source_id")?, c.str("name")?, c.str("names")?, c.str("category")?, c.str("wikidata")?);
        let (lat, lon) = (c.i32("lat_e7")?, c.i32("lon_e7")?);
        let (phone, website) = (c.opt_str("phone"), c.opt_str("website"));
        for i in 0..c.rows() {
            row += 1;
            if skip.contains(&(row - 1)) {
                continue;
            }
            let Some(n) = name(i).filter(|n| !n.trim().is_empty()) else { continue };
            let source_id = id(i).unwrap_or_default().to_owned();
            if !seen.insert((source, "poi", source_id.clone())) {
                continue;
            }
            s.pois.push(PoiRec {
                source,
                source_id,
                name: n.to_owned(),
                names: names(nm(i)),
                category: category(i).unwrap_or_default().to_owned(),
                wikidata: wd(i).map(str::to_owned),
                phone: phone(i).map(str::to_owned),
                website: website(i).map(str::to_owned),
                point: (lon(i), lat(i)),
            });
        }
    }
    Ok(())
}

fn load_addresses(path: &Path, source: &'static str, skip: &FxHashSet<u64>, s: &mut Staged, seen: &mut Seen) -> Result<()> {
    let mut row = 0u64;
    for batch in read_batches(path)? {
        let c = Columns::new(&batch, path);
        let (id, street, number, ext, pc) =
            (c.str("source_id")?, c.str("street")?, c.str("number")?, c.str("number_extension")?, c.str("postcode")?);
        let (place, lat, lon) = (c.str("place")?, c.i32("lat_e7")?, c.i32("lon_e7")?);
        for i in 0..c.rows() {
            row += 1;
            if skip.contains(&(row - 1)) {
                continue;
            }
            let source_id = id(i).unwrap_or_default().to_owned();
            if !seen.insert((source, "address", source_id.clone())) {
                continue;
            }
            s.addresses.push(AddrRec {
                source,
                source_id,
                street: street(i).map(str::to_owned),
                place: place(i).map(str::to_owned),
                number: number(i).map(str::to_owned),
                extension: ext(i).map(str::to_owned),
                postcode: pc(i).map(str::to_owned),
                point: (lon(i), lat(i)),
            });
        }
    }
    Ok(())
}

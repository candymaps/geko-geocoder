//! A small canonical dataset built with the real fusion from synthetic staged records, so that
//! index suites always test the canonical contract that `geko build` writes (schemas/canonical.md).
//!
//! Algaida (Mallorca): a province, a municipality, two OSM streets, portals of CartoCiudad and
//! OSM along "Carrer Major", a pharmacy and the hamlet of Pòrtol.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use geko_build::fusion::io::{Columns, read_columns};
use geko_build::fusion::metrics::Thresholds;
use geko_build::fusion::records::{AddrRec, AdminRec, OSM, PlaceRec, PoiRec, Staged, StreetSeg};

/// A national source of addresses, as the fixtures stage one.
const NATIONAL: &str = "national";
use geko_build::fusion::{fuse_staged, merge};
use geko_core::country::{CountryCode, RegionCode};
use geko_core::geo::Coord;
use geko_core::model::{LocalId, PlaceKind};
use geko_core::ports::TextNormalizer;
use geko_core::scope::Partition;

/// Degrees to the fixed-point `(lon_e7, lat_e7)` that fusion records use.
fn pt(lat: f64, lon: f64) -> (i32, i32) {
    ((lon * 1e7).round() as i32, (lat * 1e7).round() as i32)
}

fn square(lat: f64, lon: f64, half: f64) -> Vec<(i32, i32)> {
    let (s, n, w, e) = (lat - half, lat + half, lon - half, lon + half);
    vec![pt(s, w), pt(s, e), pt(n, e), pt(n, w), pt(s, w)]
}

pub const LAT: f64 = 39.5600;
pub const LON: f64 = 2.8900;
/// Spacing of the portals of Carrer Major, about 9 m.
const STEP: f64 = 0.0001;

pub struct Fixture {
    _tmp: tempfile::TempDir,
    pub country: CountryCode,
    /// The canonical dataset of the country (`canonical/<country>`).
    pub canonical: PathBuf,
    /// A scratch directory for the artifacts under test.
    pub out: PathBuf,
    pub municipality: &'static str,
    pub street: &'static str,
    /// Position of Carrer Major `n`, for `n` in 1..=10.
    pub portal: fn(u32) -> Coord,
    pub poi: &'static str,
    pub poi_point: Coord,
    pub locality: &'static str,
    /// A point inside the municipality but far from every feature, and one outside it.
    pub inside_empty: Coord,
    pub outside: Coord,
    /// Kind and position of every entity, by local id, read back from the canonical dataset.
    pub entities: HashMap<u32, (PlaceKind, Coord)>,
}

impl Fixture {
    pub fn entity(&self, id: LocalId) -> (PlaceKind, Coord) {
        *self.entities.get(&id.0).unwrap_or_else(|| panic!("local id {} is not in the fixture", id.0))
    }
}

fn read_entities(canonical: &Path) -> HashMap<u32, (PlaceKind, Coord)> {
    let mut out = HashMap::new();
    for (table, kind) in [
        ("places", None),
        ("streets", Some(PlaceKind::Street)),
        ("addresses", Some(PlaceKind::Address)),
        ("pois", Some(PlaceKind::Poi)),
        ("postcodes", Some(PlaceKind::Postcode)),
    ] {
        let path = canonical.join(format!("{table}.parquet"));
        let names: &[&str] = if kind.is_none() { &["local_id", "lat_e7", "lon_e7", "kind"] } else { &["local_id", "lat_e7", "lon_e7"] };
        for batch in read_columns(&path, names).expect("canonical table") {
            let c = Columns::new(&batch, &path);
            let (local, lat, lon) = (c.u32("local_id").unwrap(), c.i32("lat_e7").unwrap(), c.i32("lon_e7").unwrap());
            let kinds = kind.is_none().then(|| c.str("kind").unwrap());
            for i in 0..c.rows() {
                let k = kind.unwrap_or_else(|| kinds.as_ref().unwrap()(i).unwrap().parse().unwrap());
                out.insert(local(i), (k, Coord::from_e7(lat(i), lon(i)).unwrap()));
            }
        }
    }
    out
}

fn portal(n: u32) -> Coord {
    Coord::from_degrees(LAT, LON + STEP * n as f64).expect("valid coordinates")
}

fn names() -> geko_build::fusion::records::Names {
    Default::default()
}

/// Builds the canonical dataset with `normalizer`, the text rules of Spain.
pub fn canonical(normalizer: &dyn TextNormalizer) -> Fixture {
    let tmp = tempfile::tempdir().expect("temporary directory");
    let es = CountryCode::new("ES").unwrap();
    let partition = Partition { country: es, region: Some(RegionCode::new("ES-PM").unwrap()) };
    let admin = |osm_id, level, name: &str, iso: Option<&str>, official: Option<&str>, half| AdminRec {
        osm_id,
        level,
        name: Some(name.into()),
        names: names(),
        iso3166_2: iso.map(Into::into),
        official_ref: official.map(Into::into),
        population: Some(5_000),
        wikidata: None,
        point: pt(LAT, LON),
        rings: vec![square(LAT, LON, half)],
    };
    let street = |id: &str, name: &str, line: Vec<(i32, i32)>| StreetSeg {
        source: OSM,
        source_id: id.into(),
        name: name.into(),
        names: names(),
        highway: Some("residential".into()),
        point: line[0],
        line,
    };
    let mut addresses: Vec<AddrRec> = (1..=10)
        .map(|n| AddrRec {
            source: NATIONAL,
            source_id: format!("AD_ADDRESS_PPK_FIXTURE_{n}"),
            street: Some("CALLE MAJOR".into()),
            place: Some("Algaida".into()),
            number: Some(n.to_string()),
            extension: None,
            postcode: Some("07210".into()),
            point: pt(LAT, LON + STEP * n as f64),
        })
        .collect();
    // OSM has number 3 too, a few metres away: fusion keeps one portal.
    addresses.push(AddrRec {
        source: OSM,
        source_id: "n100".into(),
        street: Some("Carrer Major".into()),
        place: None,
        number: Some("3".into()),
        extension: None,
        postcode: None,
        point: pt(LAT + 0.00002, LON + STEP * 3.0),
    });
    let staged = Staged {
        admins: vec![
            admin(1, 6, "Illes Balears", Some("ES-PM"), None, 0.5),
            admin(2, 8, "Algaida", None, Some("07002"), 0.05),
        ],
        streets: vec![
            street("w1", "Carrer Major", vec![pt(LAT, LON), pt(LAT, LON + STEP * 11.0)]),
            street("w2", "Avinguda de Jaume III", vec![pt(LAT + 0.002, LON), pt(LAT + 0.002, LON + 0.002)]),
        ],
        addresses,
        pois: vec![PoiRec {
            source: OSM,
            source_id: "n200".into(),
            name: "Farmàcia Central".into(),
            names: names(),
            category: "amenity=pharmacy".into(),
            wikidata: None,
            phone: None,
            website: None,
            point: pt(LAT + 0.0001, LON + STEP * 5.0),
        }],
        places: vec![PlaceRec {
            source: OSM,
            source_id: "n300".into(),
            name: "Pòrtol".into(),
            names: names(),
            place: "hamlet".into(),
            population: Some(300),
            wikidata: None,
            point: pt(LAT + 0.02, LON - 0.02),
        }],
    };
    let data = tmp.path();
    let report = fuse_staged(staged, &partition, normalizer, &Default::default(), &Thresholds::default(), &merge::partition_dir(data, &partition))
        .expect("the fixture fuses");
    merge::merge(data, es, std::slice::from_ref(&partition), vec![report]).expect("the fixture merges");
    let out = data.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let canonical = merge::country_dir(data, es);
    Fixture {
        country: es,
        entities: read_entities(&canonical),
        canonical,
        out,
        municipality: "Algaida",
        street: "Carrer Major",
        portal,
        poi: "Farmàcia Central",
        poi_point: Coord::from_degrees(LAT + 0.0001, LON + STEP * 5.0).unwrap(),
        locality: "Pòrtol",
        inside_empty: Coord::from_degrees(LAT - 0.04, LON + 0.04).unwrap(),
        outside: Coord::from_degrees(LAT + 1.0, LON + 1.0).unwrap(),
        _tmp: tmp,
    }
}

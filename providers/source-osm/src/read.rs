//! Reads a `.pbf` in three parallel passes (relations, ways, nodes) so that only the
//! coordinates of nodes actually used are kept in memory (PROJECT.md §8.1).

use std::collections::BTreeMap;
use std::path::Path;

use geko_core::{GekoError, Result};
use osmpbf::{BlobDecode, BlobReader, PrimitiveBlock, RelMemberType};
use rayon::prelude::*;
use roaring::RoaringTreemap;
use rustc_hash::FxHashMap;

use geko_geom::{self as geom, Pt};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OsmType {
    Node,
    Way,
    Relation,
}

impl OsmType {
    pub fn as_str(self) -> &'static str {
        match self {
            OsmType::Node => "n",
            OsmType::Way => "w",
            OsmType::Relation => "r",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Common {
    pub osm_type: OsmType,
    pub osm_id: i64,
    pub point: Pt,
    pub name: Option<String>,
    pub names: BTreeMap<String, String>,
    pub wikidata: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Street {
    pub common: Common,
    pub highway: String,
    pub geom: Vec<Pt>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Address {
    pub common: Common,
    pub housenumber: String,
    pub street: Option<String>,
    pub place: Option<String>,
    pub postcode: Option<String>,
    pub city: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Poi {
    pub common: Common,
    pub category: String,
    pub phone: Option<String>,
    pub website: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    pub common: Common,
    pub place: String,
    pub population: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct Admin {
    pub common: Common,
    pub admin_level: u8,
    pub iso3166_1: Option<String>,
    pub iso3166_2: Option<String>,
    pub official_ref: Option<String>,
    pub population: Option<i64>,
    pub rings: Vec<Vec<Pt>>,
    pub polygons: Vec<geom::Polygon>,
}

#[derive(Default)]
pub struct Extracted {
    pub streets: Vec<Street>,
    pub addresses: Vec<Address>,
    pub pois: Vec<Poi>,
    pub places: Vec<Place>,
    pub admins: Vec<Admin>,
}

const STREET_HIGHWAYS: &[&str] = &[
    "motorway", "trunk", "primary", "secondary", "tertiary", "unclassified", "residential", "living_street",
    "pedestrian", "service", "road", "track", "footway", "path", "cycleway", "steps", "motorway_link", "trunk_link",
    "primary_link", "secondary_link", "tertiary_link",
];

const PLACES: &[&str] = &[
    "city", "town", "village", "hamlet", "isolated_dwelling", "suburb", "quarter", "neighbourhood", "borough",
    "locality", "square",
];

const POI_KEYS: &[&str] = &[
    "amenity", "shop", "tourism", "leisure", "office", "craft", "healthcare", "historic", "aeroway", "railway",
    "public_transport", "natural", "man_made", "sport",
];

const EXTRA_NAME_KEYS: &[&str] = &["alt_name", "old_name", "official_name", "short_name", "loc_name", "reg_name"];

type Tags<'a> = Vec<(&'a str, &'a str)>;

fn get<'a>(tags: &[(&'a str, &'a str)], key: &str) -> Option<&'a str> {
    tags.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

fn owned(tags: &[(&str, &str)], key: &str) -> Option<String> {
    get(tags, key).map(str::to_owned)
}

fn is_name_key(key: &str) -> bool {
    key == "name"
        || (key.starts_with("name:") && !key.starts_with("name:etymology") && !key.starts_with("name:prefix"))
        || EXTRA_NAME_KEYS.iter().any(|k| key == *k || key.strip_prefix(k).is_some_and(|rest| rest.starts_with(':')))
}

fn common(osm_type: OsmType, osm_id: i64, point: Pt, tags: &[(&str, &str)]) -> Common {
    Common {
        osm_type,
        osm_id,
        point,
        name: owned(tags, "name"),
        names: tags.iter().filter(|(k, _)| is_name_key(k)).map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        wikidata: owned(tags, "wikidata"),
    }
}

fn poi(common: Common, category: String, tags: &[(&str, &str)]) -> Poi {
    let first = |keys: &[&str]| keys.iter().find_map(|k| owned(tags, k));
    Poi { common, category, phone: first(&["phone", "contact:phone"]), website: first(&["website", "contact:website"]) }
}

fn population(tags: &[(&str, &str)]) -> Option<i64> {
    get(tags, "population").and_then(|p| p.replace(['.', ',', ' '], "").parse().ok())
}

fn poi_category(tags: &[(&str, &str)]) -> Option<String> {
    get(tags, "name")?;
    POI_KEYS.iter().find_map(|k| get(tags, k).filter(|v| *v != "no").map(|v| format!("{k}={v}")))
}

fn street_class<'a>(tags: &[(&'a str, &'a str)]) -> Option<&'a str> {
    let highway = get(tags, "highway").filter(|h| STREET_HIGHWAYS.contains(h))?;
    // Sidewalks and crossings repeat the name of the street they belong to.
    if matches!(get(tags, "footway"), Some("sidewalk" | "crossing")) {
        return None;
    }
    (get(tags, "name").is_some() || get(tags, "ref").is_some()).then_some(highway)
}

fn to_pt(lat: f64, lon: f64) -> Pt {
    ((lon * 1e7).round() as i32, (lat * 1e7).round() as i32)
}

fn osm_err(e: osmpbf::Error) -> GekoError {
    GekoError::Data(format!("cannot read OSM data: {e}"))
}

struct AdminRelation {
    id: i64,
    tags: Vec<(String, String)>,
    outer: Vec<i64>,
    inner: Vec<i64>,
    label: Option<i64>,
}

/// A multipolygon that is a POI or an address: landmarks such as the Sagrada Família basilica
/// or large parks are mapped this way, not as nodes or ways.
struct FeatureRelation {
    id: i64,
    tags: Vec<(String, String)>,
    outer: Vec<i64>,
    category: Option<String>,
    address: bool,
}

enum PendingKind {
    Street(String),
    Address,
    Poi(String),
}

struct PendingWay {
    id: i64,
    refs: Vec<i64>,
    tags: Vec<(String, String)>,
    kinds: Vec<PendingKind>,
}

enum NodeOut {
    Address(Address),
    Poi(Poi),
    Place(Place),
}

/// Decodes blocks in parallel and maps each one with `f`, in file order. Per-block results
/// avoid the cost of building and merging one result per element.
fn par_blocks<T: Send>(path: &Path, f: impl Fn(&PrimitiveBlock) -> T + Sync + Send) -> Result<Vec<T>> {
    // `read` makes three passes: together they count as the file, once, in the progress of the build.
    let file = std::io::BufReader::new(std::fs::File::open(path)?);
    // `par_bridge` yields in completion order; the staged records, and every choice fusion
    // makes from their order, must be the same on every run.
    let mut out: Vec<(usize, T)> = BlobReader::new(geko_core::progress::Reading::new(file, 3))
        .enumerate()
        .par_bridge()
        .map(|(i, blob)| match blob.map_err(osm_err)?.decode().map_err(osm_err)? {
            BlobDecode::OsmData(block) => Ok(Some((i, f(&block)))),
            _ => Ok(None),
        })
        .filter_map(Result::transpose)
        .collect::<Result<_>>()?;
    out.sort_unstable_by_key(|(i, _)| *i);
    Ok(out.into_iter().map(|(_, t)| t).collect())
}

fn admin_relation(r: &osmpbf::Relation) -> Option<AdminRelation> {
    let tags: Tags = r.tags().collect();
    if get(&tags, "boundary") != Some("administrative") || get(&tags, "admin_level").is_none() {
        return None;
    }
    let mut rel = AdminRelation {
        id: r.id(),
        tags: tags.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        outer: Vec::new(),
        inner: Vec::new(),
        label: None,
    };
    for m in r.members() {
        let role = m.role().unwrap_or("");
        match (&m.member_type, role) {
            (RelMemberType::Way, "inner") => rel.inner.push(m.member_id),
            (RelMemberType::Way, _) => rel.outer.push(m.member_id),
            (RelMemberType::Node, "label") => rel.label = Some(m.member_id),
            (RelMemberType::Node, "admin_centre") if rel.label.is_none() => rel.label = Some(m.member_id),
            _ => {}
        }
    }
    Some(rel)
}

fn feature_relation(r: &osmpbf::Relation) -> Option<FeatureRelation> {
    let tags: Tags = r.tags().collect();
    if get(&tags, "type") != Some("multipolygon") {
        return None;
    }
    let category = poi_category(&tags);
    let address = get(&tags, "addr:housenumber").is_some();
    if category.is_none() && !address {
        return None;
    }
    let outer = r
        .members()
        .filter(|m| matches!(m.member_type, RelMemberType::Way) && m.role().unwrap_or("") != "inner")
        .map(|m| m.member_id)
        .collect();
    Some(FeatureRelation {
        id: r.id(),
        tags: tags.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        outer,
        category,
        address,
    })
}

fn pending_way(w: &osmpbf::Way, boundary_ways: &RoaringTreemap) -> Option<PendingWay> {
    let is_boundary = boundary_ways.contains(w.id() as u64);
    let interesting = w.tags().any(|(k, _)| k == "highway" || k == "addr:housenumber" || k == "name");
    if !interesting && !is_boundary {
        return None;
    }
    let tags: Tags = if interesting { w.tags().collect() } else { Vec::new() };
    let mut kinds = Vec::new();
    if let Some(h) = street_class(&tags) {
        kinds.push(PendingKind::Street(h.to_owned()));
    }
    if get(&tags, "addr:housenumber").is_some() {
        kinds.push(PendingKind::Address);
    }
    if kinds.iter().all(|k| !matches!(k, PendingKind::Street(_)))
        && let Some(category) = poi_category(&tags)
    {
        kinds.push(PendingKind::Poi(category));
    }
    if kinds.is_empty() && !is_boundary {
        return None;
    }
    let tags = if kinds.is_empty() { Vec::new() } else { tags.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() };
    Some(PendingWay { id: w.id(), refs: w.refs().collect(), tags, kinds })
}

#[derive(Default)]
struct NodeBlock {
    coords: Vec<(i64, Pt)>,
    features: Vec<NodeOut>,
}

impl NodeBlock {
    fn add<'a>(&mut self, needed: &RoaringTreemap, id: i64, point: Pt, tags: impl Iterator<Item = (&'a str, &'a str)>) {
        if needed.contains(id as u64) {
            self.coords.push((id, point));
        }
        let mut tags = tags.peekable();
        if tags.peek().is_some() {
            let tags: Tags = tags.collect();
            node_features(id, point, &tags, &mut self.features);
        }
    }
}

fn as_refs(tags: &[(String, String)]) -> Tags<'_> {
    tags.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

fn node_features(id: i64, point: Pt, tags: &[(&str, &str)], out: &mut Vec<NodeOut>) {
    if tags.is_empty() {
        return;
    }
    if let Some(housenumber) = get(tags, "addr:housenumber") {
        out.push(NodeOut::Address(address(OsmType::Node, id, point, housenumber, tags)));
    }
    if let Some(category) = poi_category(tags) {
        out.push(NodeOut::Poi(poi(common(OsmType::Node, id, point, tags), category, tags)));
    }
    if let Some(place) = get(tags, "place").filter(|p| PLACES.contains(p))
        && get(tags, "name").is_some()
    {
        out.push(NodeOut::Place(Place {
            common: common(OsmType::Node, id, point, tags),
            place: place.to_owned(),
            population: population(tags),
        }));
    }
}

fn address(osm_type: OsmType, id: i64, point: Pt, housenumber: &str, tags: &[(&str, &str)]) -> Address {
    Address {
        common: common(osm_type, id, point, tags),
        housenumber: housenumber.to_owned(),
        street: owned(tags, "addr:street"),
        place: owned(tags, "addr:place"),
        postcode: owned(tags, "addr:postcode"),
        city: owned(tags, "addr:city"),
    }
}

pub fn read(path: &Path) -> Result<Extracted> {
    let t = std::time::Instant::now();
    // Pass 1: administrative boundaries and multipolygon features, and the ways and label
    // nodes they reference.
    let mut relations: Vec<AdminRelation> = Vec::new();
    let mut features: Vec<FeatureRelation> = Vec::new();
    for (admins, feats) in par_blocks(path, |block| {
        let (mut admins, mut feats) = (Vec::new(), Vec::new());
        for g in block.groups() {
            for r in g.relations() {
                if let Some(a) = admin_relation(&r) {
                    admins.push(a);
                } else if let Some(f) = feature_relation(&r) {
                    feats.push(f);
                }
            }
        }
        (admins, feats)
    })? {
        relations.extend(admins);
        features.extend(feats);
    }

    let mut boundary_ways = RoaringTreemap::new();
    for r in &relations {
        boundary_ways.extend(r.outer.iter().chain(&r.inner).map(|id| *id as u64));
    }
    for f in &features {
        boundary_ways.extend(f.outer.iter().map(|id| *id as u64));
    }
    tracing::debug!(elapsed = ?t.elapsed(), relations = relations.len(), "pass 1 done");

    // Pass 2: ways that are features themselves or parts of boundaries.
    let ways: Vec<PendingWay> = par_blocks(path, |block| {
        block
            .groups()
            .flat_map(|g| g.ways().filter_map(|w| pending_way(&w, &boundary_ways)).collect::<Vec<_>>())
            .collect::<Vec<_>>()
    })?
    .into_iter()
    .flatten()
    .collect();

    let mut needed = RoaringTreemap::new();
    for w in &ways {
        needed.extend(w.refs.iter().map(|id| *id as u64));
    }
    needed.extend(relations.iter().filter_map(|r| r.label).map(|id| id as u64));
    tracing::debug!(elapsed = ?t.elapsed(), ways = ways.len(), needed = needed.len(), "pass 2 done");

    // Pass 3: coordinates of needed nodes, and node features.
    let blocks = par_blocks(path, |block| {
        let mut out = NodeBlock::default();
        for g in block.groups() {
            for n in g.dense_nodes() {
                out.add(&needed, n.id(), to_pt(n.lat(), n.lon()), n.tags());
            }
            for n in g.nodes() {
                out.add(&needed, n.id(), to_pt(n.lat(), n.lon()), n.tags());
            }
        }
        out
    })?;

    let mut extracted = Extracted::default();
    let mut coords: Vec<(i64, Pt)> = Vec::with_capacity(blocks.iter().map(|b| b.coords.len()).sum());
    for block in blocks {
        coords.extend(block.coords);
        for f in block.features {
            match f {
                NodeOut::Address(a) => extracted.addresses.push(a),
                NodeOut::Poi(p) => extracted.pois.push(p),
                NodeOut::Place(p) => extracted.places.push(p),
            }
        }
    }
    coords.par_sort_unstable_by_key(|c| c.0);
    let coord = |id: &i64| coords.binary_search_by_key(id, |c| c.0).ok().map(|i| coords[i].1);
    tracing::debug!(elapsed = ?t.elapsed(), coords = coords.len(), "pass 3 done");

    let mut way_geoms: FxHashMap<i64, Vec<Pt>> = FxHashMap::default();
    for w in ways {
        let geom: Vec<Pt> = w.refs.iter().filter_map(coord).collect();
        if boundary_ways.contains(w.id as u64) {
            way_geoms.insert(w.id, geom.clone());
        }
        if geom.len() < 2 || w.kinds.is_empty() {
            continue;
        }
        let tags = as_refs(&w.tags);
        let centroid = geom::centroid(&geom).expect("non-empty geometry");
        for kind in w.kinds {
            match kind {
                PendingKind::Street(highway) => extracted.streets.push(Street {
                    common: common(OsmType::Way, w.id, geom[geom.len() / 2], &tags),
                    highway,
                    geom: geom.clone(),
                }),
                PendingKind::Address => {
                    let housenumber = get(&tags, "addr:housenumber").expect("classified as address");
                    extracted.addresses.push(address(OsmType::Way, w.id, centroid, housenumber, &tags));
                }
                PendingKind::Poi(category) => {
                    extracted.pois.push(poi(common(OsmType::Way, w.id, centroid, &tags), category, &tags));
                }
            }
        }
    }

    for f in features {
        let points: Vec<Pt> = f.outer.iter().filter_map(|id| way_geoms.get(id)).flatten().copied().collect();
        let Some(centroid) = geom::centroid(&points) else { continue };
        let tags = as_refs(&f.tags);
        if let Some(category) = f.category {
            extracted.pois.push(poi(common(OsmType::Relation, f.id, centroid, &tags), category, &tags));
        }
        if f.address
            && let Some(housenumber) = get(&tags, "addr:housenumber")
        {
            extracted.addresses.push(address(OsmType::Relation, f.id, centroid, housenumber, &tags));
        }
    }

    for r in relations {
        let collect = |ids: &[i64]| ids.iter().filter_map(|id| way_geoms.get(id).cloned()).collect::<Vec<_>>();
        let outers = geom::assemble_rings(collect(&r.outer));
        if outers.is_empty() {
            continue;
        }
        let inners = geom::assemble_rings(collect(&r.inner));
        let tags = as_refs(&r.tags);
        let Some(admin_level) = get(&tags, "admin_level").and_then(|l| l.parse::<u8>().ok()) else { continue };
        let largest = outers
            .iter()
            .max_by_key(|ring| {
                let b = geom::BBox::of(ring.iter().copied());
                (b.max.0 as i64 - b.min.0 as i64) * (b.max.1 as i64 - b.min.1 as i64)
            })
            .expect("at least one outer ring");
        let point = r
            .label
            .and_then(|id| coord(&id))
            .or_else(|| geom::centroid(largest))
            .expect("non-empty ring");
        let rings: Vec<Vec<Pt>> = outers.iter().chain(&inners).cloned().collect();
        extracted.admins.push(Admin {
            common: common(OsmType::Relation, r.id, point, &tags),
            admin_level,
            iso3166_1: owned(&tags, "ISO3166-1:alpha2").or_else(|| owned(&tags, "ISO3166-1")),
            iso3166_2: owned(&tags, "ISO3166-2"),
            official_ref: owned(&tags, "ine:municipio").or_else(|| owned(&tags, "ref:ine")).or_else(|| owned(&tags, "ref")),
            population: population(&tags),
            rings,
            polygons: geom::build_polygons(outers, inners),
        });
    }
    tracing::debug!(elapsed = ?t.elapsed(), "assembled");
    Ok(extracted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_keys() {
        for k in ["name", "name:ca", "alt_name", "old_name", "old_name:es", "official_name"] {
            assert!(is_name_key(k), "{k}");
        }
        for k in ["name:etymology:wikidata", "name:prefix", "names", "alt_names", "highway"] {
            assert!(!is_name_key(k), "{k}");
        }
    }

    #[test]
    fn classification() {
        let street = [("highway", "residential"), ("name", "Carrer de Mallorca")];
        assert_eq!(street_class(&street), Some("residential"));
        assert_eq!(street_class(&[("highway", "residential")]), None);
        assert_eq!(street_class(&[("highway", "motorway"), ("ref", "AP-7")]), Some("motorway"));
        assert_eq!(street_class(&[("highway", "bus_stop"), ("name", "x")]), None);
        assert_eq!(street_class(&[("highway", "footway"), ("footway", "sidewalk"), ("name", "Carrer de Mallorca")]), None);
        assert_eq!(street_class(&[("highway", "footway"), ("name", "Passatge de Permanyer")]), Some("footway"));

        assert_eq!(poi_category(&[("amenity", "pharmacy"), ("name", "Farmàcia")]).as_deref(), Some("amenity=pharmacy"));
        assert_eq!(poi_category(&[("amenity", "bench")]), None);
        assert_eq!(poi_category(&[("shop", "no"), ("name", "x")]), None);
    }

    #[test]
    fn population_parsing() {
        assert_eq!(population(&[("population", "1.620.343")]), Some(1_620_343));
        assert_eq!(population(&[("population", "about 10")]), None);
    }

    #[test]
    fn node_with_address_and_shop_yields_both() {
        let tags = [("addr:housenumber", "401"), ("addr:street", "Carrer de Mallorca"), ("shop", "bakery"), ("name", "Forn")];
        let mut out = Vec::new();
        node_features(1, (21_686_000, 413_874_000), &tags, &mut out);
        assert!(matches!(out[0], NodeOut::Address(ref a) if a.housenumber == "401" && a.street.as_deref() == Some("Carrer de Mallorca")));
        assert!(matches!(out[1], NodeOut::Poi(ref p) if p.category == "shop=bakery"));
        assert_eq!(out.len(), 2);
    }
}

//! Places and POIs (PROJECT.md §6.5.2). A `place=town` node and the municipality boundary of
//! the same town are one entity; so are a `place=suburb` node and its district boundary.

use std::collections::{BTreeMap, BTreeSet};

use geko_core::model::PlaceKind;
use geko_core::ports::TextNormalizer;
use geko_geom::{Pt, distance_m};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};

use super::addresses::AttrSource;
use super::admin::{AdminIndex, Chain};
use super::records::{AdminRec, Names, OSM, PlaceRec, PoiRec};
use super::streets::StreetEntity;
use super::uf::UnionFind;

#[derive(Debug, Clone)]
pub struct PlaceEntity {
    pub kind: PlaceKind,
    pub name: String,
    pub names: Names,
    pub point: Pt,
    pub population: Option<i64>,
    pub wikidata: Option<String>,
    /// Index of the administrative boundary this place is, if any.
    pub admin: Option<usize>,
    pub official_ref: Option<String>,
    pub iso3166_2: Option<String>,
    pub place_type: Option<String>,
    pub chain: Chain,
    pub sources: BTreeSet<&'static str>,
    pub source_ids: Vec<(&'static str, String)>,
    /// Municipalities: every postcode with its addresses and sources.
    pub postcodes: Vec<super::PostcodeShare>,
    /// Point-like places: the postcode of their zone, always inferred.
    pub postcode: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PoiEntity {
    pub name: String,
    pub names: Names,
    pub category: String,
    pub point: Pt,
    pub wikidata: Option<String>,
    pub phone: Option<String>,
    pub website: Option<String>,
    pub chain: Chain,
    pub sources: BTreeSet<&'static str>,
    pub source_ids: Vec<(&'static str, String)>,
    pub postcode: Option<String>,
    pub postcode_source: Option<AttrSource>,
    /// Nearest address entity, inferred (PROJECT.md §6.5.4).
    pub address: Option<usize>,
}

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct PlaceStats {
    pub admin_entities: usize,
    pub place_records: usize,
    pub merged_into_admin: usize,
    pub merged_into_street: usize,
    pub poi_records: usize,
    pub poi_entities: usize,
}

fn folded_names(name: &str, names: &Names, normalizer: &dyn TextNormalizer) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut buf = String::new();
    for n in std::iter::once(name).chain(names.values().map(String::as_str)) {
        normalizer.normalize(n, &mut buf);
        if !buf.is_empty() {
            out.insert(buf.clone());
        }
    }
    out
}

fn place_kind(place: &str) -> PlaceKind {
    match place {
        "suburb" | "quarter" | "neighbourhood" | "borough" => PlaceKind::Neighbourhood,
        "square" => PlaceKind::Toponym,
        _ => PlaceKind::Locality,
    }
}

fn merge_names(into: &mut Names, from: &Names) {
    for (k, v) in from {
        into.entry(k.clone()).or_insert_with(|| v.clone());
    }
}

pub fn build_places(
    admins: &[AdminRec],
    places: &[PlaceRec],
    admin_index: &AdminIndex,
    streets: &[StreetEntity],
    normalizer: &dyn TextNormalizer,
) -> (Vec<PlaceEntity>, PlaceStats) {
    let mut stats = PlaceStats { place_records: places.len(), ..Default::default() };
    let mut entities = Vec::new();
    let mut admin_entity: BTreeMap<usize, usize> = BTreeMap::new();
    for (i, a) in admins.iter().enumerate() {
        let (Some(kind), Some(name)) = (admin_index.kind(a.level), a.name.as_ref()) else { continue };
        if kind == PlaceKind::Country {
            continue;
        }
        admin_entity.insert(i, entities.len());
        let mut chain = admin_index.chain(a.point);
        if kind == PlaceKind::Municipality {
            chain.municipality = Some(i);
        }
        entities.push(PlaceEntity {
            kind,
            name: name.clone(),
            names: a.names.clone(),
            point: a.point,
            population: a.population,
            wikidata: a.wikidata.clone(),
            admin: Some(i),
            official_ref: a.official_ref.clone(),
            iso3166_2: a.iso3166_2.clone(),
            place_type: None,
            chain,
            sources: BTreeSet::from([OSM]),
            source_ids: vec![(OSM, format!("r{}", a.osm_id))],
            postcodes: Vec::new(),
            postcode: None,
        });
    }
    let admin_folded: BTreeMap<usize, BTreeSet<String>> = admin_entity
        .iter()
        .map(|(&a, &e)| (a, folded_names(&entities[e].name, &entities[e].names, normalizer)))
        .collect();

    // A neighbourhood-level unit named as the municipality that contains it is its main
    // settlement (a "localité" of a commune): one entity for whoever searches it.
    let mut removed = vec![false; entities.len()];
    for (&a, &e) in &admin_entity.clone() {
        let Some(m) = entities[e].chain.municipality.filter(|&m| m != a) else { continue };
        if entities[e].kind != PlaceKind::Neighbourhood || !admin_folded[&a].iter().any(|n| admin_folded.get(&m).is_some_and(|ns| ns.contains(n))) {
            continue;
        }
        let Some(&into) = admin_entity.get(&m) else { continue };
        let (names, source_id) = (entities[e].names.clone(), entities[e].source_ids.clone());
        merge_names(&mut entities[into].names, &names);
        entities[into].source_ids.extend(source_id);
        removed[e] = true;
        admin_entity.insert(a, into);
        stats.merged_into_admin += 1;
    }
    stats.admin_entities = entities.len() - removed.iter().filter(|r| **r).count();
    let mut street_by_core: BTreeMap<(Option<usize>, &str), Vec<usize>> = BTreeMap::new();
    for (i, s) in streets.iter().enumerate() {
        if matches!(s.type_code, Some("PZ") | None) {
            street_by_core.entry((s.municipality, s.core.as_str())).or_default().push(i);
        }
    }

    let chains: Vec<Chain> = places.par_iter().map(|p| admin_index.chain(p.point)).collect();
    for (p, chain) in places.iter().zip(chains) {
        let kind = place_kind(&p.place);
        let folded = folded_names(&p.name, &p.names, normalizer);
        let target_admin = match kind {
            PlaceKind::Locality if matches!(p.place.as_str(), "city" | "town" | "village") => chain.municipality,
            PlaceKind::Neighbourhood => chain.neighbourhood,
            _ => None,
        };
        if let Some(a) = target_admin
            && admin_folded.get(&a).is_some_and(|names| !names.is_disjoint(&folded))
        {
            let e = &mut entities[admin_entity[&a]];
            merge_names(&mut e.names, &p.names);
            e.population = e.population.or(p.population);
            e.wikidata = e.wikidata.clone().or_else(|| p.wikidata.clone());
            e.place_type = Some(p.place.clone());
            e.source_ids.push((p.source, p.source_id.clone()));
            stats.merged_into_admin += 1;
            continue;
        }
        if kind == PlaceKind::Toponym {
            let core = normalizer.street_parts(&p.name).core;
            let is_street = street_by_core
                .get(&(chain.municipality, core.as_str()))
                .is_some_and(|ss| ss.iter().any(|&s| streets[s].lines.iter().flatten().any(|q| distance_m(*q, p.point) < 150.0)));
            if is_street {
                stats.merged_into_street += 1;
                continue;
            }
        }
        entities.push(PlaceEntity {
            kind,
            name: p.name.clone(),
            names: p.names.clone(),
            point: p.point,
            population: p.population,
            wikidata: p.wikidata.clone(),
            admin: None,
            official_ref: None,
            iso3166_2: None,
            place_type: Some(p.place.clone()),
            chain,
            sources: BTreeSet::from([p.source]),
            source_ids: vec![(p.source, p.source_id.clone())],
            postcodes: Vec::new(),
            postcode: None,
        });
    }
    let mut i = 0;
    entities.retain(|_| {
        i += 1;
        !removed.get(i - 1).copied().unwrap_or(false)
    });
    (entities, stats)
}

/// Parts of a transit station mapped as separate nodes; they are not POIs of their own
/// (Barcelona-Sants is one station node plus several of these).
const TRANSIT_PARTS: &[&str] = &[
    "railway=stop",
    "railway=platform",
    "railway=subway_entrance",
    "railway=train_station_entrance",
    "public_transport=stop_position",
    "public_transport=platform",
    "public_transport=stop_area",
];
const TRANSIT: &str = "transit";
/// Homonymous stations of one family (train, metro, bus) within this distance are one.
const TRANSIT_MERGE_M: f64 = 200.0;

fn poi_family(category: &str) -> &str {
    match category.split_once('=') {
        Some(("railway" | "public_transport", _)) | Some(("amenity", "bus_station")) => TRANSIT,
        _ => category,
    }
}

/// POIs with the same name and category within `merge_m` are one (e.g. a shop mapped both as
/// a node and as its building); stations of the same transit family within 200 m too.
pub fn build_pois(
    pois: &[PoiRec],
    admin_index: &AdminIndex,
    normalizer: &dyn TextNormalizer,
    merge_m: f64,
    stats: &mut PlaceStats,
) -> Vec<PoiEntity> {
    stats.poi_records = pois.len();
    let mut groups: BTreeMap<(String, &str), Vec<usize>> = BTreeMap::new();
    let mut buf = String::new();
    for (i, p) in pois.iter().enumerate() {
        if TRANSIT_PARTS.contains(&p.category.as_str()) {
            continue;
        }
        normalizer.normalize(&p.name, &mut buf);
        groups.entry((buf.clone(), poi_family(&p.category))).or_default().push(i);
    }
    let clusters: Vec<Vec<usize>> = groups
        .into_par_iter()
        .flat_map_iter(|((_, family), members)| {
            let max_m = if family == TRANSIT { TRANSIT_MERGE_M } else { merge_m };
            let mut uf = UnionFind::new(members.len());
            for a in 0..members.len() {
                for b in a + 1..members.len() {
                    if distance_m(pois[members[a]].point, pois[members[b]].point) < max_m {
                        uf.union(a as u32, b as u32);
                    }
                }
            }
            uf.groups().into_iter().map(|g| g.into_iter().map(|k| members[k as usize]).collect::<Vec<_>>()).collect::<Vec<_>>()
        })
        .collect();
    let entities: Vec<PoiEntity> = clusters
        .into_par_iter()
        .map(|mut members| {
            // The public id comes from the first member: the smallest source id, not whichever
            // record came first.
            members.sort_by(|&a, &b| (pois[a].source, &pois[a].source_id).cmp(&(pois[b].source, &pois[b].source_id)));
            let first = &pois[members[0]];
            let mut names = Names::new();
            for &i in &members {
                merge_names(&mut names, &pois[i].names);
            }
            PoiEntity {
                name: first.name.clone(),
                names,
                category: first.category.clone(),
                point: first.point,
                wikidata: members.iter().find_map(|&i| pois[i].wikidata.clone()),
                phone: members.iter().find_map(|&i| pois[i].phone.clone()),
                website: members.iter().find_map(|&i| pois[i].website.clone()),
                chain: admin_index.chain(first.point),
                sources: members.iter().map(|&i| pois[i].source).collect(),
                source_ids: members.iter().map(|&i| (pois[i].source, pois[i].source_id.clone())).collect(),
                postcode: None,
                postcode_source: None,
                address: None,
            }
        })
        .collect();
    let entities = merge_across_sources(entities, normalizer);
    stats.poi_entities = entities.len();
    entities
}

/// A POI of another source within this distance of an OSM one with a compatible name is that
/// POI: sources place the same shop tens of metres apart, and file it under other categories.
const CROSS_SOURCE_M: f64 = 75.0;

/// A phone or a web identifies a place only when at most this many POIs of the partition have it:
/// a chain, a hotel and its restaurant, or a town hall and its offices share theirs.
const RARE_CONTACT: usize = 4;

/// Digits of a phone, without the international prefix `00`.
fn phone_digits(phone: &str) -> String {
    let digits: String = phone.chars().filter(char::is_ascii_digit).collect();
    digits.strip_prefix("00").map(str::to_owned).unwrap_or(digits)
}

/// Whether two phones are the same number, written with or without the country code.
fn same_phone(a: &str, b: &str) -> bool {
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    short.len() >= 7 && long.ends_with(short)
}

/// The host of a web, without `www.` or `m.`: `https://www.example.com/es` is `example.com`.
fn web_host(url: &str) -> String {
    let url = url.trim().to_lowercase();
    let rest = url.split_once("://").map_or(url.as_str(), |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    host.strip_prefix("www.").or_else(|| host.strip_prefix("m.")).unwrap_or(host).to_owned()
}

/// Two categories are the same kind when they are equal, or when POIs of both sources with the
/// same name next to each other have them at least this many times in the partition: each source
/// sorts places its own way ("shop=supermarket" and "shop=convenience").
const SEEN_TOGETHER: usize = 3;

/// A word tells a place apart only when at most this many POIs of the partition have it in their
/// name: "asador" or "viajes" are in many.
const RARE_WORD: usize = 20;

/// Words of a normalized name that can tell a place apart: three letters or more.
fn name_words(name: &str) -> impl Iterator<Item = &str> {
    name.split(' ').filter(|w| w.chars().count() >= 3)
}

/// Whether two normalized names have something in common: a word for which `rare` holds, or one
/// written inside the other without spaces ("canribs braseria" and "can ribs").
fn share_a_word(a: &str, b: &str, rare: impl Fn(&str) -> bool) -> bool {
    if name_words(a).any(|w| rare(w) && name_words(b).any(|x| x == w)) {
        return true;
    }
    let (ja, jb) = (a.replace(' ', ""), b.replace(' ', ""));
    let (short, long) = if ja.len() <= jb.len() { (&ja, &jb) } else { (&jb, &ja) };
    short.len() >= 4 && long.contains(short.as_str())
}

/// The key of an OSM-style category: `amenity` of `amenity=restaurant`.
fn category_key(category: &str) -> &str {
    category.split_once('=').map_or(category, |(k, _)| k)
}

/// Whether two normalized names name the same place: equal, or one is the other with more words
/// ("carrefour express" and "carrefour express calle oms 35").
fn compatible_names(a: &str, b: &str) -> bool {
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    if short.len() < 4 {
        return false;
    }
    // Every place where `short` appears in `long` with a space or an end on both sides.
    long.match_indices(short).any(|(i, _)| {
        let before = i == 0 || long.as_bytes()[i - 1] == b' ';
        let after = i + short.len() == long.len() || long.as_bytes()[i + short.len()] == b' ';
        before && after
    })
}

/// Merges each POI that only other sources have into the OSM POI it is (PROJECT.md §6.5.2): OSM
/// keeps its position and category, and the other names join as variants.
fn merge_across_sources(entities: Vec<PoiEntity>, normalizer: &dyn TextNormalizer) -> Vec<PoiEntity> {
    let normalized = |name: &str| {
        let mut buf = String::new();
        normalizer.normalize(name, &mut buf);
        buf
    };
    // A grid of the OSM POIs in cells of about 110 m, wider than the distance searched, so the
    // cell of a POI and its neighbours hold every candidate. A degree of longitude shrinks with
    // the latitude: each row of cells is as many degrees wide as keeps it 110 m.
    const CELL: i32 = 10_000;
    let width = |row: i32| {
        let lat = ((row as f64 + 0.5) * CELL as f64 * 1e-7).clamp(-89.0, 89.0);
        (CELL as f64 / lat.to_radians().cos()).ceil() as i32
    };
    let cell_in = |row: i32, lon: i32| (lon.div_euclid(width(row)), row);
    let cell = |p: Pt| cell_in(p.1.div_euclid(CELL), p.0);
    let (mut base, others): (Vec<PoiEntity>, Vec<PoiEntity>) = entities.into_iter().partition(|e| e.sources.contains(OSM));
    let base_names: Vec<String> = base.iter().map(|e| normalized(&e.name)).collect();
    let contacts = |e: &PoiEntity| (e.phone.as_deref().map(phone_digits), e.website.as_deref().map(web_host));
    let base_contacts: Vec<(Option<String>, Option<String>)> = base.iter().map(contacts).collect();
    let other_contacts: Vec<(Option<String>, Option<String>)> = others.iter().map(contacts).collect();
    // Phones are counted by their last seven digits, so that one written with the country code
    // and one without count as the same.
    let phone_key = |p: &str| p[p.len().saturating_sub(7)..].to_owned();
    let mut phone_pois: FxHashMap<String, usize> = FxHashMap::default();
    let mut web_pois: FxHashMap<String, usize> = FxHashMap::default();
    for (phone, web) in base_contacts.iter().chain(&other_contacts) {
        if let Some(p) = phone.as_deref().filter(|p| p.len() >= 7) {
            *phone_pois.entry(phone_key(p)).or_default() += 1;
        }
        if let Some(w) = web.as_deref().filter(|w| !w.is_empty()) {
            *web_pois.entry(w.to_owned()).or_default() += 1;
        }
    }
    let mut word_pois: FxHashMap<String, usize> = FxHashMap::default();
    let other_names: Vec<String> = others.iter().map(|e| normalized(&e.name)).collect();
    for name in base_names.iter().chain(&other_names) {
        let mut words: Vec<&str> = name_words(name).collect();
        words.sort_unstable();
        words.dedup();
        for w in words {
            *word_pois.entry(w.to_owned()).or_default() += 1;
        }
    }
    let rare = |w: &str| word_pois.get(w).copied().unwrap_or(0) <= RARE_WORD;
    let same_contact = |a: &(Option<String>, Option<String>), b: &(Option<String>, Option<String>)| {
        let phone = matches!((&a.0, &b.0), (Some(x), Some(y)) if same_phone(x, y) && phone_pois.get(&phone_key(x)).copied().unwrap_or(0) <= RARE_CONTACT);
        let web = matches!((&a.1, &b.1), (Some(x), Some(y)) if !x.is_empty() && x == y && web_pois.get(x).copied().unwrap_or(0) <= RARE_CONTACT);
        phone || web
    };
    let mut grid: FxHashMap<(i32, i32), Vec<usize>> = FxHashMap::default();
    for (i, e) in base.iter().enumerate() {
        grid.entry(cell(e.point)).or_default().push(i);
    }
    let near_of = |p: Pt| -> Vec<(f64, usize)> {
        let row = p.1.div_euclid(CELL);
        (row - 1..=row + 1)
            .flat_map(|r| {
                let (cx, _) = cell_in(r, p.0);
                (cx - 1..=cx + 1).map(move |x| (x, r))
            })
            .filter_map(|c| grid.get(&c))
            .flatten()
            .map(|&i| (distance_m(p, base[i].point), i))
            .filter(|&(d, _)| d < CROSS_SOURCE_M)
            .collect()
    };
    let near: Vec<Vec<(f64, usize)>> = others.iter().map(|o| near_of(o.point)).collect();
    // The categories each source gives the same place, learned from the names that are equal.
    let mut together: FxHashMap<(&str, &str), usize> = FxHashMap::default();
    for ((other, name), near) in others.iter().zip(&other_names).zip(&near) {
        for &(_, i) in near {
            if base_names[i] == *name && base[i].category != other.category {
                *together.entry((base[i].category.as_str(), other.category.as_str())).or_default() += 1;
            }
        }
    }
    let together: FxHashSet<(String, String)> =
        together.into_iter().filter(|&(_, n)| n >= SEEN_TOGETHER).map(|((a, b), _)| (a.to_owned(), b.to_owned())).collect();
    let same_kind = |a: &str, b: &str| a == b || together.contains(&(a.to_owned(), b.to_owned()));
    let mut kept = Vec::new();
    for (((other, other_contact), name), near) in others.into_iter().zip(&other_contacts).zip(other_names).zip(near) {
        let closest = |found: &mut dyn Iterator<Item = (f64, usize)>| found.min_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        // An equal name is the same place; a name inside the other ("Fátima", "Parroquia de
        // Nuestra Señora de Fátima") only when it is also the same kind of place.
        let mut target = closest(&mut near.iter().copied().filter(|&(_, i)| {
            base_names[i] == name || (compatible_names(&name, &base_names[i]) && same_kind(&base[i].category, &other.category))
        }));
        // A name told otherwise ("Restaurant Ca'n Salvador", "Can Salvador") is the same place
        // only with more proof: the same rare phone or web, a word in common and the same kind.
        if target.is_none() {
            target = closest(&mut near.iter().copied().filter(|&(_, i)| {
                category_key(&base[i].category) == category_key(&other.category)
                    && same_contact(&base_contacts[i], other_contact)
                    && share_a_word(&name, &base_names[i], rare)
            }));
        }
        match target {
            Some((_, i)) => {
                let into = &mut base[i];
                merge_names(&mut into.names, &other.names);
                if other.name != into.name {
                    let key = format!("{}:name", other.sources.iter().next().copied().unwrap_or("other"));
                    into.names.entry(key).or_insert(other.name.clone());
                }
                into.wikidata = into.wikidata.take().or(other.wikidata);
                into.phone = into.phone.take().or(other.phone);
                into.website = into.website.take().or(other.website);
                into.sources.extend(other.sources);
                into.source_ids.extend(other.source_ids);
            }
            None => kept.push(other),
        }
    }
    base.extend(kept);
    base
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_of_one_place_across_sources() {
        assert!(compatible_names("carrefour express", "carrefour express calle oms 35"));
        assert!(compatible_names("farmacia central", "farmacia central"));
        assert!(compatible_names("la seu", "catedral la seu"));
        assert!(!compatible_names("bar", "bar sol"), "too short to tell");
        assert!(!compatible_names("cafe sol", "cafe solar"), "a word, not a part of one");
    }

    fn poi(source: &'static str, name: &str, category: &str, point: Pt) -> PoiEntity {
        PoiEntity {
            name: name.into(),
            names: Names::new(),
            category: category.into(),
            point,
            wikidata: None,
            phone: None,
            website: None,
            chain: Chain::default(),
            sources: BTreeSet::from([source]),
            source_ids: vec![(source, name.into())],
            postcode: None,
            postcode_source: None,
            address: None,
        }
    }

    #[test]
    fn a_poi_of_another_source_joins_the_osm_one_it_is() {
        let normalizer = pack_free::Plain;
        let mut entities = vec![
            poi(OSM, "Carrefour Express", "shop=convenience", (0, 0)),
            poi("overture", "Carrefour Express Calle Oms 35", "shop=supermarket", (300, 300)),
            poi("overture", "Pizza Industria", "amenity=restaurant", (500, 500)),
            poi("overture", "Carrefour Express", "shop=convenience", (90_000, 0)),
        ];
        // The two sources call the same shops by these two categories three times.
        for i in 0..3 {
            let at = (300_000 + i * 100_000, 0);
            entities.push(poi(OSM, &format!("Super {i}"), "shop=convenience", at));
            entities.push(poi("overture", &format!("Super {i}"), "shop=supermarket", at));
        }
        let merged = merge_across_sources(entities, &normalizer);
        assert_eq!(merged.len(), 6, "joined the near one, kept the new and the far one");
        let joined = &merged[0];
        assert_eq!(joined.category, "shop=convenience", "OSM keeps its category");
        assert_eq!(joined.point, (0, 0), "and its position");
        assert!(joined.sources.contains("overture"));
        assert_eq!(joined.names.get("overture:name").map(String::as_str), Some("Carrefour Express Calle Oms 35"));
    }

    #[test]
    fn a_name_inside_another_is_not_enough_for_another_kind_of_place() {
        let normalizer = pack_free::Plain;
        let entities = vec![
            poi(OSM, "Fatima", "amenity=taxi", (0, 0)),
            poi("overture", "Parroquia de Nuestra Senora de Fatima", "amenity=place_of_worship", (0, 300)),
            poi(OSM, "Bar Rita", "amenity=bar", (100_000, 0)),
            poi("overture", "Bar Rita", "amenity=pub", (100_000, 300)),
        ];
        let merged = merge_across_sources(entities, &normalizer);
        assert_eq!(merged.len(), 3, "the church stays; an equal name joins whatever its category");
    }

    #[test]
    fn a_name_told_otherwise_merges_only_with_the_same_rare_contact_a_word_and_the_kind() {
        let normalizer = pack_free::Plain;
        let with = |mut e: PoiEntity, phone: &str, web: &str| {
            e.phone = (!phone.is_empty()).then(|| phone.into());
            e.website = (!web.is_empty()).then(|| web.into());
            e
        };
        let mut entities = vec![
            with(poi(OSM, "Can Salvador", "amenity=restaurant", (0, 0)), "971 12 34 56", ""),
            with(poi("overture", "Restaurant Ca'n Salvador", "amenity=restaurant", (0, 300)), "+34971123456", ""),
            // The web of a hotel is also that of its restaurant: not the same kind.
            with(poi(OSM, "El Cielo del Born", "amenity=restaurant", (100_000, 0)), "", "https://www.niviaborn.com/es"),
            with(poi("overture", "Nivia Born Boutique Hotel", "tourism=hotel", (100_000, 300)), "", "http://niviaborn.com"),
            // Same phone, nothing in the name.
            with(poi(OSM, "Sa Cova", "shop=bakery", (200_000, 0)), "971000111", ""),
            with(poi("overture", "Pastisseria La Larena", "shop=bakery", (200_000, 300)), "971000111", ""),
        ];
        // A web many places share identifies none of them.
        for i in 0..5 {
            entities.push(with(poi(OSM, &format!("Cafe {i}"), "amenity=cafe", (900_000 + i * 50_000, 0)), "", "facebook.com/x"));
        }
        entities.push(with(poi(OSM, "Bar Sol", "amenity=bar", (300_000, 0)), "", "https://facebook.com/barsol"));
        entities.push(with(poi("overture", "Sol Bar Terraza", "amenity=bar", (300_000, 300)), "", "https://m.facebook.com/x"));
        let merged = merge_across_sources(entities, &normalizer);
        let names: Vec<&str> = merged.iter().filter(|e| e.point.0 < 400_000).map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Can Salvador", "El Cielo del Born", "Sa Cova", "Bar Sol", "Nivia Born Boutique Hotel", "Pastisseria La Larena", "Sol Bar Terraza"]);
    }

    #[test]
    fn contacts_are_compared_as_numbers_and_hosts() {
        assert!(same_phone(&phone_digits("+34 971 12 34 56"), &phone_digits("971123456")));
        assert!(same_phone(&phone_digits("0034971123456"), &phone_digits("971-123-456")));
        assert!(!same_phone(&phone_digits("112"), &phone_digits("112")), "too short to tell a place");
        assert_eq!(web_host("HTTPS://www.Example.com/es?x=1"), "example.com");
        assert_eq!(web_host("m.example.com"), "example.com");
        let any = |_: &str| true;
        assert!(share_a_word("canribs braseria", "can ribs", |_| false), "one inside the other");
        assert!(!share_a_word("sa cova", "pastisseria la larena", any));
        assert!(!share_a_word("asador el tahiti", "asador el cortijo", |w| w != "asador"), "a common word is no proof");
    }

    #[test]
    fn the_grid_finds_neighbours_where_degrees_of_longitude_are_short() {
        let normalizer = pack_free::Plain;
        // At 70° N a degree of longitude is about 38 km: 60 m is about 0.0016°, more than one
        // cell of 0.001° and a half.
        let lat = 700_000_000;
        for lon in [-5_000, 0, 7_777, 123_456_789] {
            let entities = vec![poi(OSM, "Farmacia Nord", "amenity=pharmacy", (lon, lat)), poi("overture", "Farmacia Nord", "amenity=pharmacy", (lon + 16_000, lat))];
            assert_eq!(merge_across_sources(entities, &normalizer).len(), 1, "at lon {lon}");
        }
    }

    mod pack_free {
        /// Lowercase and spaces only: enough to compare names in tests.
        pub struct Plain;

        impl geko_core::ports::TextNormalizer for Plain {
            fn country(&self) -> geko_core::country::CountryCode {
                geko_core::country::CountryCode::new("ES").unwrap()
            }
            fn normalize(&self, text: &str, out: &mut String) {
                *out = text.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ");
            }
            fn tokenize(&self, normalized: &str, out: &mut Vec<String>) {
                *out = normalized.split(' ').map(str::to_owned).collect();
            }
            fn variants(&self, name: &str, out: &mut Vec<String>) {
                *out = vec![name.to_lowercase()];
            }
            fn street_parts(&self, name: &str) -> geko_core::model::StreetParts {
                geko_core::model::StreetParts { type_code: None, core: name.to_lowercase() }
            }
            fn parse_housenumber(&self, _raw: &str) -> Option<geko_core::model::HouseNumber> {
                None
            }
            fn street_types_compatible(&self, _a: Option<&str>, _b: Option<&str>) -> bool {
                true
            }
        }
    }
}

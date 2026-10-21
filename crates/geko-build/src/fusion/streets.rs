//! Street entities (PROJECT.md §6.5.2): OSM segments aggregated into streets, then every
//! address of every source attached to its street. Addresses whose street exists in no
//! geometry source create new streets.

use std::collections::{BTreeMap, BTreeSet};

use geko_core::model::StreetParts;
use geko_core::ports::TextNormalizer;
use geko_geom::{BBox, Pt, distance_m, point_line_m};
use rayon::prelude::*;
use rustc_hash::FxHashMap;

use super::admin::AdminIndex;
use super::records::{AddrRec, Names, StreetSeg};
use super::uf::UnionFind;

#[derive(Debug, Clone)]
pub struct StreetParams {
    /// Two geometry segments with the same name and street type are the same street if they
    /// are closer than this. City streets are often interrupted by squares, parks or railways
    /// (e.g. Carrer de Mallorca in Sants), so this is well above mere connectivity.
    pub segment_join_m: f64,
    /// An address is attached to a street with its exact name only within this distance.
    /// Measured on Barcelona: most misses were between 150 and 300 m, past 600 m they are
    /// mostly homonymous streets in another village of the same municipality.
    pub address_street_m: f64,
    /// Same, for a name that only matches approximately.
    pub address_street_fuzzy_m: f64,
    /// Addresses of a street missing from geometry sources form one street within this distance.
    pub address_cluster_m: f64,
}

impl Default for StreetParams {
    fn default() -> Self {
        Self { segment_join_m: 500.0, address_street_m: 300.0, address_street_fuzzy_m: 150.0, address_cluster_m: 400.0 }
    }
}

#[derive(Debug, Clone)]
pub struct StreetEntity {
    pub municipality: Option<usize>,
    pub type_code: Option<&'static str>,
    pub core: String,
    pub display_name: String,
    pub names: Names,
    /// Every name under which a source knows this street.
    pub aliases: BTreeSet<String>,
    pub lines: Vec<Vec<Pt>>,
    /// Points of addresses that created the street, when no source has its geometry.
    pub points: Vec<Pt>,
    /// Population entity (village, urbanisation) most of its addresses belong to, per the
    /// sources that give one; tells apart homonymous streets of one municipality.
    pub locality: Option<String>,
    pub highway: Option<String>,
    pub sources: BTreeSet<&'static str>,
    pub source_ids: Vec<String>,
}

impl StreetEntity {
    pub fn representative_point(&self) -> Option<Pt> {
        if let Some(line) = self.lines.iter().max_by_key(|l| l.len()) {
            return line.get(line.len() / 2).copied();
        }
        let mut pts = self.points.clone();
        pts.sort();
        pts.get(pts.len() / 2).copied()
    }

    fn distance_m(&self, p: Pt) -> f64 {
        let to_lines = self.lines.iter().map(|l| point_line_m(p, l)).fold(f64::INFINITY, f64::min);
        let to_points = self.points.iter().map(|q| distance_m(p, *q)).fold(f64::INFINITY, f64::min);
        to_lines.min(to_points)
    }
}

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct StreetStats {
    pub segments: usize,
    pub entities_from_geometry: usize,
    pub entities_from_addresses: usize,
    pub addresses_exact: usize,
    pub addresses_fuzzy: usize,
    pub addresses_new_street: usize,
    pub addresses_without_street: usize,
}

pub struct Streets {
    pub entities: Vec<StreetEntity>,
    /// Street of each address record, by index.
    pub address_street: Vec<Option<usize>>,
    pub stats: StreetStats,
}

/// Candidate streets per (municipality, name of the address), and whether the match is fuzzy.
type Candidates<'a> = FxHashMap<(Option<usize>, &'a StreetParts), (Vec<usize>, bool)>;

fn highway_rank(h: Option<&str>) -> u8 {
    match h {
        Some("motorway" | "motorway_link") => 7,
        Some("trunk" | "trunk_link") => 6,
        Some("primary" | "primary_link") => 5,
        Some("secondary" | "secondary_link") => 4,
        Some("tertiary" | "tertiary_link") => 3,
        Some("unclassified" | "residential" | "living_street" | "pedestrian") => 2,
        Some(_) => 1,
        None => 0,
    }
}

/// Degrees·1e7 that cover at least `m` metres in any direction at Spanish latitudes and below 60°.
fn margin_e7(m: f64) -> i32 {
    (m * 200.0).ceil() as i32
}

fn expand(b: BBox, m: f64) -> BBox {
    let d = margin_e7(m);
    BBox { min: (b.min.0 - d, b.min.1 - d), max: (b.max.0 + d, b.max.1 + d) }
}

fn lines_near(a: &[Pt], b: &[Pt], max_m: f64) -> bool {
    if a.first().is_some_and(|p| b.contains(p)) || a.last().is_some_and(|p| b.contains(p)) {
        return true;
    }
    let step = (a.len() / 32).max(1);
    a.iter().step_by(step).chain(a.last()).any(|p| point_line_m(*p, b) < max_m)
        || [b.first(), b.last()].into_iter().flatten().any(|p| point_line_m(*p, a) < max_m)
}

fn most_frequent<'a>(values: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for v in values {
        *counts.entry(v).or_default() += 1;
    }
    counts.into_iter().max_by_key(|(v, n)| (*n, std::cmp::Reverse(*v))).map(|(v, _)| v)
}

/// Display name for an all-caps source name: "CALLE CONSELL DE CENT" → "Calle Consell de Cent".
pub fn title_case(name: &str) -> String {
    const LOWER: &[&str] = &["de", "del", "dels", "la", "las", "el", "els", "los", "les", "y", "i", "e", "d'", "l'", "da", "do"];
    name.split_whitespace()
        .enumerate()
        .map(|(i, w)| {
            let lower = w.to_lowercase();
            if i > 0 && LOWER.contains(&lower.as_str()) {
                return lower;
            }
            let mut chars = lower.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn levenshtein_le1(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.len().abs_diff(b.len()) > 1 {
        return false;
    }
    let (mut i, mut j, mut edits) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            i += 1;
            j += 1;
            continue;
        }
        edits += 1;
        if edits > 1 {
            return false;
        }
        match a.len().cmp(&b.len()) {
            std::cmp::Ordering::Greater => i += 1,
            std::cmp::Ordering::Less => j += 1,
            std::cmp::Ordering::Equal => {
                i += 1;
                j += 1;
            }
        }
    }
    edits + (a.len() - i) + (b.len() - j) <= 1
}

fn sorted_tokens(core: &str) -> String {
    let mut t: Vec<&str> = core.split(' ').collect();
    t.sort_unstable();
    t.join(" ")
}

/// Same street name allowing word order changes and one typo in names of 7+ characters.
fn fuzzy_core(a: &str, b: &str) -> bool {
    a != b && (sorted_tokens(a) == sorted_tokens(b) || (a.len() >= 7 && b.len() >= 7 && levenshtein_le1(a, b)))
}

pub fn build(
    segments: &[StreetSeg],
    addresses: &[AddrRec],
    admin: &AdminIndex,
    normalizer: &dyn TextNormalizer,
    params: &StreetParams,
) -> Streets {
    let mut stats = StreetStats { segments: segments.len(), ..Default::default() };

    // 1. Aggregate geometry segments by municipality, name and proximity.
    let seg_muni: Vec<Option<usize>> = segments.par_iter().map(|s| admin.chain(s.point).municipality).collect();
    let seg_parts: Vec<StreetParts> = segments.par_iter().map(|s| normalizer.street_parts(&s.name)).collect();
    let mut by_name: BTreeMap<(Option<usize>, &str), Vec<usize>> = BTreeMap::new();
    for (i, p) in seg_parts.iter().enumerate() {
        if !p.core.is_empty() {
            by_name.entry((seg_muni[i], p.core.as_str())).or_default().push(i);
        }
    }
    let groups: Vec<Vec<usize>> = by_name
        .into_par_iter()
        .flat_map_iter(|(_, members)| {
            let bboxes: Vec<BBox> =
                members.iter().map(|&i| expand(BBox::of(segments[i].line.iter().copied()), params.segment_join_m)).collect();
            let mut uf = UnionFind::new(members.len());
            // Pairs of segments near each other where one has no type, for the second pass.
            let mut untyped_links: Vec<(usize, usize)> = Vec::new();
            for a in 0..members.len() {
                for b in a + 1..members.len() {
                    let (sa, sb) = (members[a], members[b]);
                    if !bboxes[a].intersects(&bboxes[b]) || !lines_near(&segments[sa].line, &segments[sb].line, params.segment_join_m) {
                        continue;
                    }
                    let (ta, tb) = (seg_parts[sa].type_code, seg_parts[sb].type_code);
                    // Within one geometry source the type is explicit: "Carrer de Sants" and
                    // "Plaça de Sants" are different streets.
                    if ta == tb {
                        uf.union(a as u32, b as u32);
                    } else if ta.is_none() || tb.is_none() {
                        untyped_links.push(if ta.is_none() { (a, b) } else { (b, a) });
                    }
                }
            }
            // A name without type ("Retuerta") is the street of the same name with type nearby
            // ("Barrio Retuerta"), but only when every typed candidate has the same type: a bare
            // "Sants" must not join "Carrer de Sants" and "Plaça de Sants".
            let mut typed_near: BTreeMap<u32, BTreeMap<&str, Vec<u32>>> = BTreeMap::new();
            for &(untyped, typed) in &untyped_links {
                let ty = seg_parts[members[typed]].type_code.expect("typed");
                let (ru, rt) = (uf.find(untyped as u32), uf.find(typed as u32));
                typed_near.entry(ru).or_default().entry(ty).or_default().push(rt);
            }
            for (untyped_root, by_type) in typed_near {
                if let [(_, roots)] = by_type.into_iter().collect::<Vec<_>>().as_slice() {
                    for &r in roots {
                        uf.union(untyped_root, r);
                    }
                }
            }
            uf.groups().into_iter().map(|g| g.into_iter().map(|k| members[k as usize]).collect::<Vec<_>>()).collect::<Vec<_>>()
        })
        .collect();

    let mut entities: Vec<StreetEntity> = groups
        .into_iter()
        .map(|members| {
            let type_code: Option<&'static str> =
                most_frequent(members.iter().filter_map(|&i| seg_parts[i].type_code).filter(|t| *t != "CL"))
                    .or_else(|| members.iter().find_map(|&i| seg_parts[i].type_code));
            let mut names = Names::new();
            for &i in &members {
                for (k, v) in &segments[i].names {
                    names.entry(k.clone()).or_insert_with(|| v.clone());
                }
            }
            StreetEntity {
                municipality: seg_muni[members[0]],
                type_code,
                core: seg_parts[members[0]].core.clone(),
                display_name: most_frequent(members.iter().map(|&i| segments[i].name.as_str())).unwrap_or_default().to_owned(),
                names,
                aliases: members.iter().map(|&i| segments[i].name.clone()).collect(),
                lines: members.iter().map(|&i| segments[i].line.clone()).collect(),
                points: Vec::new(),
                locality: None,
                highway: members
                    .iter()
                    .filter_map(|&i| segments[i].highway.as_deref())
                    .max_by_key(|h| highway_rank(Some(h)))
                    .map(str::to_owned),
                sources: members.iter().map(|&i| segments[i].source).collect(),
                source_ids: members.iter().map(|&i| segments[i].source_id.clone()).collect(),
            }
        })
        .collect();
    stats.entities_from_geometry = entities.len();

    // 2. Candidate streets per distinct (municipality, type, name) of the addresses.
    let addr_muni: Vec<Option<usize>> = addresses.par_iter().map(|a| admin.chain(a.point).municipality).collect();
    let addr_parts: Vec<Option<StreetParts>> = addresses
        .par_iter()
        .map(|a| a.street.as_deref().or(a.place.as_deref()).map(|n| normalizer.street_parts(n)).filter(|p| !p.core.is_empty()))
        .collect();

    let mut by_muni_core: FxHashMap<(Option<usize>, &str), Vec<usize>> = FxHashMap::default();
    let mut by_muni: FxHashMap<Option<usize>, Vec<usize>> = FxHashMap::default();
    for (i, e) in entities.iter().enumerate() {
        by_muni_core.entry((e.municipality, e.core.as_str())).or_default().push(i);
        by_muni.entry(e.municipality).or_default().push(i);
    }
    let keys: BTreeSet<(Option<usize>, &StreetParts)> =
        addr_parts.iter().enumerate().filter_map(|(i, p)| p.as_ref().map(|p| (addr_muni[i], p))).collect();
    let candidates: Candidates = keys
        .into_par_iter()
        .map(|(muni, parts)| {
            let ok = |e: &usize| normalizer.street_types_compatible(entities[*e].type_code, parts.type_code);
            let exact: Vec<usize> =
                by_muni_core.get(&(muni, parts.core.as_str())).into_iter().flatten().copied().filter(ok).collect();
            if !exact.is_empty() {
                return ((muni, parts), (exact, false));
            }
            let fuzzy: Vec<usize> = by_muni
                .get(&muni)
                .into_iter()
                .flatten()
                .copied()
                .filter(|e| ok(e) && fuzzy_core(&entities[*e].core, &parts.core))
                .collect();
            ((muni, parts), (fuzzy, true))
        })
        .collect();

    // 3. Each address goes to the nearest candidate street within range.
    let assigned: Vec<Option<(usize, bool)>> = addresses
        .par_iter()
        .enumerate()
        .map(|(i, a)| {
            let parts = addr_parts[i].as_ref()?;
            let (cands, fuzzy) = candidates.get(&(addr_muni[i], parts))?;
            let max_m = if *fuzzy { params.address_street_fuzzy_m } else { params.address_street_m };
            cands
                .iter()
                .map(|&e| (e, entities[e].distance_m(a.point)))
                .filter(|(_, d)| *d <= max_m)
                .min_by(|x, y| x.1.total_cmp(&y.1))
                .map(|(e, _)| (e, *fuzzy))
        })
        .collect();

    let mut address_street: Vec<Option<usize>> = vec![None; addresses.len()];
    let mut unmatched: BTreeMap<(Option<usize>, &str), Vec<usize>> = BTreeMap::new();
    for (i, a) in assigned.into_iter().enumerate() {
        match (a, &addr_parts[i]) {
            (Some((e, fuzzy)), _) => {
                address_street[i] = Some(e);
                if fuzzy { stats.addresses_fuzzy += 1 } else { stats.addresses_exact += 1 }
            }
            (None, Some(parts)) => unmatched.entry((addr_muni[i], parts.core.as_str())).or_default().push(i),
            (None, None) => stats.addresses_without_street += 1,
        }
    }

    // 4. Addresses of streets missing from geometry sources form new streets: one per cluster
    // of nearby addresses with the same name and compatible types, whatever the source. Rural
    // roads and "Diseminados" have addresses kilometres apart: within a municipality, the same
    // name in the same population entity (or with none) is one street; only distinct entities
    // (two "Carrer del Sol" in Santa Ponça and in Magaluf) keep homonyms apart.
    let mut area_buf = String::new();
    for ((muni, core), members) in unmatched {
        let type_of = |k: usize| addr_parts[members[k]].as_ref().and_then(|p| p.type_code);
        let areas: Vec<Option<String>> = members
            .iter()
            .map(|&i| {
                let a = &addresses[i];
                // Without a street, the place is the street name itself, not an entity.
                let area = a.street.as_ref().and(a.place.as_deref())?;
                normalizer.normalize(area, &mut area_buf);
                Some(area_buf.clone())
            })
            .collect();
        let cell = margin_e7(params.address_cluster_m) as i64;
        let mut grid: FxHashMap<(i64, i64), Vec<usize>> = FxHashMap::default();
        for (k, &i) in members.iter().enumerate() {
            let p = addresses[i].point;
            grid.entry((p.0 as i64 / cell, p.1 as i64 / cell)).or_default().push(k);
        }
        let mut uf = UnionFind::new(members.len());
        for (&(cx, cy), ks) in &grid {
            for dx in -1..=1 {
                for dy in -1..=1 {
                    let Some(others) = grid.get(&(cx + dx, cy + dy)) else { continue };
                    for &a in ks {
                        for &b in others {
                            if a < b
                                && normalizer.street_types_compatible(type_of(a), type_of(b))
                                && distance_m(addresses[members[a]].point, addresses[members[b]].point) < params.address_cluster_m
                            {
                                uf.union(a as u32, b as u32);
                            }
                        }
                    }
                }
            }
        }
        let mut by_area: FxHashMap<&Option<String>, Vec<usize>> = FxHashMap::default();
        for (k, area) in areas.iter().enumerate() {
            by_area.entry(area).or_default().push(k);
        }
        for ks in by_area.values() {
            for (x, &a) in ks.iter().enumerate() {
                if let Some(&b) = ks[x + 1..].iter().find(|&&b| normalizer.street_types_compatible(type_of(a), type_of(b))) {
                    uf.union(a as u32, b as u32);
                }
            }
        }
        for group in uf.groups() {
            let recs: Vec<usize> = group.iter().map(|&k| members[k as usize]).collect();
            let raw_names: Vec<&str> =
                recs.iter().filter_map(|&i| addresses[i].street.as_deref().or(addresses[i].place.as_deref())).collect();
            let display = most_frequent(raw_names.iter().copied()).unwrap_or_default();
            let display_name = if display.chars().any(|c| c.is_lowercase()) { display.to_owned() } else { title_case(display) };
            let types = || group.iter().filter_map(|&k| type_of(k as usize));
            let type_code = most_frequent(types().filter(|t| *t != "CL")).or_else(|| types().next());
            let id = entities.len();
            for &i in &recs {
                address_street[i] = Some(id);
            }
            stats.addresses_new_street += recs.len();
            entities.push(StreetEntity {
                municipality: muni,
                type_code,
                core: core.to_owned(),
                display_name,
                names: Names::new(),
                aliases: raw_names.iter().map(|n| n.to_string()).collect(),
                lines: Vec::new(),
                points: recs.iter().map(|&i| addresses[i].point).collect(),
                locality: None,
                highway: None,
                sources: recs.iter().map(|&i| addresses[i].source).collect(),
                source_ids: Vec::new(),
            });
        }
    }
    stats.entities_from_addresses = entities.len() - stats.entities_from_geometry;

    // 5. Names, sources and population entity that addresses contribute to their streets.
    let mut localities: Vec<BTreeMap<&str, u32>> = vec![BTreeMap::new(); entities.len()];
    for (i, a) in addresses.iter().enumerate() {
        if let Some(e) = address_street[i] {
            if let (Some(_), Some(area)) = (&a.street, a.place.as_deref()) {
                *localities[e].entry(area).or_default() += 1;
            }
            let entity = &mut entities[e];
            entity.sources.insert(a.source);
            if let Some(name) = a.street.as_deref().or(a.place.as_deref())
                && !entity.aliases.contains(name)
            {
                entity.aliases.insert(name.to_owned());
            }
        }
    }

    for (e, counts) in entities.iter_mut().zip(localities) {
        e.locality = counts.into_iter().max_by_key(|(name, c)| (*c, std::cmp::Reverse(*name))).map(|(n, _)| n.to_owned());
    }

    Streets { entities, address_street, stats }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fusion::records::AdminRec;

    const NATIONAL: &str = "national";

    #[test]
    fn title_case_of_official_names() {
        assert_eq!(title_case("CALLE CONSELL DE CENT"), "Calle Consell de Cent");
        assert_eq!(title_case("PLAZA MOSSEN JACINT VERDAGUER"), "Plaza Mossen Jacint Verdaguer");
        assert_eq!(title_case("CARRER DE LA MERCÈ"), "Carrer de la Mercè");
    }

    #[test]
    fn fuzzy_names() {
        assert!(fuzzy_core("jacint verdaguer", "verdaguer jacint"));
        assert!(fuzzy_core("tarradellas", "taradellas"));
        assert!(!fuzzy_core("balmes", "balmas"), "too short for a typo");
        assert!(!fuzzy_core("mallorca", "mallorca"));
        assert!(levenshtein_le1("abcdefg", "abcdeg"));
        assert!(!levenshtein_le1("abcdefg", "abdcefh"));
    }

    fn addr(id: &str, street: &str, area: Option<&str>, x: i32) -> AddrRec {
        AddrRec {
            source: NATIONAL,
            source_id: id.into(),
            street: Some(street.into()),
            place: area.map(str::to_owned),
            number: Some("1".into()),
            extension: None,
            postcode: None,
            point: (x, 0),
        }
    }

    fn seg(id: &str, name: &str, x: i32) -> StreetSeg {
        StreetSeg {
            source: crate::fusion::records::OSM,
            source_id: id.into(),
            name: name.into(),
            names: Default::default(),
            highway: None,
            line: vec![(x, 0), (x + 1_000, 0)],
            point: (x, 0),
        }
    }

    #[test]
    fn a_name_without_type_joins_the_typed_street_nearby() {
        let normalizer = country_pack_es::normalizer();
        let admins = AdminIndex::new(&[], &Default::default());
        let segments = [
            seg("w1", "Retuerta", 0),
            seg("w2", "Barrio Retuerta", 2_000),
            // Two typed streets of the same name: the bare name joins neither.
            seg("w3", "Sants", 100_000),
            seg("w4", "Carrer de Sants", 102_000),
            seg("w5", "Plaça de Sants", 104_000),
        ];
        let s = build(&segments, &[], &admins, &normalizer, &StreetParams::default());
        assert_eq!(s.entities.len(), 4, "{:?}", s.entities.iter().map(|e| &e.display_name).collect::<Vec<_>>());
        let retuerta = s.entities.iter().find(|e| e.core == "retuerta").unwrap();
        assert_eq!((retuerta.type_code, retuerta.lines.len()), (Some("BO"), 2));
    }

    #[test]
    fn distant_addresses_of_one_road_are_one_street() {
        let ring = vec![(-1_000_000, -1_000_000), (1_000_000, -1_000_000), (1_000_000, 1_000_000), (-1_000_000, 1_000_000), (-1_000_000, -1_000_000)];
        let admins = vec![AdminRec {
            osm_id: 1,
            level: 8,
            name: Some("Algaida".into()),
            names: Default::default(),
            iso3166_2: None,
            official_ref: None,
            population: None,
            wikidata: None,
            point: (0, 0),
            rings: vec![ring],
        }];
        // 180_000 units of longitude at the equator are about 2 km.
        let addresses = vec![
            addr("1", "CARRETERA MA-15", None, 0),
            addr("2", "CARRETERA Ma - 15", None, 180_000),
            addr("3", "CARRETERA MA-15", None, 360_000),
            addr("4", "CALLE DEL SOL", Some("Santa Ponça"), 0),
            addr("5", "CALLE DEL SOL", Some("SANTA PONÇA"), 180_000),
            addr("6", "CALLE DEL SOL", Some("Magaluf"), 540_000),
            // A type without a name joins no street, nor do all of them form a "Calle".
            addr("7", "CALLE", Some("Pòrtol"), 0),
            addr("8", "CALLE", Some("Pòrtol"), 1_000),
        ];
        let normalizer = country_pack_es::normalizer();
        let s = build(&[], &addresses, &AdminIndex::new(&admins, &Default::default()), &normalizer, &StreetParams::default());
        let street = |i: usize| s.address_street[i].unwrap();
        assert_eq!(street(0), street(1));
        assert_eq!(street(1), street(2));
        assert_eq!(street(3), street(4), "same population entity");
        assert_ne!(street(4), street(5), "homonyms in distinct entities");
        assert_eq!(s.entities.len(), 3);
        assert_eq!(s.entities[street(5)].locality.as_deref(), Some("Magaluf"));
        assert_eq!(s.entities[street(0)].locality, None);
        assert_eq!((s.address_street[6], s.address_street[7]), (None, None));
        assert_eq!(s.stats.addresses_without_street, 2);
    }
}

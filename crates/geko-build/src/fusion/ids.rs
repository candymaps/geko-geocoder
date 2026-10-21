//! Stable public ids (PROJECT.md §5.5) and static importance (§6.3).

use std::collections::{BTreeMap, BTreeSet};

use geko_core::country::CountryCode;
use geko_core::model::{PlaceKind, PublicId};

use super::Fused;

fn slug(text: &str) -> String {
    text.split(' ').filter(|t| !t.is_empty()).collect::<Vec<_>>().join("-")
}

/// Appends `-2`, `-3`… to keys that repeat, in the given (deterministic) order. A suffixed key
/// never takes a key that exists as such: the second "Vial A" (`vial`) must not become `vial-2`
/// when a street is named "Vial 2".
fn disambiguate(keys: Vec<String>) -> Vec<String> {
    let natural: BTreeSet<String> = keys.iter().cloned().collect();
    let mut used: BTreeSet<String> = BTreeSet::new();
    let mut next: BTreeMap<String, usize> = BTreeMap::new();
    keys.into_iter()
        .map(|k| {
            if used.insert(k.clone()) {
                return k;
            }
            let n = next.entry(k.clone()).or_insert(1);
            loop {
                *n += 1;
                let candidate = format!("{k}-{n}");
                if !natural.contains(&candidate) && used.insert(candidate.clone()) {
                    return candidate;
                }
            }
        })
        .collect()
}

fn id(kind: PlaceKind, country: CountryCode, key: &str) -> String {
    PublicId::new(kind, country, key).map(|p| p.as_str().to_owned()).unwrap_or_else(|_| format!("geko:{kind}:{}-invalid", country.lowercase()))
}

pub struct Ids {
    pub places: Vec<String>,
    pub streets: Vec<String>,
    pub addresses: Vec<String>,
    pub pois: Vec<String>,
}

/// Public ids of every entity of a partition. An entity outside every municipality of its
/// partition gets `x-<region>` instead of a municipality code, so that two partitions never
/// produce the same id (BACKLOG 6f: CartoCiudad addresses with a postcode of a neighbouring
/// province).
pub fn assign(f: &Fused, partition: &geko_core::scope::Partition) -> Ids {
    let country = partition.country;
    let no_municipality = match &partition.region {
        Some(r) => format!("x-{}", r.as_str().split_once('-').map_or(r.as_str(), |(_, s)| s).to_ascii_lowercase()),
        None => "x".to_owned(),
    };
    let muni_ref = |m: Option<usize>| -> String {
        m.and_then(|m| f.admins[m].official_ref.clone())
            .unwrap_or_else(|| m.map_or_else(|| no_municipality.clone(), |m| format!("osm-r{}", f.admins[m].osm_id)))
    };

    let place_keys: Vec<String> = f
        .places
        .iter()
        .map(|p| match (p.kind, &p.official_ref, &p.iso3166_2) {
            (PlaceKind::Municipality, Some(r), _) => r.clone(),
            (PlaceKind::Province | PlaceKind::Region, _, Some(iso)) => {
                iso.split_once('-').map_or(iso.as_str(), |(_, s)| s).to_ascii_lowercase()
            }
            _ => format!("osm-{}", p.source_ids[0].1),
        })
        .collect();
    let places = disambiguate(place_keys).iter().zip(&f.places).map(|(k, p)| id(p.kind, country, k)).collect();

    let mut street_order: Vec<usize> = (0..f.streets.len()).collect();
    street_order.sort_by_key(|&i| f.streets[i].representative_point());
    let mut street_keys = vec![String::new(); f.streets.len()];
    let ordered_keys: Vec<String> = street_order
        .iter()
        .map(|&i| {
            let s = &f.streets[i];
            let ty = s.type_code.map(|t| format!("{}-", t.to_ascii_lowercase())).unwrap_or_default();
            format!("{}-{ty}{}", muni_ref(s.municipality), slug(&s.core))
        })
        .collect();
    for (k, &i) in disambiguate(ordered_keys).into_iter().zip(&street_order) {
        street_keys[i] = k;
    }
    let streets = street_keys.iter().map(|k| id(PlaceKind::Street, country, k)).collect();

    let address_keys: Vec<String> = f
        .addresses
        .iter()
        .map(|a| {
            let number = a.number.as_ref().map(|n| n.label().to_ascii_lowercase()).unwrap_or_else(|| "sn".into());
            format!("{}:{number}", street_keys[a.street])
        })
        .collect();
    let addresses = disambiguate(address_keys).iter().map(|k| id(PlaceKind::Address, country, k)).collect();

    let pois = disambiguate(f.pois.iter().map(|p| format!("osm-{}", p.source_ids[0].1)).collect())
        .iter()
        .map(|k| id(PlaceKind::Poi, country, k))
        .collect();

    Ids { places, streets, addresses, pois }
}

/// Dense per-segment ids (PROJECT.md §5.5). Searchable entities come first, in descending order
/// of importance, so that index postings in ascending id order visit the most important first.
/// Addresses follow, grouped by street and sorted by number, so each street's addresses form one
/// contiguous block.
pub struct LocalIds {
    pub places: Vec<u32>,
    pub streets: Vec<u32>,
    pub pois: Vec<u32>,
    pub postcodes: Vec<u32>,
    pub addresses: Vec<u32>,
}

pub const POSTCODE_IMPORTANCE: u16 = (0.4 * u16::MAX as f64) as u16;

pub fn local_ids(f: &Fused, ids: &Ids, imp: &Importance) -> LocalIds {
    #[derive(Clone, Copy)]
    enum Table {
        Place,
        Street,
        Poi,
        Postcode,
    }
    let postcode_ids: Vec<String> =
        f.postcodes.iter().map(|p| format!("geko:postcode:{}-{}", f.country.lowercase(), p.postcode)).collect();
    let mut docs: Vec<(u16, &str, Table, usize)> = Vec::new();
    docs.extend(ids.places.iter().enumerate().map(|(i, id)| (imp.places[i], id.as_str(), Table::Place, i)));
    docs.extend(ids.streets.iter().enumerate().map(|(i, id)| (imp.streets[i], id.as_str(), Table::Street, i)));
    docs.extend(ids.pois.iter().enumerate().map(|(i, id)| (imp.pois[i], id.as_str(), Table::Poi, i)));
    docs.extend(postcode_ids.iter().enumerate().map(|(i, id)| (POSTCODE_IMPORTANCE, id.as_str(), Table::Postcode, i)));
    docs.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));

    let mut out = LocalIds {
        places: vec![0; f.places.len()],
        streets: vec![0; f.streets.len()],
        pois: vec![0; f.pois.len()],
        postcodes: vec![0; f.postcodes.len()],
        addresses: vec![0; f.addresses.len()],
    };
    for (local, (_, _, table, i)) in docs.iter().enumerate() {
        let target = match table {
            Table::Place => &mut out.places,
            Table::Street => &mut out.streets,
            Table::Poi => &mut out.pois,
            Table::Postcode => &mut out.postcodes,
        };
        target[*i] = local as u32;
    }

    let mut addresses: Vec<usize> = (0..f.addresses.len()).collect();
    addresses.sort_by(|&a, &b| {
        let (x, y) = (&f.addresses[a], &f.addresses[b]);
        let key = |e: &super::addresses::AddressEntity| {
            (e.number.as_ref().map_or(u32::MAX, |n| n.number), e.number.as_ref().and_then(|n| n.suffix.clone()))
        };
        out.streets[x.street]
            .cmp(&out.streets[y.street])
            .then_with(|| key(x).cmp(&key(y)))
            .then_with(|| ids.addresses[a].cmp(&ids.addresses[b]))
    });
    let base = docs.len() as u32;
    for (k, &a) in addresses.iter().enumerate() {
        out.addresses[a] = base + k as u32;
    }
    out
}

/// Fixed reference scales, not the maxima of the data: importance must not depend on which
/// regions are in scope, nor differ between the partitions fused separately (PROJECT.md §6.5.8).
/// About the largest municipality of western Europe (Madrid); larger values saturate.
const REFERENCE_POPULATION: f64 = 3_500_000.0;
/// Addresses of a long avenue of a large city (Gran Via de les Corts Catalanes has 1,010).
const REFERENCE_STREET_ADDRESSES: f64 = 1_000.0;
/// Languages other than those of its country that a world-famous landmark is named in (the
/// Sagrada Família has 16 in OSM).
const REFERENCE_FOREIGN_NAMES: f64 = 15.0;

/// How many languages other than those of the country an entity is named in (`name:<lang>`):
/// how well known it is abroad, a sign of fame that every country's data has. Without
/// languages of its own, any language counts.
fn foreign_names(names: &super::records::Names, languages: &[String]) -> usize {
    let mut foreign: Vec<&str> = names
        .keys()
        .filter_map(|k| k.strip_prefix("name:"))
        .map(|lang| lang.split(['-', '_']).next().unwrap_or(lang))
        .filter(|lang| !languages.iter().any(|l| l == lang))
        .collect();
    foreign.sort_unstable();
    foreign.dedup();
    foreign.len()
}

fn log_scale(value: f64, max: f64) -> f64 {
    ((1.0 + value).ln() / (1.0 + max).ln()).clamp(0.0, 1.0)
}

fn to_u16(v: f64) -> u16 {
    (v.clamp(0.0, 1.0) * u16::MAX as f64).round() as u16
}

pub struct Importance {
    pub places: Vec<u16>,
    pub streets: Vec<u16>,
    pub addresses: Vec<u16>,
    pub pois: Vec<u16>,
}

pub fn importance(f: &Fused, languages: &[String]) -> Importance {
    // Entities inherit part of their municipality's weight (PROJECT.md §6.3): a neighbourhood
    // or street of a city ranks above a homonym in a small town.
    let municipality_pop: rustc_hash::FxHashMap<usize, i64> = f
        .places
        .iter()
        .filter(|p| p.kind == PlaceKind::Municipality)
        .filter_map(|p| Some((p.admin?, p.population?)))
        .collect();
    let municipality_weight =
        |m: Option<usize>| m.and_then(|m| municipality_pop.get(&m)).map_or(0.0, |&p| log_scale(p as f64, REFERENCE_POPULATION));
    let places = f
        .places
        .iter()
        .map(|p| {
            let inherited = match p.kind {
                PlaceKind::Region | PlaceKind::Province | PlaceKind::Municipality => 0.0,
                _ => 0.2 * municipality_weight(p.chain.municipality),
            };
            let base = match (p.kind, p.place_type.as_deref()) {
                (PlaceKind::Region, _) => 0.9,
                (PlaceKind::Province, _) => 0.85,
                (PlaceKind::Municipality, _) => 0.55,
                (_, Some("city")) => 0.6,
                (_, Some("town")) => 0.5,
                (_, Some("village")) => 0.4,
                (PlaceKind::Neighbourhood, _) => 0.35,
                (_, Some("hamlet" | "isolated_dwelling")) => 0.25,
                _ => 0.2,
            };
            // Population weighs by kind, on the municipal scale: a hamlet's or a neighbourhood's
            // own population must not outrank a district of a large city.
            let population_weight = match p.kind {
                PlaceKind::Municipality => 0.4,
                PlaceKind::Locality => 0.3,
                PlaceKind::Neighbourhood => 0.1,
                _ => 0.0,
            };
            let pop = p.population.map_or(0.0, |v| population_weight * log_scale(v as f64, REFERENCE_POPULATION));
            to_u16(base + pop + inherited)
        })
        .collect();

    let mut addr_count = vec![0usize; f.streets.len()];
    for a in &f.addresses {
        addr_count[a.street] += 1;
    }
    let street_values: Vec<f64> = f
        .streets
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let class = match s.highway.as_deref() {
                Some("motorway" | "trunk") => 0.2,
                Some("primary") => 0.15,
                Some("secondary") => 0.1,
                Some("tertiary") => 0.05,
                _ => 0.0,
            };
            0.15 + 0.35 * log_scale(addr_count[i] as f64, REFERENCE_STREET_ADDRESSES) + class + 0.15 * municipality_weight(s.municipality)
        })
        .collect();
    let addresses = f.addresses.iter().map(|a| to_u16(street_values[a.street] * 0.9)).collect();
    let streets = street_values.into_iter().map(to_u16).collect();
    let pois = f
        .pois
        .iter()
        .map(|p| {
            let fame = 0.4 * log_scale(foreign_names(&p.names, languages) as f64, REFERENCE_FOREIGN_NAMES);
            to_u16(0.2 + if p.wikidata.is_some() { 0.2 } else { 0.0 } + fame + 0.15 * municipality_weight(p.chain.municipality))
        })
        .collect();
    Importance { places, streets, addresses, pois }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_keys_get_a_suffix() {
        let keys = vec!["a".to_string(), "b".into(), "a".into(), "a".into()];
        assert_eq!(disambiguate(keys), ["a", "b", "a-2", "a-3"]);
        let keys = vec!["vial".to_string(), "vial".into(), "vial-2".into()];
        assert_eq!(disambiguate(keys), ["vial", "vial-3", "vial-2"]);
    }

    #[test]
    fn foreign_names_count_languages_outside_the_country() {
        let names: super::super::records::Names = ["name", "name:es", "name:ca", "name:en", "name:de", "name:zh-Hans", "name:zh-Hant", "old_name"]
            .into_iter()
            .map(|k| (k.to_owned(), "x".to_owned()))
            .collect();
        let languages = ["es".to_owned(), "ca".to_owned()];
        assert_eq!(foreign_names(&names, &languages), 3);
        assert_eq!(foreign_names(&names, &[]), 5);
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("consell cent"), "consell-cent");
    }
}

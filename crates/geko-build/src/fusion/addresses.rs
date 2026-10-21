//! Address entities (PROJECT.md §6.5.2–6.5.3): records of every source with the same street,
//! number and suffix, close to each other, are one address. Each attribute comes from the
//! source with the highest priority that has it; disagreements are kept as conflicts.

use std::collections::{BTreeMap, BTreeSet};

use geko_core::model::HouseNumber;
use geko_core::ports::TextNormalizer;
use geko_geom::{Pt, distance_m};
use rayon::prelude::*;

use super::records::{AddrRec, OSM};
use super::uf::UnionFind;

#[derive(Debug, Clone)]
pub struct AddressParams {
    /// Records with the same street and number are one address within this distance.
    pub same_address_m: f64,
    /// Same, for addresses without a number.
    pub same_unnumbered_m: f64,
}

impl Default for AddressParams {
    fn default() -> Self {
        Self { same_address_m: 75.0, same_unnumbered_m: 30.0 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AttrSource {
    Source(&'static str),
    Inferred,
}

impl AttrSource {
    pub fn as_str(self) -> &'static str {
        match self {
            AttrSource::Source(s) => s,
            AttrSource::Inferred => "inferred",
        }
    }
}

#[derive(Debug, Clone)]
pub struct AddressEntity {
    pub street: usize,
    pub number: Option<HouseNumber>,
    pub point: Pt,
    pub postcode: Option<String>,
    pub postcode_source: Option<AttrSource>,
    pub sources: BTreeSet<&'static str>,
    pub source_ids: Vec<(&'static str, String)>,
}

#[derive(Debug, Clone)]
pub struct Conflict {
    pub entity_kind: &'static str,
    pub entity: usize,
    pub attribute: &'static str,
    pub chosen: String,
    pub chosen_source: &'static str,
    pub discarded: String,
    pub discarded_source: &'static str,
}

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct AddressStats {
    pub records: usize,
    pub entities: usize,
    pub multi_source: usize,
    pub without_street: usize,
}

/// Street, number and suffix.
type AddressKey<'a> = (usize, Option<u32>, Option<&'a str>);

/// Priority for position and postcode, highest first: the national sources of addresses, whose
/// whole purpose they are, before OSM.
fn rank(source: &str) -> usize {
    usize::from(source == OSM)
}

pub fn parse_number(rec: &AddrRec, normalizer: &dyn TextNormalizer) -> Option<HouseNumber> {
    let mut hn = normalizer.parse_housenumber(rec.number.as_deref()?)?;
    if hn.suffix.is_none()
        && let Some(ext) = rec.extension.as_deref()
    {
        let ext: String = ext.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_uppercase();
        if !ext.is_empty() {
            hn.suffix = Some(ext);
        }
    }
    Some(hn)
}

pub fn build(
    records: &[AddrRec],
    address_street: &[Option<usize>],
    normalizer: &dyn TextNormalizer,
    params: &AddressParams,
) -> (Vec<AddressEntity>, Vec<Conflict>, AddressStats) {
    let mut stats = AddressStats { records: records.len(), ..Default::default() };
    let numbers: Vec<Option<HouseNumber>> = records.par_iter().map(|r| parse_number(r, normalizer)).collect();

    let mut groups: BTreeMap<AddressKey, Vec<usize>> = BTreeMap::new();
    for (i, street) in address_street.iter().enumerate() {
        match street {
            Some(s) => {
                let n = numbers[i].as_ref();
                groups.entry((*s, n.map(|n| n.number), n.and_then(|n| n.suffix.as_deref()))).or_default().push(i);
            }
            None => stats.without_street += 1,
        }
    }

    let clusters: Vec<(usize, Vec<usize>)> = groups
        .into_par_iter()
        .flat_map_iter(|((street, number, _), members)| {
            let max_m = if number.is_some() { params.same_address_m } else { params.same_unnumbered_m };
            let mut uf = UnionFind::new(members.len());
            for a in 0..members.len() {
                for b in a + 1..members.len() {
                    if distance_m(records[members[a]].point, records[members[b]].point) < max_m {
                        uf.union(a as u32, b as u32);
                    }
                }
            }
            uf.groups()
                .into_iter()
                .map(|g| (street, g.into_iter().map(|k| members[k as usize]).collect::<Vec<_>>()))
                .collect::<Vec<_>>()
        })
        .collect();

    let mut entities = Vec::with_capacity(clusters.len());
    let mut conflicts = Vec::new();
    for (street, mut members) in clusters {
        members.sort_by(|&a, &b| {
            rank(records[a].source)
                .cmp(&rank(records[b].source))
                .then_with(|| records[a].source_id.cmp(&records[b].source_id))
        });
        let master = members[0];
        let with_postcode: Vec<usize> = members.iter().copied().filter(|&i| records[i].postcode.is_some()).collect();
        let chosen = with_postcode.first().copied();
        let id = entities.len();
        if let Some(c) = chosen {
            let value = records[c].postcode.as_deref().unwrap_or_default();
            for &o in &with_postcode[1..] {
                let other = records[o].postcode.as_deref().unwrap_or_default();
                if other != value && records[o].source != records[c].source {
                    conflicts.push(Conflict {
                        entity_kind: "address",
                        entity: id,
                        attribute: "postcode",
                        chosen: value.to_owned(),
                        chosen_source: records[c].source,
                        discarded: other.to_owned(),
                        discarded_source: records[o].source,
                    });
                }
            }
        }
        let sources: BTreeSet<&'static str> = members.iter().map(|&i| records[i].source).collect();
        if sources.len() > 1 {
            stats.multi_source += 1;
        }
        entities.push(AddressEntity {
            street,
            number: numbers[master].clone(),
            point: records[master].point,
            postcode: chosen.and_then(|c| records[c].postcode.clone()),
            postcode_source: chosen.map(|c| AttrSource::Source(records[c].source)),
            sources,
            source_ids: members.iter().map(|&i| (records[i].source, records[i].source_id.clone())).collect(),
        });
    }
    stats.entities = entities.len();
    (entities, conflicts, stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NATIONAL: &str = "national";

    struct Plain;

    impl TextNormalizer for Plain {
        fn country(&self) -> geko_core::country::CountryCode {
            geko_core::country::CountryCode::new("ES").unwrap()
        }
        fn normalize(&self, text: &str, out: &mut String) {
            *out = text.to_lowercase();
        }
        fn tokenize(&self, _: &str, _: &mut Vec<String>) {}
        fn variants(&self, _: &str, _: &mut Vec<String>) {}
        fn street_parts(&self, name: &str) -> geko_core::model::StreetParts {
            geko_core::model::StreetParts { type_code: None, core: name.to_lowercase() }
        }
        fn parse_housenumber(&self, raw: &str) -> Option<HouseNumber> {
            Some(HouseNumber { number: raw.parse().ok()?, suffix: None, range_end: None })
        }
        fn street_types_compatible(&self, a: Option<&str>, b: Option<&str>) -> bool {
            a.is_none() || b.is_none() || a == b
        }
    }

    fn rec(source: &'static str, id: &str, number: &str, postcode: Option<&str>, x: i32) -> AddrRec {
        AddrRec {
            source,
            source_id: id.into(),
            street: Some("x".into()),
            place: None,
            number: Some(number.into()),
            extension: None,
            postcode: postcode.map(Into::into),
            point: (21_686_000 + x, 413_874_000),
        }
    }

    #[test]
    fn same_address_from_two_sources_is_one_entity() {
        let records = vec![
            rec(OSM, "n1", "401", None, 0),
            rec(NATIONAL, "c1", "401", Some("08013"), 120),
            rec(NATIONAL, "c2", "403", Some("08013"), 300),
        ];
        let (entities, conflicts, stats) = build(&records, &[Some(0), Some(0), Some(0)], &Plain, &AddressParams::default());
        assert_eq!(entities.len(), 2);
        let merged = entities.iter().find(|e| e.number.as_ref().unwrap().number == 401).unwrap();
        assert_eq!(merged.postcode.as_deref(), Some("08013"));
        assert_eq!(merged.postcode_source, Some(AttrSource::Source(NATIONAL)));
        assert_eq!(merged.point, records[1].point, "position from the source with priority");
        assert_eq!(merged.sources.len(), 2);
        assert!(conflicts.is_empty());
        assert_eq!(stats.multi_source, 1);
    }

    #[test]
    fn far_apart_records_stay_separate_and_conflicts_are_kept() {
        let records = vec![
            rec(OSM, "n1", "7", Some("08001"), 0),
            rec(NATIONAL, "c1", "7", Some("08002"), 100),
            rec(NATIONAL, "c2", "7", Some("08003"), 20_000),
        ];
        let (entities, conflicts, _) = build(&records, &[Some(0), Some(0), Some(0)], &Plain, &AddressParams::default());
        assert_eq!(entities.len(), 2, "the third record is ~1.7 km away");
        assert_eq!(conflicts.len(), 1);
        assert_eq!((conflicts[0].chosen.as_str(), conflicts[0].discarded.as_str()), ("08002", "08001"));
    }

    #[test]
    fn addresses_without_street_are_counted() {
        let records = vec![rec(OSM, "n1", "1", None, 0)];
        let (entities, _, stats) = build(&records, &[None], &Plain, &AddressParams::default());
        assert!(entities.is_empty());
        assert_eq!(stats.without_street, 1);
    }
}

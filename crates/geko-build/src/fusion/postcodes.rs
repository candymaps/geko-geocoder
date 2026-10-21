//! Postcode zones and inferred postcodes (PROJECT.md §6.5.4).

use std::collections::BTreeMap;

use geko_geom::Pt;
use h3o::{CellIndex, LatLng, Resolution};
use rustc_hash::FxHashMap;

use super::addresses::{AddressEntity, AttrSource};

pub const RESOLUTION: Resolution = Resolution::Nine;
const MAX_RING: u32 = 3;
const MAX_NUMBER_GAP: u32 = 20;

pub fn cell(p: Pt) -> Option<CellIndex> {
    LatLng::new(p.1 as f64 * 1e-7, p.0 as f64 * 1e-7).ok().map(|ll| ll.to_cell(RESOLUTION))
}

/// H3 cell → majority postcode of the addresses with a postcode from a source.
pub struct Zones {
    pub cells: FxHashMap<CellIndex, String>,
}

impl Zones {
    pub fn build(addresses: &[AddressEntity]) -> Self {
        let mut counts: FxHashMap<CellIndex, BTreeMap<&str, usize>> = FxHashMap::default();
        for a in addresses {
            if let (Some(pc), Some(AttrSource::Source(_)), Some(c)) = (&a.postcode, a.postcode_source, cell(a.point)) {
                *counts.entry(c).or_default().entry(pc.as_str()).or_default() += 1;
            }
        }
        let cells = counts.into_iter().filter_map(|(c, by_pc)| majority(by_pc).map(|pc| (c, pc))).collect();
        Self { cells }
    }

    /// The postcode of the point's cell or, failing that, the majority of the nearest rings.
    pub fn lookup(&self, p: Pt) -> Option<&str> {
        let c = cell(p)?;
        if let Some(pc) = self.cells.get(&c) {
            return Some(pc);
        }
        for k in 1..=MAX_RING {
            let mut by_pc: BTreeMap<&str, usize> = BTreeMap::new();
            for n in c.grid_disk::<Vec<_>>(k) {
                if let Some(pc) = self.cells.get(&n) {
                    *by_pc.entry(pc.as_str()).or_default() += 1;
                }
            }
            if let Some(pc) = majority_ref(&by_pc) {
                return Some(pc);
            }
        }
        None
    }
}

fn majority(by_pc: BTreeMap<&str, usize>) -> Option<String> {
    majority_ref(&by_pc).map(str::to_owned)
}

fn majority_ref<'a>(by_pc: &BTreeMap<&'a str, usize>) -> Option<&'a str> {
    by_pc.iter().max_by_key(|(pc, n)| (**n, std::cmp::Reverse(**pc))).map(|(pc, _)| *pc)
}

/// Postcodes for addresses without one: from the nearest numbers on the same side of the same
/// street, else from the zone. Returns how many were inferred.
pub fn infer_for_addresses(addresses: &mut [AddressEntity], zones: &Zones) -> usize {
    let mut by_street: FxHashMap<usize, Vec<(u32, String)>> = FxHashMap::default();
    for a in addresses.iter() {
        if let (Some(n), Some(pc), Some(AttrSource::Source(_))) = (&a.number, &a.postcode, a.postcode_source) {
            by_street.entry(a.street).or_default().push((n.number, pc.clone()));
        }
    }
    let mut inferred = 0;
    for a in addresses.iter_mut().filter(|a| a.postcode.is_none()) {
        let from_street = a.number.as_ref().and_then(|n| {
            by_street
                .get(&a.street)?
                .iter()
                .filter(|(m, _)| m % 2 == n.number % 2 && m.abs_diff(n.number) <= MAX_NUMBER_GAP)
                .min_by_key(|(m, pc)| (m.abs_diff(n.number), pc.clone()))
                .map(|(_, pc)| pc.clone())
        });
        if let Some(pc) = from_street.or_else(|| zones.lookup(a.point).map(str::to_owned)) {
            a.postcode = Some(pc);
            a.postcode_source = Some(AttrSource::Inferred);
            inferred += 1;
        }
    }
    inferred
}

#[cfg(test)]
mod tests {
    use super::*;
    use geko_core::model::HouseNumber;
    use std::collections::BTreeSet;

    fn addr(street: usize, number: Option<u32>, pc: Option<&str>, x: i32) -> AddressEntity {
        AddressEntity {
            street,
            number: number.map(|n| HouseNumber { number: n, suffix: None, range_end: None }),
            point: (21_686_000 + x, 413_874_000),
            postcode: pc.map(Into::into),
            postcode_source: pc.map(|_| AttrSource::Source("national")),
            sources: BTreeSet::new(),
            source_ids: Vec::new(),
        }
    }

    #[test]
    fn infers_from_same_side_of_the_street_first() {
        let mut addresses = vec![
            addr(0, Some(10), Some("08001"), 0),
            addr(0, Some(11), Some("08002"), 0),
            addr(0, Some(14), None, 0),
            addr(1, Some(3), None, 50),
        ];
        let zones = Zones::build(&addresses);
        assert_eq!(infer_for_addresses(&mut addresses, &zones), 2);
        assert_eq!(addresses[2].postcode.as_deref(), Some("08001"), "even side");
        assert_eq!(addresses[2].postcode_source, Some(AttrSource::Inferred));
        assert!(addresses[3].postcode.is_some(), "from the zone");
    }

    #[test]
    fn zone_lookup_uses_neighbouring_cells() {
        let addresses = vec![addr(0, Some(1), Some("08013"), 0)];
        let zones = Zones::build(&addresses);
        assert_eq!(zones.lookup((21_686_000, 413_874_000)), Some("08013"));
        assert_eq!(zones.lookup((21_686_000 + 4_000, 413_874_000)), Some("08013"), "~330 m away");
        assert_eq!(zones.lookup((22_686_000, 413_874_000)), None, "~8 km away");
    }
}

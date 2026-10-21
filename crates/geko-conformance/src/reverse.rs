//! Conformance of `ReverseIndex`. The port has no builder trait: each provider passes its build
//! function, which reads the canonical fixture and writes the index into a directory.

use std::path::Path;

use geko_core::Result;
use geko_core::country::CountryCode;
use geko_core::model::PlaceKind;
use geko_core::ports::ReverseIndex;
use geko_core::query::{Hit, KindFilter};

use crate::fixture::Fixture;

/// Builds the fixture with `build` and checks the index that `open` opens on it.
pub fn check(
    build: &dyn Fn(&Path, CountryCode, &Path) -> Result<u64>,
    open: &dyn Fn(&Path) -> Result<Box<dyn ReverseIndex>>,
    fixture: &Fixture,
) {
    let dir = fixture.out.join("reverse");
    std::fs::create_dir_all(&dir).unwrap();
    let points = build(&fixture.canonical, fixture.country, &dir).expect("the reverse index builds from the fixture");
    assert!(points >= 10, "portals, streets and the POI are indexed: {points}");
    let index = open(&dir).expect("the index opens what the build wrote");
    let index = index.as_ref();
    let place = |h: &Hit| fixture.entity(h.id);
    assert_eq!(index.country(), fixture.country);

    // The portal under the point comes first; results are nearest first and within `k`.
    let target = (fixture.portal)(7);
    let mut hits = Vec::new();
    index.nearest(target, KindFilter::ALL, 5, &mut hits).unwrap();
    assert!(!hits.is_empty() && hits.len() <= 5, "{hits:?}");
    let (kind, point) = place(&hits[0]);
    assert_eq!(kind, PlaceKind::Address);
    assert!(crate::distance_m(point, target) < 1.0, "nearest is not the portal under the point");
    // Streets are measured to their line, not to their representative point: compare points only.
    let distances: Vec<f64> = hits
        .iter()
        .map(place)
        .filter(|(kind, _)| matches!(kind, PlaceKind::Address | PlaceKind::Poi))
        .map(|(_, p)| crate::distance_m(p, target))
        .collect();
    for pair in distances.windows(2) {
        assert!(pair[0] <= pair[1] + 0.5, "not nearest first: {distances:?}");
    }
    for h in &hits {
        assert_eq!(h.country, fixture.country);
    }

    // Asked for streets only, a point at a portal gets the portal's street first.
    index.nearest(target, KindFilter::only(&[PlaceKind::Street]), 3, &mut hits).unwrap();
    assert!(!hits.is_empty() && hits.len() <= 3, "{hits:?}");
    let (kind, point) = place(&hits[0]);
    // Carrer Major runs along the latitude of its portals; the other street is 220 m north.
    assert!(kind == PlaceKind::Street && (point.lat_e7() - target.lat_e7()).abs() < 1_000, "the street of the portal comes first: {hits:?}");

    // The filter keeps only the kinds asked for, and the buffer is replaced, not appended to.
    index.nearest(target, KindFilter::only(&[PlaceKind::Poi]), 3, &mut hits).unwrap();
    assert!(!hits.is_empty(), "the POI is found with a POI filter");
    assert!(hits.iter().all(|h| place(h).0 == PlaceKind::Poi), "filter: {hits:?}");
    index.nearest(target, KindFilter::ALL, 0, &mut hits).unwrap();
    assert!(hits.is_empty(), "k = 0");

    // Administrative chain: by coordinates, from coarse to fine; none outside every boundary.
    let chain = index.admin_at(fixture.inside_empty).expect("a point inside the municipality has a chain");
    assert_eq!(chain.country, fixture.country);
    let municipality = chain.levels.iter().find(|l| l.kind == PlaceKind::Municipality).expect("the chain has the municipality");
    assert_eq!(municipality.name, fixture.municipality);
    let order: Vec<PlaceKind> = chain.levels.iter().map(|l| l.kind).collect();
    let mut sorted = order.clone();
    sorted.sort();
    assert_eq!(order, sorted, "levels from coarse to fine");
    assert!(index.admin_at(fixture.outside).is_none(), "no chain outside every boundary");
}

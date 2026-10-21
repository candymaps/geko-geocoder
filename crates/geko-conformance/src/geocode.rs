//! Conformance of `GeocodeIndexBuilder` and `GeocodeEngine`: one segment built from the canonical
//! fixture, queried with interpretations built by hand, so the suite does not depend on a parser.

use std::path::Path;

use geko_core::Result;
use geko_core::model::PlaceKind;
use geko_core::ports::{CanonicalPartition, GeocodeEngine, GeocodeIndexBuilder, TextNormalizer};
use geko_core::query::{AddressComponents, Hit, Interpretation, InterpretationKind, ParsedQuery, QueryContext};

use crate::distance_m;
use crate::fixture::Fixture;

fn query(fixture: &Fixture, raw: &str, kind: InterpretationKind) -> ParsedQuery {
    ParsedQuery { raw: raw.into(), interpretations: vec![Interpretation { probability: 1.0, country: fixture.country, kind }] }
}

fn address(street: &str, number: Option<&str>, locality: Option<&str>) -> InterpretationKind {
    InterpretationKind::Address(AddressComponents {
        street: Some(street.into()),
        housenumber: number.map(Into::into),
        postcode: None,
        locality: locality.map(Into::into),
    })
}

fn place(text: &str) -> InterpretationKind {
    InterpretationKind::Place { text: text.into(), within: None }
}

/// Checks the contract shared by every hit list: within `size`, best first, every id with a
/// place whose public id resolves back to it.
fn check_hits(engine: &dyn GeocodeEngine, hits: &[Hit], ctx: &QueryContext, what: &str) {
    assert!(hits.len() <= ctx.size, "{what}: {} hits for size {}", hits.len(), ctx.size);
    for pair in hits.windows(2) {
        assert!(pair[0].score >= pair[1].score, "{what}: hits not sorted by score: {pair:?}");
    }
    for h in hits {
        assert_eq!(h.country, engine.country(), "{what}: hit of another country");
        let place = engine.place(h.id).unwrap_or_else(|| panic!("{what}: no place for hit {h:?}"));
        assert_eq!(place.id, h.id, "{what}");
        assert_eq!(engine.resolve(&geko_core::model::PublicId::parse(&place.public_id).expect("valid public id")), Some(h.id), "{what}");
    }
}

fn top<'e>(engine: &'e dyn GeocodeEngine, hits: &[Hit], what: &str) -> geko_core::model::PlaceRef<'e> {
    let first = hits.first().unwrap_or_else(|| panic!("{what}: no results"));
    engine.place(first.id).unwrap()
}

/// Builds the fixture with `builder` and checks the engine that `open` opens on it.
pub fn check(
    builder: &dyn GeocodeIndexBuilder,
    open: &dyn Fn(&Path) -> Result<Box<dyn GeocodeEngine>>,
    normalizer: &dyn TextNormalizer,
    fixture: &Fixture,
) {
    let dir = fixture.out.join("geocode");
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = builder
        .build(&CanonicalPartition { country: fixture.country, dir: fixture.canonical.clone() }, normalizer, &dir)
        .expect("the builder indexes the fixture");
    assert_eq!(manifest.format, builder.format(), "the manifest reports the builder's format");
    assert_eq!(manifest.country, fixture.country);
    assert!(manifest.entities >= 5, "places, streets, portals and the POI are indexed: {}", manifest.entities);

    let engine = open(&dir).expect("the engine opens what the builder wrote");
    let engine = engine.as_ref();
    assert_eq!(engine.country(), fixture.country);
    let ctx = QueryContext { size: 5, ..Default::default() };
    let mut hits = vec![Hit { country: fixture.country, id: geko_core::model::LocalId(u32::MAX), score: 0.0, match_type: geko_core::model::MatchType::Exact }];

    // An exact address: the portal first, at its position.
    let q = query(fixture, "carrer major 5, algaida", address("carrer major", Some("5"), Some(fixture.municipality)));
    engine.search(&q, &ctx, &mut hits).unwrap();
    check_hits(engine, &hits, &ctx, "address");
    let first = top(engine, &hits, "address");
    assert_eq!(first.kind, PlaceKind::Address, "address: {}", first.public_id);
    assert!(distance_m(first.point, (fixture.portal)(5)) < 15.0, "address: {} is not Carrer Major 5", first.public_id);

    // A spent time budget never cuts the likeliest reading: the portal is still first.
    let late = QueryContext { deadline: Some(std::time::Instant::now()), ..ctx.clone() };
    let mut rushed = Vec::new();
    engine.search(&q, &late, &mut rushed).unwrap();
    assert_eq!(rushed.first().map(|h| h.id), hits.first().map(|h| h.id), "time budget spent");

    // The same query in Spanish and in capitals finds the same portal.
    let mut again = Vec::new();
    let q = query(fixture, "CALLE MAYOR 5", address("CALLE MAJOR", Some("5"), None));
    engine.search(&q, &ctx, &mut again).unwrap();
    assert_eq!(again.first().map(|h| h.id), hits.first().map(|h| h.id), "address written as in another source");

    // A street without number, a municipality, a locality and a POI by name.
    for (text, kind, expected) in [
        ("carrer major", address(fixture.street, None, None), PlaceKind::Street),
        ("algaida", place(fixture.municipality), PlaceKind::Municipality),
        ("portol", place(fixture.locality), PlaceKind::Locality),
        ("farmacia central", place(fixture.poi), PlaceKind::Poi),
    ] {
        engine.search(&query(fixture, text, kind), &ctx, &mut hits).unwrap();
        check_hits(engine, &hits, &ctx, text);
        assert_eq!(top(engine, &hits, text).kind, expected, "{text}");
    }
    assert!(distance_m(top(engine, &hits, "poi").point, fixture.poi_point) < 15.0);

    // Postcodes of the sources are searchable.
    engine.search(&query(fixture, "07210", InterpretationKind::Postcode("07210".into())), &ctx, &mut hits).unwrap();
    check_hits(engine, &hits, &ctx, "postcode");
    assert_eq!(top(engine, &hits, "postcode").kind, PlaceKind::Postcode);

    // Autocomplete: the word being typed is a prefix.
    engine.autocomplete(&query(fixture, "algai", place("algai")), &ctx, &mut hits).unwrap();
    check_hits(engine, &hits, &ctx, "autocomplete");
    assert_eq!(top(engine, &hits, "autocomplete").kind, PlaceKind::Municipality);

    // Nothing to find: no results, and the buffer of the previous query is cleared.
    engine.search(&query(fixture, "zzqx", place("zzqx")), &ctx, &mut hits).unwrap();
    assert!(hits.is_empty(), "unknown text: {hits:?}");
    engine.search(&ParsedQuery { raw: String::new(), interpretations: vec![] }, &ctx, &mut hits).unwrap();
    assert!(hits.is_empty(), "no interpretations");

    // `size` bounds the results and `min_score` lets the engine stop: nothing beats infinity.
    let one = QueryContext { size: 1, ..Default::default() };
    engine.search(&query(fixture, "carrer major", address(fixture.street, None, None)), &one, &mut hits).unwrap();
    assert_eq!(hits.len(), 1);
    let unbeatable = QueryContext { min_score: f32::INFINITY, ..ctx.clone() };
    engine.search(&query(fixture, "carrer major", address(fixture.street, None, None)), &unbeatable, &mut hits).unwrap();
    assert!(hits.is_empty(), "min_score: {hits:?}");

    // Unknown ids.
    assert!(engine.place(geko_core::model::LocalId(u32::MAX)).is_none());
    let foreign = geko_core::model::PublicId::parse("geko:street:es-99999-cl-nowhere").unwrap();
    assert_eq!(engine.resolve(&foreign), None);
}

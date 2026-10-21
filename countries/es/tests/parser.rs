//! Query parsing in Spain: the generic rules with the pack of Spain.

use geko_core::ports::AddressParser;
use geko_core::query::{AddressComponents, InterpretationKind, ParsedQuery};
use text_rules::MAX_INTERPRETATIONS;

fn parse(text: &str) -> ParsedQuery {
    country_pack_es::parser().parse(text).unwrap()
}

fn best(text: &str) -> InterpretationKind {
    parse(text).interpretations.remove(0).kind
}

fn addr(street: Option<&str>, number: Option<&str>, postcode: Option<&str>, locality: Option<&str>) -> InterpretationKind {
    InterpretationKind::Address(AddressComponents {
        street: street.map(Into::into),
        housenumber: number.map(Into::into),
        postcode: postcode.map(Into::into),
        locality: locality.map(Into::into),
    })
}

fn place(text: &str, within: Option<&str>) -> InterpretationKind {
    InterpretationKind::Place { text: text.into(), within: within.map(Into::into) }
}

#[test]
fn full_addresses() {
    assert_eq!(
        best("Carrer de Mallorca, 401, 08013 Barcelona"),
        addr(Some("Carrer de Mallorca"), Some("401"), Some("08013"), Some("Barcelona"))
    );
    assert_eq!(best("calle mayor 23 alcala de henares"), addr(Some("calle mayor"), Some("23"), None, Some("alcala de henares")));
    assert_eq!(best("c/ mayor, 23, 28801"), addr(Some("c/ mayor"), Some("23"), Some("28801"), None));
    assert_eq!(best("Av. Diagonal nº 640"), addr(Some("Av Diagonal"), Some("640"), None, None));
    assert_eq!(best("mallorca 401"), addr(Some("mallorca"), Some("401"), None, None));
}

#[test]
fn house_number_forms() {
    assert_eq!(best("calle mayor 12B"), addr(Some("calle mayor"), Some("12B"), None, None));
    assert_eq!(best("calle mayor 187-189"), addr(Some("calle mayor"), Some("187-189"), None, None));
    assert_eq!(best("carretera de vic s/n manresa"), addr(Some("carretera de vic"), None, None, Some("manresa")));
}

#[test]
fn numbers_inside_street_names() {
    assert_eq!(best("Calle 13 de Septiembre 4"), addr(Some("Calle 13 de Septiembre"), Some("4"), None, None));
    assert_eq!(best("Calle 13 de Septiembre"), addr(Some("Calle 13 de Septiembre"), None, None, None));
    // A street type alone is no name: the number is the name.
    assert_eq!(best("Carrer 106, Palma"), addr(Some("Carrer 106"), None, None, Some("Palma")));
    // Words between the number and the comma cannot be the locality.
    assert_eq!(
        best("Camino 2 Cala Mastella, Santa Eulària des Riu"),
        addr(Some("Camino 2 Cala Mastella"), None, None, Some("Santa Eulària des Riu"))
    );
    assert_eq!(
        best("Calle Numero 5 Zona Servicios, Sant Antoni de Portmany"),
        addr(Some("Calle 5 Zona Servicios"), None, None, Some("Sant Antoni de Portmany"))
    );
    // Otherwise it is a house number: an alternative reading on every address costs too much.
    assert_eq!(best("Lugar Poligono 6, Llucmajor"), addr(Some("Lugar Poligono"), Some("6"), None, Some("Llucmajor")));
    assert_eq!(best("carrer major 5 bajo, algaida"), addr(Some("carrer major"), Some("5"), None, Some("algaida")));
}

#[test]
fn a_number_after_a_street_type_alone_may_be_the_house_number() {
    let readings = parse("carrer 24 femenies").interpretations;
    assert_eq!(readings[0].kind, addr(Some("carrer 24 femenies"), None, None, None), "first, as a name");
    assert!(readings.iter().any(|i| i.kind == addr(Some("carrer femenies"), Some("24"), None, None)));
    let readings = parse("street 24 femenies, palma").interpretations;
    assert!(readings.iter().any(|i| i.kind == addr(Some("street femenies"), Some("24"), None, Some("palma"))), "a type of another language too");
    assert!(!parse("Carrer 106, Palma").interpretations.iter().any(|i| matches!(&i.kind, InterpretationKind::Address(a) if a.housenumber.is_some())));
}

#[test]
fn floor_and_door_are_not_a_locality() {
    assert_eq!(
        best("Carrer de Mallorca 401 2º 1ª, Barcelona"),
        addr(Some("Carrer de Mallorca"), Some("401"), None, Some("Barcelona"))
    );
    assert_eq!(best("calle mayor 5 bajo"), addr(Some("calle mayor"), Some("5"), None, None));
}

#[test]
fn a_locality_with_an_article_after_the_number() {
    assert_eq!(
        best("calle pintor laureano martinez 32 la Romana"),
        addr(Some("calle pintor laureano martinez"), Some("32"), None, Some("la Romana"))
    );
    assert_eq!(best("calle parpers 5 les Franqueses del Vallès"), addr(Some("calle parpers"), Some("5"), None, Some("les Franqueses del Vallès")));
    // The name still starts with the number when only the type precedes it.
    assert_eq!(best("Calle 13 de Septiembre"), addr(Some("Calle 13 de Septiembre"), None, None, None));
}

#[test]
fn a_misspelt_type_still_makes_an_address() {
    let q = parse("Crarer de Sant Roc 7, Ribera d'Ondara");
    assert!(q.interpretations.iter().all(|i| matches!(i.kind, InterpretationKind::Address(_))), "{q:?}");
}

#[test]
fn number_first() {
    assert_eq!(best("401 Carrer de Mallorca, Barcelona"), addr(Some("Carrer de Mallorca"), Some("401"), None, Some("Barcelona")));
}

#[test]
fn places() {
    assert_eq!(best("Tomelloso"), place("Tomelloso", None));
    assert_eq!(best("Sants, Barcelona"), place("Sants", Some("Barcelona")));
    let q = parse("sants barcelona");
    assert!(q.interpretations.iter().any(|i| i.kind == place("sants", Some("barcelona"))));
}

#[test]
fn street_without_number() {
    let q = parse("calle mayor alcala de henares");
    assert_eq!(q.interpretations[0].kind, addr(Some("calle mayor alcala de henares"), None, None, None));
    assert!(q.interpretations.iter().any(|i| i.kind == addr(Some("calle mayor"), None, None, Some("alcala de henares"))));
    assert_eq!(best("Passeig de Gràcia, Barcelona"), addr(Some("Passeig de Gràcia"), None, None, Some("Barcelona")));
}

#[test]
fn area_types_are_places_first() {
    assert_eq!(best("parc guell"), place("parc guell", None));
    assert!(parse("parc guell").interpretations.iter().any(|i| i.kind == addr(Some("parc guell"), None, None, None)));
    assert_eq!(best("carrer guell"), addr(Some("carrer guell"), None, None, None));
}

#[test]
fn postcodes() {
    assert_eq!(best("08013"), InterpretationKind::Postcode("08013".into()));
    assert_eq!(parse("99001").interpretations[0].kind, place("99001", None), "99 is not a province");
    assert!(parse("08013 barcelona").interpretations.iter().any(|i| i.kind == InterpretationKind::Postcode("08013".into())));
}

#[test]
fn coordinates_win() {
    let q = parse("41.3874, 2.1686");
    assert_eq!(q.interpretations.len(), 1);
    assert!(matches!(q.interpretations[0].kind, InterpretationKind::Coordinates(_)));
}

#[test]
fn never_panics_on_any_text() {
    let samples = [
        "Camí Curnia 106", "Avinguda de Cala Molí 7", "ñ", "ç 1 2", "°", "º 1", "40° N 3° W ñ", "30Ñ 440291 4474254",
        "s/n", ",,,", "  ;  ", "日本語 1", "émoji 🙂 12", "1", "12-", "-12", "Carrer d'en Ça 3 bis", "N", "40.5", "40,5",
    ];
    for s in samples {
        let _ = country_pack_es::parser().parse(s);
    }
}

#[test]
fn probabilities_are_normalized_and_bounded() {
    for text in ["calle mayor alcala de henares", "sants barcelona", "Carrer de Mallorca, 401, 08013 Barcelona", "08013 sants"] {
        let q = parse(text);
        let total: f32 = q.interpretations.iter().map(|i| i.probability).sum();
        assert!((total - 1.0).abs() < 1e-4, "{text}: {total}");
        assert!(q.interpretations.len() <= MAX_INTERPRETATIONS);
        assert!(q.interpretations.windows(2).all(|w| w[0].probability >= w[1].probability));
    }
    assert!(parse("   ").interpretations.is_empty());
}

/// Queries as people write them: addresses, places, postcodes and coordinates.
const QUERIES: &[&str] = &[
    "carrer de sant miquel 30, palma",
    "c/ mayor, 23, 28801",
    "calle mayor 23 alcala",
    "tomelloso",
    "el palmar murcia",
    "parque del retiro",
    "28801",
    "40.4168,-3.7038",
    "40°25'N 3°42'W",
    "30T 440000 4474000",
    "avinguda diagonal 640 barcelona",
];

#[test]
fn conformance() {
    geko_conformance::text::check_parser(&country_pack_es::parser(), QUERIES);
}

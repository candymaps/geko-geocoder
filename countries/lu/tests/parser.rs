//! Query parsing in Luxembourg: the generic rules with the pack of Luxembourg. Addresses are
//! written with the number first ("12, rue de la Gare") and the postcode with the country
//! prefix ("L-1611").

use geko_core::ports::{AddressParser, TextNormalizer};
use geko_core::query::{AddressComponents, InterpretationKind, ParsedQuery};

fn parse(text: &str) -> ParsedQuery {
    country_pack_lu::parser().parse(text).unwrap()
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

#[test]
fn addresses_with_the_number_after_the_street() {
    assert_eq!(best("rue de la Gare 12, Luxembourg"), addr(Some("rue de la Gare"), Some("12"), None, Some("Luxembourg")));
    assert_eq!(best("Avenue de la Liberté 34 1930 Luxembourg"), addr(Some("Avenue de la Liberté"), Some("34"), Some("1930"), Some("Luxembourg")));
}

#[test]
fn addresses_with_the_number_first() {
    assert_eq!(best("12, rue de la Gare, Luxembourg"), addr(Some("rue de la Gare"), Some("12"), None, Some("Luxembourg")));
    assert_eq!(best("34 Avenue de la Liberté, L-1930 Luxembourg"), addr(Some("Avenue de la Liberté"), Some("34"), Some("1930"), Some("Luxembourg")));
}

#[test]
fn street_types_in_three_languages() {
    let n = country_pack_lu::normalizer();
    assert_eq!(n.street_parts("Rue de la Gare").type_code, Some("RUE"));
    assert_eq!(n.street_parts("Garer Strooss").type_code, None, "the type goes last in Luxembourgish");
    assert_eq!(n.street_parts("Boulevard Royal").type_code, Some("BD"));
    assert_eq!(n.street_parts("Montée de la Pétrusse").type_code, Some("MTE"));
}

#[test]
fn queries_read_street_types_in_english() {
    let n = country_pack_lu::normalizer();
    assert_eq!(n.query_street_parts("Gare Street"), n.street_parts("Rue de la Gare"));
    assert_eq!(n.query_street_parts("Road Arlon").type_code, Some("RTE"));
    assert_eq!(best("12 Gare Street, Luxembourg"), addr(Some("Gare Street"), Some("12"), None, Some("Luxembourg")));
}

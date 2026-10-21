//! Text rules in Spain: the generic normalizer with the pack of Spain.

use geko_core::model::HouseNumber;
use geko_core::ports::TextNormalizer;

fn folded(text: &str) -> String {
    let mut s = String::new();
    text_rules::fold(text, &mut s);
    s
}

fn parts(name: &str) -> (Option<&'static str>, String) {
    let p = country_pack_es::normalizer().street_parts(name);
    (p.type_code, p.core)
}

#[test]
fn folding() {
    assert_eq!(folded("Plaça de Mossèn Jacint Verdaguer"), "placa de mossen jacint verdaguer");
    assert_eq!(folded("Carrer de l'Hospital"), "carrer de l hospital");
    assert_eq!(folded("Passeig de la Pau, 1ª"), "passeig de la pau 1");
    assert_eq!(folded("Col·legi  d'Advocats"), "collegi d advocats");
    assert_eq!(folded("  ÑANDÚ  "), "nandu");
}

#[test]
fn same_street_in_both_sources() {
    let pairs = [
        ("Carrer de Mallorca", "CALLE MALLORCA"),
        ("Avinguda Diagonal", "AVENIDA DIAGONAL"),
        ("Avinguda Diagonal", "AVINGUDA DIAGONAL"),
        ("Plaça de Mossèn Jacint Verdaguer", "PLAZA MOSSEN JACINT VERDAGUER"),
        ("Passeig de Gràcia", "PASEO GRACIA"),
        ("Carrer del Consell de Cent", "CALLE CONSELL DE CENT"),
        ("Carrer de Pi i Margall", "CALLE PI Y MARGALL"),
        ("Passatge de Permanyer", "PASAJE, PERMANYER"),
        ("Travessera de Gràcia", "TRAVESSERA GRACIA"),
        ("La Rambla", "RAMBLA"),
    ];
    for (osm, cartociudad) in pairs {
        assert_eq!(parts(osm), parts(cartociudad), "{osm} / {cartociudad}");
    }
}

#[test]
fn different_streets_stay_different() {
    assert_ne!(parts("Plaça de Catalunya"), parts("Rambla de Catalunya"));
    assert_ne!(parts("Carrer de Balmes"), parts("Carrer de Bailèn"));
}

#[test]
fn names_without_type() {
    assert_eq!(parts("Gran Via de les Corts Catalanes"), (None, "gran via corts catalanes".into()));
    assert_eq!(parts("Ronda"), (Some("RD"), "ronda".into()));
    assert_eq!(parts("Diseminado"), (Some("DS"), "diseminado".into()));
}

#[test]
fn a_generic_type_alone_is_no_name() {
    for name in ["CALLE", "Carrer de", "Camino", "AVENIDA", "Carretera"] {
        assert_eq!(parts(name).1, "", "{name}");
    }
}

#[test]
fn queries_tolerate_two_swapped_letters_in_the_type() {
    let q = |name: &str| country_pack_es::normalizer().query_street_parts(name);
    assert_eq!(q("Crarer de Sant Roc"), country_pack_es::normalizer().street_parts("Carrer de Sant Roc"));
    assert_eq!(q("Ubranizacion Torre Esteril").type_code, Some("UR"));
    assert_eq!(q("Cmaino Santa Lucia").type_code, Some("CM"));
    // Indexing never guesses, and a changed letter is no typo: "Valle" is not "Calle".
    assert_eq!(country_pack_es::normalizer().street_parts("Crarer de Sant Roc").type_code, None);
    assert_eq!(q("Valle Inclan").type_code, None);
    assert_eq!(q("Carrer de Mallorca"), country_pack_es::normalizer().street_parts("Carrer de Mallorca"));
}

#[test]
fn queries_read_street_types_in_english() {
    let n = country_pack_es::normalizer();
    let q = |name: &str| n.query_street_parts(name);
    assert_eq!(q("Street Femenies"), n.street_parts("Carrer de Femenies"));
    assert_eq!(q("Femenies Street"), n.street_parts("Carrer de Femenies"));
    assert_eq!(q("street femeneis").type_code, Some("CL"));
    assert_eq!(q("Jaume III Avenue").type_code, Some("AV"));
    assert_eq!(q("Major Square").type_code, Some("PZ"));
    assert_eq!(q("Street").core, "", "a generic type alone is no name");
    assert_eq!(q("Carrer de Street"), n.street_parts("Carrer de Street"), "the pack's own type comes first");
    assert_eq!(n.street_parts("Femenies Street").type_code, None, "the names of the sources are read as written");
}

#[test]
fn a_letter_after_the_type_is_the_name() {
    assert_eq!(parts("CALLE A"), (Some("CL"), "a".into()));
    assert_eq!(parts("Carrer D"), (Some("CL"), "d".into()));
    assert_ne!(parts("CALLE A"), parts("CALLE E"));
}

#[test]
fn house_numbers() {
    let n = |raw| country_pack_es::normalizer().parse_housenumber(raw);
    let hn = |number, suffix: Option<&str>, range_end| Some(HouseNumber { number, suffix: suffix.map(Into::into), range_end });
    assert_eq!(n("401"), hn(401, None, None));
    assert_eq!(n("12B"), hn(12, Some("B"), None));
    assert_eq!(n("12 b"), hn(12, Some("B"), None));
    assert_eq!(n("12-B"), hn(12, Some("B"), None));
    assert_eq!(n("12 bis"), hn(12, Some("BIS"), None));
    assert_eq!(n("187-189"), hn(187, None, Some(189)));
    assert_eq!(n("187-189 A"), hn(187, Some("A"), Some(189)));
    assert_eq!(n("s/n"), None);
    assert_eq!(n("SN"), None);
    assert_eq!(n("0"), None);
    assert_eq!(n("5-3"), hn(5, None, None));
    assert_eq!(hn(187, Some("A"), Some(189)).unwrap().label(), "187-189A");
}

#[test]
fn street_type_compatibility() {
    let c = |a, b| country_pack_es::normalizer().street_types_compatible(a, b);
    assert!(c(Some("AV"), Some("AV")));
    assert!(c(Some("AV"), Some("CL")), "generic street type");
    assert!(c(None, Some("PZ")));
    assert!(c(Some("PJ"), Some("PS")), "CartoCiudad's PASEO for Passatge");
    assert!(!c(Some("PZ"), Some("RB")));
}

#[test]
fn variants_cover_every_language() {
    let mut out = Vec::new();
    country_pack_es::normalizer().variants("Carrer de Mallorca", &mut out);
    for v in ["carrer mallorca", "calle mallorca", "mallorca", "carrer de mallorca"] {
        assert!(out.contains(&v.to_string()), "{v} not in {out:?}");
    }
}

#[test]
fn codes_are_unique() {
    let mut codes: Vec<&str> = country_pack_es::pack().street_types.iter().map(|t| t.code.as_str()).collect();
    codes.sort();
    codes.dedup();
    assert_eq!(codes.len(), country_pack_es::pack().street_types.len());
}

#[test]
fn longest_type_wins() {
    assert_eq!(parts("Poligono Industrial Can Roca"), (Some("PO"), "can roca".into()));
    assert_eq!(parts("Poligono Can Roca"), (Some("PO"), "can roca".into()));
    assert_eq!(parts("Gran Via").0, None);
}

/// Names as OSM and CartoCiudad write them, in the languages of Spain.
const NAMES: &[&str] = &[
    "Carrer de Mallorca",
    "CALLE MALLORCA",
    "Avinguda Diagonal",
    "Plaça de Mossèn Jacint Verdaguer",
    "Rúa do Franco",
    "Kale Nagusia",
    "La Rambla",
    "Ronda",
    "Calle A",
    "Camí de s'Arenal",
    "Passeig del Born",
    "Gran Via de les Corts Catalanes",
    "Avenida de la Constitución",
    "Polígono Industrial Son Castelló",
];

#[test]
fn conformance() {
    geko_conformance::text::check_normalizer(&country_pack_es::normalizer(), NAMES);
}

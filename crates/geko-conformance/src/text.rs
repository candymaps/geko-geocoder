//! Conformance of the text ports of a country: `TextNormalizer` and `AddressParser`. Properties
//! every country must keep, plus samples of real names and queries that the caller provides.

use geko_core::ports::{AddressParser, TextNormalizer};
use geko_core::query::InterpretationKind;

/// Hostile inputs: empty, blank, combining marks, controls, other scripts, very long.
fn hostile() -> Vec<String> {
    let mut v: Vec<String> = ["", " ", "\t\n", "e\u{301}", "\u{0}", "東京", "🙂🙂", "---", ",,,", "1/2", "Ⅻ", "ß", "İ", "\u{202e}abc"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    v.push("carrer ".repeat(300));
    v.push("9".repeat(1000));
    v
}

/// `names`: real names of the country, written as its sources write them.
pub fn check_normalizer(n: &dyn TextNormalizer, names: &[&str]) {
    let (mut once, mut twice, mut tokens, mut variants) = (String::new(), String::new(), Vec::new(), Vec::new());
    let inputs: Vec<String> = names.iter().map(|s| s.to_string()).chain(hostile()).collect();
    for name in &inputs {
        n.normalize(name, &mut once);
        n.normalize(&once, &mut twice);
        assert_eq!(once, twice, "normalize is idempotent: {name:?}");
        n.tokenize(&once, &mut tokens);
        assert!(tokens.iter().all(|t| !t.is_empty() && !t.contains(' ')), "tokens of {name:?}: {tokens:?}");
        n.variants(name, &mut variants);
        let _ = n.street_parts(name);
        let _ = n.parse_housenumber(name);
    }
    // Case checks on real names only: "ß" in capitals is "SS", another text.
    for name in names {
        assert_eq!(n.street_parts(name), n.street_parts(&name.to_uppercase()), "street parts ignore case: {name:?}");
        assert_eq!(n.query_street_parts(name), n.street_parts(name), "a well-written name reads the same in a query: {name:?}");
        n.normalize(name, &mut once);
        assert!(!once.is_empty(), "{name:?} normalizes to something");
        n.variants(name, &mut variants);
        assert!(variants.contains(&once), "the variants of {name:?} include its normalized form: {variants:?}");
        let upper = name.to_uppercase();
        let mut shout = String::new();
        n.normalize(&upper, &mut shout);
        assert_eq!(once, shout, "normalize ignores case: {name:?}");
    }

    // House numbers: digits are numbers; "0" and words are none.
    assert_eq!(n.parse_housenumber("12").map(|h| h.number), Some(12));
    assert_eq!(n.parse_housenumber("0"), None);
    assert_eq!(n.parse_housenumber("abc"), None);

    // Type compatibility is reflexive, symmetric and lets unknown types match anything.
    let codes: Vec<Option<&str>> = names.iter().map(|s| n.street_parts(s).type_code).collect();
    for &a in &codes {
        assert!(n.street_types_compatible(a, a));
        assert!(n.street_types_compatible(a, None) && n.street_types_compatible(None, a));
        for &b in &codes {
            assert_eq!(n.street_types_compatible(a, b), n.street_types_compatible(b, a), "{a:?} {b:?}");
        }
    }
}

/// `queries`: real queries of the country, each expected to have at least one interpretation.
pub fn check_parser(p: &dyn AddressParser, queries: &[&str]) {
    for text in queries.iter().map(|s| s.to_string()).chain(hostile()) {
        let parsed = p.parse(&text).unwrap_or_else(|e| panic!("parse {text:?}: {e}"));
        assert!(text.contains(parsed.raw.trim()), "{text:?}: raw {:?} is not the query", parsed.raw);
        assert!(parsed.interpretations.len() <= 8, "{text:?}: {} interpretations", parsed.interpretations.len());
        let total: f32 = parsed.interpretations.iter().map(|i| i.probability).sum();
        assert!(total <= 1.0 + 1e-3, "{text:?}: probabilities add up to {total}");
        for pair in parsed.interpretations.windows(2) {
            assert!(pair[0].probability >= pair[1].probability, "{text:?}: not sorted by probability");
        }
        for i in &parsed.interpretations {
            assert!((0.0..=1.0).contains(&i.probability), "{text:?}: probability {}", i.probability);
            assert_eq!(i.country, p.country(), "{text:?}: interpretation of another country");
            if let InterpretationKind::Address(a) = &i.kind {
                assert!(a.street.is_some() || a.locality.is_some() || a.postcode.is_some(), "{text:?}: empty address");
            }
        }
    }
    for text in queries {
        assert!(!p.parse(text).unwrap().interpretations.is_empty(), "{text:?} has an interpretation");
    }
    assert!(p.parse("").unwrap().interpretations.is_empty(), "nothing to interpret in an empty query");
}

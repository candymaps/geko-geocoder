//! Words that are the same in the languages of a country ("major" and "mayor", "sant" and "san"),
//! learned from the entities named in several of them (`name:ca`, `name:es`…) when the segment
//! is built. A query that finds nothing in the place it names is retried translated word by word,
//! so a query in one language finds names written only in another (PROJECT.md §6.5.1). The index
//! stays literal: indexing the equivalents too made the lists of common words ("sant" with every
//! "san") so long that the right street fell out of the candidates.
//!
//! The names of an entity in two languages are aligned word by word when they have the same
//! street type and as many significant words. A pair of different words is kept when it is seen
//! in enough distinct names, is the usual reading of one of its words and not a rare one of the
//! other: this rules out the chance alignments of languages that order words differently
//! ("Mikel Deuna" and "San Miguel" align "mikel" with "san").
//!
//! The pack of a country adds the pairs learned with the whole country in scope, so a build of one
//! region knows them too, and the pairs it rules out (`TextNormalizer::equivalences`).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use geko_core::ports::TextNormalizer;

/// Distinct aligned names a pair must appear in.
const MIN_NAMES: usize = 3;
/// Share of the alignments of one of its words the pair must account for.
const MIN_SHARE: f64 = 0.3;
/// Share of the alignments of the other word.
const MIN_OTHER_SHARE: f64 = 0.05;

pub type Equivalents = HashMap<String, Vec<String>>;

/// The learned pairs in a segment, in its cold part.
pub const FILE: &str = "equivalences.txt";

pub fn learn(multilingual: &[Vec<(String, String)>], normalizer: &dyn TextNormalizer) -> Equivalents {
    let languages = normalizer.languages();
    let (mut buf, mut tokens) = (String::new(), Vec::new());
    let mut aligned: BTreeSet<(Vec<String>, Vec<String>)> = BTreeSet::new();
    for names in multilingual {
        let parts: Vec<_> = names
            .iter()
            .filter(|(lang, _)| languages.is_empty() || languages.contains(lang))
            .map(|(_, name)| {
                let parts = normalizer.street_parts(name);
                normalizer.normalize(&parts.core, &mut buf);
                normalizer.tokenize(&buf, &mut tokens);
                (parts.type_code, tokens.clone())
            })
            .collect();
        for (i, (type_a, a)) in parts.iter().enumerate() {
            for (type_b, b) in &parts[i + 1..] {
                if type_a == type_b && a.len() == b.len() && !a.is_empty() && a != b {
                    aligned.insert(if a < b { (a.clone(), b.clone()) } else { (b.clone(), a.clone()) });
                }
            }
        }
    }

    let mut seen: HashMap<&str, usize> = HashMap::new();
    let mut pairs: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for (a, b) in &aligned {
        for (x, y) in a.iter().zip(b) {
            *seen.entry(x).or_default() += 1;
            *seen.entry(y).or_default() += 1;
            if x != y && !x.bytes().any(|c| c.is_ascii_digit()) && !y.bytes().any(|c| c.is_ascii_digit()) {
                let key = if x < y { (x.as_str(), y.as_str()) } else { (y.as_str(), x.as_str()) };
                *pairs.entry(key).or_default() += 1;
            }
        }
    }

    let (known, excluded) = normalizer.equivalences();
    let ordered = |x: &str, y: &str| if x < y { (x.to_owned(), y.to_owned()) } else { (y.to_owned(), x.to_owned()) };
    let excluded: BTreeSet<(String, String)> = excluded.iter().map(|(x, y)| ordered(x, y)).collect();
    let mut kept: BTreeSet<(String, String)> = known.iter().map(|(x, y)| ordered(x, y)).collect();
    for ((x, y), n) in pairs {
        let (sx, sy) = (n as f64 / seen[x] as f64, n as f64 / seen[y] as f64);
        if n >= MIN_NAMES && sx.max(sy) >= MIN_SHARE && sx.min(sy) >= MIN_OTHER_SHARE {
            kept.insert(ordered(x, y));
        }
    }
    let mut out = Equivalents::new();
    for (x, y) in kept.difference(&excluded) {
        out.entry(x.clone()).or_default().push(y.clone());
        out.entry(y.clone()).or_default().push(x.clone());
    }
    for list in out.values_mut() {
        list.sort();
        list.dedup();
    }
    out
}

/// Translations of a name.
const MAX_TRANSLATIONS: usize = 8;

/// The name of `tokens` with its words replaced by their equivalents, in every combination up to
/// `MAX_TRANSLATIONS`, without the name itself.
pub fn translations(tokens: &[String], equivalents: &Equivalents) -> Vec<String> {
    if !tokens.iter().any(|t| equivalents.contains_key(t)) {
        return Vec::new();
    }
    let mut names: Vec<Vec<&str>> = vec![Vec::new()];
    for t in tokens {
        let options = std::iter::once(t.as_str()).chain(equivalents.get(t).into_iter().flatten().map(String::as_str));
        let options: Vec<&str> = options.collect();
        names = names
            .iter()
            .flat_map(|n| options.iter().map(move |o| n.iter().copied().chain(std::iter::once(*o)).collect::<Vec<&str>>()))
            .take(MAX_TRANSLATIONS + 1)
            .collect();
    }
    names.into_iter().skip(1).map(|n| n.join(" ")).collect()
}

/// Reads the pairs written by `listing`.
pub fn parse(text: &str) -> Equivalents {
    let mut out = Equivalents::new();
    for (a, b) in text.lines().filter_map(|l| l.split_once(' ')) {
        out.entry(a.to_owned()).or_default().push(b.to_owned());
        out.entry(b.to_owned()).or_default().push(a.to_owned());
    }
    for list in out.values_mut() {
        list.sort();
    }
    out
}

/// One line per pair, sorted: what a segment learned, read back by `parse`.
pub fn listing(equivalents: &Equivalents) -> String {
    let mut pairs: Vec<(&str, &str)> =
        equivalents.iter().flat_map(|(w, eq)| eq.iter().filter(|e| w.as_str() < e.as_str()).map(move |e| (w.as_str(), e.as_str()))).collect();
    pairs.sort_unstable();
    pairs.iter().map(|(a, b)| format!("{a} {b}\n")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(l, n)| (l.to_string(), n.to_string())).collect()
    }

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    /// The text rules of Spain with other known and ruled-out pairs than those of its pack.
    struct Rules {
        known: Vec<(String, String)>,
        excluded: Vec<(String, String)>,
    }

    impl TextNormalizer for Rules {
        fn country(&self) -> geko_core::country::CountryCode {
            country_pack_es::normalizer().country()
        }
        fn normalize(&self, text: &str, out: &mut String) {
            country_pack_es::normalizer().normalize(text, out)
        }
        fn tokenize(&self, normalized: &str, out: &mut Vec<String>) {
            country_pack_es::normalizer().tokenize(normalized, out)
        }
        fn variants(&self, name: &str, out: &mut Vec<String>) {
            country_pack_es::normalizer().variants(name, out)
        }
        fn street_parts(&self, name: &str) -> geko_core::model::StreetParts {
            country_pack_es::normalizer().street_parts(name)
        }
        fn parse_housenumber(&self, raw: &str) -> Option<geko_core::model::HouseNumber> {
            country_pack_es::normalizer().parse_housenumber(raw)
        }
        fn street_types_compatible(&self, a: Option<&str>, b: Option<&str>) -> bool {
            country_pack_es::normalizer().street_types_compatible(a, b)
        }
        fn languages(&self) -> &[String] {
            country_pack_es::pack().languages.as_slice()
        }
        fn equivalences(&self) -> (&geko_core::ports::WordPairs, &geko_core::ports::WordPairs) {
            (&self.known, &self.excluded)
        }
    }

    #[test]
    fn learns_words_seen_aligned_in_several_names() {
        let normalizer = Rules { known: Vec::new(), excluded: Vec::new() };
        let mut data = Vec::new();
        for (ca, es) in [("Carrer Major Nou", "Calle Mayor Nueva"), ("Carrer Major", "Calle Mayor"), ("Carrer Major de Sant Joan", "Calle Mayor de San Juan")] {
            data.push(names(&[("ca", ca), ("es", es)]));
        }
        data.push(names(&[("ca", "Sant Pere"), ("es", "San Pedro")]));
        data.push(names(&[("ca", "Sant Pau"), ("es", "San Pablo")]));
        // Different street types are not aligned; English is not a language of the country.
        data.push(names(&[("ca", "Avinguda Major"), ("es", "Calle Grande")]));
        data.push(names(&[("es", "Calle Mayor"), ("en", "Main Street")]));
        let eq = learn(&data, &normalizer);
        assert_eq!(eq["major"], ["mayor"]);
        assert_eq!(eq["mayor"], ["major"]);
        assert_eq!(eq["sant"], ["san"]);
        assert!(!eq.contains_key("joan"), "seen in one name only");
        assert!(!eq.contains_key("grande") && !eq.contains_key("main"));
    }

    #[test]
    fn the_pack_adds_known_pairs_and_rules_out_others() {
        let data: Vec<_> = ["Major Nou", "Major", "Major de Baix"]
            .iter()
            .map(|ca| names(&[("ca", &format!("Carrer {ca}")), ("es", &format!("Calle {}", ca.replace("Major", "Mayor")))]))
            .collect();
        let normalizer = Rules { known: pairs(&[("oms", "olmos")]), excluded: pairs(&[("mayor", "major")]) };
        let eq = learn(&data, &normalizer);
        assert_eq!(eq["olmos"], ["oms"]);
        assert!(!eq.contains_key("major"), "ruled out in either order");
    }

    #[test]
    fn translates_names_word_by_word() {
        let eq = Equivalents::from([
            ("sant".to_owned(), vec!["san".to_owned()]),
            ("joan".to_owned(), vec!["juan".to_owned(), "xoan".to_owned()]),
        ]);
        let tokens = |s: &str| s.split(" ").map(str::to_owned).collect::<Vec<_>>();
        assert_eq!(
            translations(&tokens("placa sant joan"), &eq),
            ["placa sant juan", "placa sant xoan", "placa san joan", "placa san juan", "placa san xoan"]
        );
        assert!(translations(&tokens("placa major"), &eq).is_empty());
    }
}

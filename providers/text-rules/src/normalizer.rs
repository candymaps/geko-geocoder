//! Text normalization from a country pack (PROJECT.md §6.5.1): folding, street types, connecting
//! words and house numbers.

use geko_core::country::CountryCode;
use geko_core::model::{HouseNumber, StreetParts};
use geko_core::ports::{TextNormalizer, WordPairs};

use crate::{Pack, StreetType};

/// Lowercase, without diacritics, with every non-alphanumeric character as a single space.
/// `l·l` becomes `ll` and `º`/`ª` disappear.
pub fn fold(text: &str, out: &mut String) {
    out.clear();
    let mut pending_space = false;
    let push = |c: char, out: &mut String, pending: &mut bool| {
        if *pending && !out.is_empty() {
            out.push(' ');
        }
        *pending = false;
        out.push(c);
    };
    // Letters of the word so far, while it has only letters (a road code is one to three of
    // them), and of the previous word while only spaces follow it.
    let (mut letters, mut before_spaces) = (0, 0);
    let mut joining = false;
    for (i, c) in text.char_indices() {
        // A road code is one word however it is written: "A-7", "A - 7" and "A7" are "a7", not
        // the article "a" and a number.
        if c == '-' {
            let code = if letters > 0 { letters } else { before_spaces };
            if (1..=3).contains(&code) && text[i + 1..].trim_start_matches(' ').as_bytes().first().is_some_and(u8::is_ascii_digit) {
                (letters, before_spaces, pending_space, joining) = (0, 0, false, true);
                continue;
            }
        }
        if joining && c == ' ' {
            continue;
        }
        joining = false;
        before_spaces = match c {
            ' ' if letters > 0 => letters,
            ' ' => before_spaces,
            _ => 0,
        };
        letters = if c.is_alphabetic() && letters < usize::MAX { letters + 1 } else if c.is_alphanumeric() { usize::MAX } else { 0 };
        let folded = match c {
            'a'..='z' | '0'..='9' => Some(c),
            'A'..='Z' => Some(c.to_ascii_lowercase()),
            'à' | 'á' | 'â' | 'ä' | 'ã' | 'À' | 'Á' | 'Â' | 'Ä' | 'Ã' => Some('a'),
            'è' | 'é' | 'ê' | 'ë' | 'È' | 'É' | 'Ê' | 'Ë' => Some('e'),
            'ì' | 'í' | 'î' | 'ï' | 'Ì' | 'Í' | 'Î' | 'Ï' => Some('i'),
            'ò' | 'ó' | 'ô' | 'ö' | 'õ' | 'Ò' | 'Ó' | 'Ô' | 'Ö' | 'Õ' => Some('o'),
            'ù' | 'ú' | 'û' | 'ü' | 'Ù' | 'Ú' | 'Û' | 'Ü' => Some('u'),
            'ç' | 'Ç' => Some('c'),
            'ñ' | 'Ñ' => Some('n'),
            '·' | 'º' | 'ª' => None,
            _ => {
                pending_space = true;
                None
            }
        };
        if let Some(f) = folded {
            push(f, out, &mut pending_space);
        }
    }
}

fn folded(text: &str) -> String {
    let mut s = String::with_capacity(text.len());
    fold(text, &mut s);
    s
}

/// Whether `a` is `b` with two adjacent letters swapped ("crarer", "carrer"), the commonest
/// typo. Changed letters are not accepted: "valle" is not a typo of "calle".
fn swapped(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.len() != b.len() || a == b {
        return false;
    }
    let diff: Vec<usize> = (0..a.len()).filter(|&i| a[i] != b[i]).collect();
    matches!(diff[..], [i, j] if j == i + 1 && a[i] == b[j] && a[j] == b[i])
}

/// The normalizer of a country, from its pack.
#[derive(Debug, Clone, Copy)]
pub struct RulesNormalizer {
    pack: &'static Pack,
}

impl RulesNormalizer {
    pub fn new(pack: &'static Pack) -> Self {
        Self { pack }
    }

    pub fn pack(&self) -> &'static Pack {
        self.pack
    }

    pub fn is_connector(&self, folded_token: &str) -> bool {
        self.pack.words.connectors.iter().any(|c| c == folded_token)
    }

    pub fn street_type(&self, code: &str) -> Option<&'static StreetType> {
        self.pack.street_type(code)
    }

    fn is_area(&self, code: &str) -> bool {
        self.pack.street_type(code).is_some_and(|t| t.area)
    }

    fn is_generic(&self, code: &str) -> bool {
        self.pack.street_type(code).is_some_and(|t| t.generic)
    }

    /// The street type a name starts with ("calle", "avinguda", "c/"…), after any article.
    pub fn leading_street_type(&self, name: &str) -> Option<&'static str> {
        let f = folded(name);
        let tokens: Vec<&str> = f.split(' ').filter(|t| !t.is_empty()).collect();
        let first = tokens.iter().position(|t| !self.is_connector(t)).unwrap_or(tokens.len());
        self.match_prefix(&tokens[first..]).map(|(code, _)| code)
    }

    /// Whether a name starts with a street type that names a way, strong evidence of an address.
    pub fn starts_with_way_type(&self, name: &str) -> bool {
        self.leading_street_type(name).is_some_and(|t| !self.is_area(t))
    }

    /// Like `starts_with_way_type`, for a query: a type with two letters swapped counts
    /// ("Crarer de Sant Roc").
    pub fn query_starts_with_way_type(&self, name: &str) -> bool {
        self.parts(name, true).type_code.is_some_and(|t| !self.is_area(t))
    }

    /// The longest street type at the start of `tokens`, and how many tokens it uses.
    fn match_prefix(&self, tokens: &[&str]) -> Option<(&'static str, usize)> {
        let mut best: Option<(&'static str, usize)> = None;
        for t in &self.pack.street_types {
            for word in &t.words {
                let parts: Vec<&str> = word.split(' ').collect();
                if tokens.len() > parts.len() - 1 && tokens[..parts.len()] == parts[..] && best.is_none_or(|(_, used)| parts.len() > used) {
                    best = Some((t.code.as_str(), parts.len()));
                }
            }
        }
        best
    }

    /// The street type whose word `token` is with two adjacent letters swapped, for words of 5+
    /// letters and only if a single type matches.
    fn match_swapped(&self, token: &str) -> Option<&'static str> {
        if token.chars().count() < 5 {
            return None;
        }
        let mut found: Option<&'static str> = None;
        for t in &self.pack.street_types {
            if t.words.iter().any(|w| !w.contains(' ') && swapped(token, w)) {
                if found.is_some_and(|f| f != t.code) {
                    return None;
                }
                found = Some(t.code.as_str());
            }
        }
        found
    }

    /// Whether a query names a street type and nothing else ("Carrer de", "Street"), as the
    /// first words before a house number: then the number may be part of the name. Cheaper than
    /// `query_street_parts`, whose swapped letters it does not need.
    pub fn is_street_type_alone(&self, name: &str) -> bool {
        if self.parts(name, false).core.is_empty() {
            return true;
        }
        let f = folded(name);
        let mut words = f.split(' ').filter(|t| !t.is_empty() && !self.is_connector(t));
        matches!((words.next(), words.next()), (Some(w), None) if self.match_foreign(w, false).is_some_and(|c| self.is_generic(c)))
    }

    /// The type of the pack that `token` names in another language (`languages/*.toml`).
    fn match_foreign(&self, token: &str, after: bool) -> Option<&'static str> {
        self.pack.foreign_types.iter().find(|f| f.word == token && (f.after || !after)).map(|f| f.code.as_str())
    }

    /// Splits a street name into its type code and normalized core (PROJECT.md §6.5.1). With
    /// `typo`, for queries, a first word that is a street type with two letters swapped counts as
    /// that type, and so does a street type of another language, first or, where that language
    /// writes it so, last ("Street Femenies", "Femenies Street").
    fn parts(&self, name: &str, typo: bool) -> StreetParts {
        let f = folded(name);
        let tokens: Vec<&str> = f.split(' ').filter(|t| !t.is_empty()).collect();
        let first = tokens.iter().position(|t| !self.is_connector(t)).unwrap_or(tokens.len());
        let typed = self.match_prefix(&tokens[first..]).or_else(|| {
            let t = tokens.get(first).filter(|_| typo)?;
            self.match_swapped(t).or_else(|| self.match_foreign(t, false)).map(|c| (c, 1))
        });
        let trailing = || match tokens[first..] {
            [.., name, last] if typo && !self.is_connector(name) => self.match_foreign(last, true),
            _ => None,
        };
        let (type_code, rest) = match typed {
            Some((code, used)) => (Some(code), &tokens[first + used..]),
            None => match trailing() {
                Some(code) => (Some(code), &tokens[first..tokens.len() - 1]),
                None => (None, &tokens[first..]),
            },
        };
        let mut core: Vec<&str> = rest.iter().copied().filter(|t| !self.is_connector(t)).collect();
        // "Calle A", "Carrer D": a letter alone after the type is the name, not a connector.
        if core.is_empty()
            && let [letter] = rest
            && letter.len() == 1
        {
            core.push(*letter);
        }
        // "La Rambla", "Ronda": the type word is the whole name. A generic type alone ("Calle",
        // "Carrer de") is no name: the core stays empty and the street counts as unnamed.
        if core.is_empty() && !type_code.is_some_and(|c| self.is_generic(c)) {
            core = tokens[first..].iter().copied().filter(|t| !self.is_connector(t)).collect();
        }
        StreetParts { type_code, core: core.join(" ") }
    }
}

impl TextNormalizer for RulesNormalizer {
    fn country(&self) -> CountryCode {
        self.pack.country
    }

    fn normalize(&self, text: &str, out: &mut String) {
        fold(text, out);
    }

    fn tokenize(&self, normalized: &str, out: &mut Vec<String>) {
        out.clear();
        out.extend(normalized.split(' ').filter(|t| !t.is_empty()).map(str::to_owned));
    }

    fn variants(&self, name: &str, out: &mut Vec<String>) {
        out.clear();
        let full = folded(name);
        let parts = self.street_parts(name);
        if let Some(t) = parts.type_code.and_then(|code| self.street_type(code)) {
            for word in &t.display {
                out.push(format!("{word} {}", parts.core));
            }
        }
        out.push(parts.core);
        out.push(full);
        out.sort();
        out.dedup();
        out.retain(|v| !v.is_empty());
    }

    fn street_parts(&self, name: &str) -> StreetParts {
        self.parts(name, false)
    }

    fn query_street_parts(&self, name: &str) -> StreetParts {
        self.parts(name, true)
    }

    fn parse_housenumber(&self, raw: &str) -> Option<HouseNumber> {
        let s = raw.trim().to_ascii_uppercase();
        let digits_end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
        let number: u32 = s[..digits_end].parse().ok()?;
        if number == 0 {
            return None;
        }
        let rest = s[digits_end..].trim_start_matches([' ', '-', ',', '/', '.']);
        let range_digits = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        let (range_end, rest) = match rest[..range_digits].parse::<u32>() {
            Ok(end) if end > number => (Some(end), &rest[range_digits..]),
            Ok(_) => (None, &rest[range_digits..]),
            Err(_) => (None, rest),
        };
        let word: String = rest.chars().filter(|c| c.is_ascii_alphabetic()).collect();
        let words = &self.pack.words;
        let suffix = if word.is_empty() {
            None
        } else if words.housenumber_suffixes.contains(&word) || word.len() == 1 {
            Some(word)
        } else {
            words.housenumber_suffix_prefixes.iter().find(|p| word.starts_with(p.as_str())).cloned()
        };
        Some(HouseNumber { number, suffix, range_end })
    }

    fn languages(&self) -> &[String] {
        &self.pack.languages
    }

    fn equivalences(&self) -> (&WordPairs, &WordPairs) {
        (&self.pack.equivalences, &self.pack.not_equivalent)
    }

    fn is_connector(&self, folded: &str) -> bool {
        RulesNormalizer::is_connector(self, folded)
    }

    fn street_types_compatible(&self, a: Option<&str>, b: Option<&str>) -> bool {
        match (a, b) {
            (Some(a), Some(b)) => {
                let any = self.pack.any_type.as_deref();
                a == b
                    || Some(a) == any
                    || Some(b) == any
                    || self.pack.interchanged_types.iter().any(|(x, y)| (x == a && y == b) || (x == b && y == a))
            }
            _ => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folding() {
        assert_eq!(folded("Plaça de Mossèn Jacint Verdaguer"), "placa de mossen jacint verdaguer");
        assert_eq!(folded("Carrer de l'Hospital"), "carrer de l hospital");
        assert_eq!(folded("Col·legi  d'Advocats"), "collegi d advocats");
        assert_eq!(folded("  ÑANDÚ  "), "nandu");
    }

    #[test]
    fn road_codes_are_one_word() {
        assert_eq!(folded("Carretera A-7"), "carretera a7");
        assert_eq!(folded("N-340 km 1050"), "n340 km 1050");
        assert_eq!(folded("CV-500"), "cv500");
        assert_eq!(folded("LU-P-2804"), "lu p2804");
        assert_eq!(folded("Carretera N - 340"), "carretera n340", "as some sources write it");
        assert_eq!(folded("Ma - 15"), "ma15");
        assert_eq!(folded("Sant Joan - 2"), "sant joan 2", "a word of four letters is no code");
        assert_eq!(folded("Santa-Ana 3"), "santa ana 3", "a word, not a code");
        assert_eq!(folded("12-B"), "12 b");
        assert_eq!(folded("A-"), "a", "nothing typed after the hyphen yet");
    }

    #[test]
    fn swapped_letters() {
        assert!(swapped("crarer", "carrer"));
        assert!(!swapped("valle", "calle"));
        assert!(!swapped("calle", "calle"));
    }
}

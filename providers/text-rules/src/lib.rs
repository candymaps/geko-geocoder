//! Generic text rules (PROJECT.md §2.4): one normalizer and one query parser for every country,
//! driven by the data of its pack (`countries/<country>/pack.toml` and `data/*.toml`). A country
//! adds its street types, connecting words and address conventions as data, not code.

mod coords;
mod normalizer;
mod parser;

use geko_core::country::CountryCode;
use geko_core::model::AdminLevels;
use geko_core::{GekoError, Result};
use serde::Deserialize;

pub use normalizer::{RulesNormalizer, fold};
pub use parser::{MAX_INTERPRETATIONS, RulesParser};

/// The text data of a country pack, as its files describe it.
#[derive(Debug)]
pub struct Pack {
    pub country: CountryCode,
    /// The version of the pack, raised by hand with every change to it: what `geko plan` and the
    /// build show, and the tag it is published with (`pack-<n>`).
    pub version: u32,
    pub languages: Vec<String>,
    pub postcode: PostcodeFormat,
    pub street_types: Vec<StreetType>,
    /// Code of the type that streets of any type are also written with ("CL", calle, in Spain):
    /// compatible with every other type.
    pub any_type: Option<String>,
    /// Type pairs that sources use for one another.
    pub interchanged_types: Vec<(String, String)>,
    pub words: Words,
    /// Word pairs that are the same in two languages of the country (`data/equivalences.txt`).
    pub equivalences: Vec<(String, String)>,
    /// Word pairs never to take as the same, even if names of the sources suggest it.
    pub not_equivalent: Vec<(String, String)>,
    /// The OSM `admin_level` of each kind of administrative unit (`data/admin-levels.toml`).
    pub admin_levels: AdminLevels,
    /// Street type words of other languages (`languages/*.toml`) that queries may use for the
    /// types of this pack, longest first.
    pub foreign_types: Vec<ForeignType>,
}

/// A street type word of another language, as a query may write it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignType {
    pub word: String,
    /// The type of the pack it names.
    pub code: String,
    /// Written after the name in its language ("Femenies Street").
    pub after: bool,
}

/// The street types of the languages that queries in any country may be written in.
const LANGUAGES: &[(&str, &str)] = &[("languages/en.toml", include_str!("../../../languages/en.toml"))];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Language {
    street_types: Vec<LanguageType>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LanguageType {
    words: Vec<String>,
    #[serde(default)]
    after: bool,
    same_as: Vec<String>,
}

/// The words of `LANGUAGES` for the types of `types`: each language type is the first type of
/// the pack with one of its `same_as` words. Words the pack already has are its own.
fn foreign_types(types: &[StreetType]) -> Result<Vec<ForeignType>> {
    let mut out: Vec<ForeignType> = Vec::new();
    for (file, text) in LANGUAGES {
        let language: Language = parse(file, text)?;
        for t in language.street_types {
            let Some(native) = types.iter().find(|n| n.words.iter().any(|w| t.same_as.contains(w))) else { continue };
            for word in t.words {
                if !types.iter().any(|n| n.words.contains(&word)) && !out.iter().any(|f| f.word == word) {
                    out.push(ForeignType { word, code: native.code.clone(), after: t.after });
                }
            }
        }
    }
    out.sort_by_key(|f| std::cmp::Reverse(f.word.len()));
    Ok(out)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostcodeFormat {
    pub digits: usize,
    /// Valid leading digits (a province, a département…); empty accepts any.
    #[serde(default)]
    pub prefixes: Vec<String>,
    /// Country letters a postcode may be written with, uppercase ("L-" in "L-1930").
    #[serde(default)]
    pub country_prefixes: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreetType {
    /// Code common to every language of the country.
    pub code: String,
    /// Folded words and abbreviations that name the type, in every language.
    pub words: Vec<String>,
    /// Words used to generate index-time variants, one per language where it differs.
    pub display: Vec<String>,
    /// Never a street name on its own: sources write it alone when the name is missing.
    #[serde(default)]
    pub generic: bool,
    /// Names an area rather than a way (a park, an estate): a query starting with it is as
    /// likely a place as an address.
    #[serde(default)]
    pub area: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Words {
    /// Words that link a street type to its name, and articles. Folded.
    pub connectors: Vec<String>,
    /// Words that introduce a house number ("nº"). Folded.
    pub number_markers: Vec<String>,
    /// Floor, door and similar details written after the house number. Folded.
    pub unit_words: Vec<String>,
    /// How "no number" is written, lowercase.
    pub unnumbered: Vec<String>,
    /// House number suffixes that are words, uppercase ("BIS").
    pub housenumber_suffixes: Vec<String>,
    /// Suffix words also recognised by their start ("BISA" is "BIS").
    #[serde(default)]
    pub housenumber_suffix_prefixes: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    country: String,
    version: u32,
    languages: Vec<String>,
    postcode: PostcodeFormat,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StreetTypes {
    any_type: Option<String>,
    #[serde(default)]
    interchanged: Vec<[String; 2]>,
    types: Vec<StreetType>,
}

type Pairs = Vec<(String, String)>;

/// One pair of folded words per line, `a b`; `-a b` rules a pair out; `#` starts a comment.
fn parse_equivalences(text: &str) -> Result<(Pairs, Pairs)> {
    let (mut pairs, mut excluded) = (Vec::new(), Vec::new());
    let mut folded = String::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (out, line) = match line.strip_prefix('-') {
            Some(rest) => (&mut excluded, rest),
            None => (&mut pairs, line),
        };
        let words: Vec<&str> = line.split_whitespace().collect();
        let mut valid = |w: &str| {
            fold(w, &mut folded);
            folded == w
        };
        match words[..] {
            [a, b] if a != b && valid(a) && valid(b) => out.push((a.to_owned(), b.to_owned())),
            _ => return Err(GekoError::Config(format!("data/equivalences.txt:{}: expected two different folded words", n + 1))),
        }
    }
    Ok((pairs, excluded))
}

fn parse<T: serde::de::DeserializeOwned>(file: &str, text: &str) -> Result<T> {
    toml::from_str(text).map_err(|e| GekoError::Config(format!("{file}: {e}")))
}

impl Pack {
    /// Reads a pack from the text of `pack.toml`, `data/street-types.toml`, `data/words.toml`,
    /// `data/equivalences.txt` and `data/admin-levels.toml`.
    pub fn from_toml(pack: &str, street_types: &str, words: &str, equivalences: &str, admin_levels: &str) -> Result<Self> {
        let manifest: Manifest = parse("pack.toml", pack)?;
        let types: StreetTypes = parse("data/street-types.toml", street_types)?;
        let words: Words = parse("data/words.toml", words)?;
        let country = CountryCode::new(&manifest.country).map_err(|e| GekoError::Config(format!("pack.toml: {e}")))?;
        if manifest.version == 0 {
            return Err(GekoError::Config("pack.toml: version starts at 1".into()));
        }
        let mut words_seen = std::collections::HashMap::new();
        for t in &types.types {
            for w in &t.words {
                if let Some(other) = words_seen.insert(w.as_str(), t.code.as_str())
                    && other != t.code
                {
                    return Err(GekoError::Config(format!("data/street-types.toml: word `{w}` names both {other} and {}", t.code)));
                }
            }
        }
        let (equivalences, not_equivalent) = parse_equivalences(equivalences)?;
        let admin_levels: AdminLevels = parse("data/admin-levels.toml", admin_levels)?;
        let foreign_types = foreign_types(&types.types)?;
        Ok(Self {
            country,
            version: manifest.version,
            languages: manifest.languages,
            postcode: manifest.postcode,
            street_types: types.types,
            any_type: types.any_type,
            interchanged_types: types.interchanged.into_iter().map(|[a, b]| (a, b)).collect(),
            words,
            equivalences,
            not_equivalent,
            admin_levels,
            foreign_types,
        })
    }

    pub fn street_type(&self, code: &str) -> Option<&StreetType> {
        self.street_types.iter().find(|t| t.code == code)
    }
}

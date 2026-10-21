//! Reference queries (PROJECT.md §13.1), generated from real canonical entities with the
//! deformations people actually type.

use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result};
use arrow_array::cast::AsArray;
use arrow_array::types::{Int32Type, UInt16Type, UInt32Type};
use arrow_array::{Array, RecordBatch};
use geko_core::ports::TextNormalizer;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Case {
    pub category: String,
    pub query: String,
    #[serde(default)]
    pub autocomplete: bool,
    pub lat: f64,
    pub lon: f64,
    /// A result within this distance of the expected point counts as correct.
    pub tolerance_m: f64,
    #[serde(default)]
    pub gid: Option<String>,
    /// Position an app would send as `focus` (map centre or user location).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus_lat: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus_lon: Option<f64>,
    /// A reverse query of the point `query` ("lat,lon"), asking for these layers (empty: all).
    /// The result asked for is the one with `gid`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverse: Option<String>,
    /// Name of the municipality that the administrative units of a reverse query must hold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin: Option<String>,
}

/// SplitMix64: deterministic, so the reference set can be regenerated identically.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn batches(path: &Path) -> Result<Vec<RecordBatch>> {
    let file = File::open(path).with_context(|| path.display().to_string())?;
    Ok(ParquetRecordBatchReaderBuilder::try_new(file)?.build()?.collect::<std::result::Result<Vec<_>, _>>()?)
}

struct Row {
    gid: String,
    name: String,
    lat: f64,
    lon: f64,
    municipality: Option<String>,
    extra: Option<String>,
    number: Option<u32>,
    sources: String,
    importance: u16,
    /// Its names by tag (`{"name": …, "name:en": …}`), where the table has them.
    names: Option<String>,
}

fn read(path: &Path, name_col: &str, extra_col: Option<&str>) -> Result<Vec<Row>> {
    let mut rows = Vec::new();
    for b in batches(path)? {
        let s = |c: &str| b.column_by_name(c).with_context(|| format!("{}: no column {c}", path.display())).map(|a| a.as_string::<i32>().clone());
        let (gid, name, muni) = (s("public_id")?, s(name_col)?, s("municipality_id")?);
        let extra = extra_col.map(s).transpose()?;
        let lat = b.column_by_name("lat_e7").context("lat_e7")?.as_primitive::<Int32Type>().clone();
        let lon = b.column_by_name("lon_e7").context("lon_e7")?.as_primitive::<Int32Type>().clone();
        let number = b.column_by_name("number").map(|a| a.as_primitive::<UInt32Type>().clone());
        let sources = b.column_by_name("sources").map(|a| a.as_string::<i32>().clone());
        let importance = b.column_by_name("importance").map(|a| a.as_primitive::<UInt16Type>().clone());
        let names = b.column_by_name("names").map(|a| a.as_string::<i32>().clone());
        for i in 0..b.num_rows() {
            rows.push(Row {
                gid: gid.value(i).to_owned(),
                name: name.value(i).to_owned(),
                lat: lat.value(i) as f64 * 1e-7,
                lon: lon.value(i) as f64 * 1e-7,
                municipality: muni.is_valid(i).then(|| muni.value(i).to_owned()),
                extra: extra.as_ref().filter(|e| e.is_valid(i)).map(|e| e.value(i).to_owned()),
                number: number.as_ref().filter(|n| n.is_valid(i)).map(|n| n.value(i)),
                sources: sources.as_ref().map(|s| s.value(i).to_owned()).unwrap_or_default(),
                importance: importance.as_ref().map_or(0, |v| v.value(i)),
                names: names.as_ref().filter(|n| n.is_valid(i)).map(|n| n.value(i).to_owned()),
            });
        }
    }
    Ok(rows)
}

/// One typo in the longest word: two adjacent letters swapped.
fn typo(text: &str, rng: &mut Rng) -> Option<String> {
    let words: Vec<&str> = text.split(' ').collect();
    let (i, w) = words.iter().enumerate().max_by_key(|(_, w)| w.chars().count())?;
    let chars: Vec<char> = w.chars().collect();
    if chars.len() < 6 {
        return None;
    }
    let k = 1 + rng.below(chars.len() - 3);
    let mut swapped = chars.clone();
    swapped.swap(k, k + 1);
    if swapped == chars {
        return None;
    }
    let mut out = words.clone();
    let s: String = swapped.into_iter().collect();
    out[i] = &s;
    Some(out.join(" "))
}

/// The text rules of the country of a canonical dataset (`canonical/<country>`).
fn normalizer(canonical: &Path) -> Result<text_rules::RulesNormalizer> {
    let country = canonical.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_uppercase();
    let Ok(code) = geko_core::country::CountryCode::new(&country) else {
        anyhow::bail!("{}: `{country}` is not a country code", canonical.display());
    };
    Ok(geko_country_pack::repo(code.as_str()).normalizer())
}

/// The street name with its type in another language of the country, the first one its pack
/// lists that the name does not start with: "Carrer de Mallorca" → "calle mallorca".
fn type_variant(normalizer: &text_rules::RulesNormalizer, name: &str) -> Option<String> {
    let parts = normalizer.street_parts(name);
    let mut folded = String::new();
    text_rules::fold(name, &mut folded);
    let word = normalizer.street_type(parts.type_code?)?.display.iter().find(|w| !folded.starts_with(w.as_str()))?;
    Some(format!("{word} {}", parts.core))
}

/// What a person types to autocomplete each municipality of `rows` (by `gid`): from half its name
/// (at least 4 letters) on, the shortest start of its name whose most important municipality is
/// it. Without focus, an app rightly shows first the most important municipalities starting with
/// what was typed: "Villa" is not any of the hundreds of "Villa…". None when a homonym is more
/// important even with the whole name.
fn autocomplete_prefixes(rows: &[Row]) -> std::collections::HashMap<String, String> {
    let fold = |s: &str| {
        let mut out = String::new();
        text_rules::fold(s, &mut out);
        out
    };
    let mut munis: Vec<(String, u16, usize)> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.extra.as_deref() == Some("municipality"))
        .map(|(i, r)| (fold(&r.name), r.importance, i))
        .collect();
    munis.sort();
    let mut out = std::collections::HashMap::new();
    for (folded, importance, i) in &munis {
        let name = &rows[*i].name;
        let chars: Vec<char> = name.chars().collect();
        if !chars.first().is_some_and(|c| c.is_alphabetic()) {
            continue;
        }
        for len in chars.len().div_ceil(2).max(4).min(chars.len())..=chars.len() {
            let typed: String = chars[..len].iter().collect();
            let key = fold(&typed);
            if !folded.starts_with(key.as_str()) {
                break;
            }
            let start = munis.partition_point(|(f, _, _)| f.as_str() < key.as_str());
            let first = munis[start..].iter().take_while(|(f, _, _)| f.starts_with(key.as_str())).all(|(f, imp, j)| j == i || imp < importance || (imp == importance && f > folded));
            if first {
                out.insert(rows[*i].gid.clone(), typed);
                break;
            }
        }
    }
    out
}

/// The `address_no_municipality` cases again, with the focus an app would send: the centre of
/// the expected municipality, as with the map centred on the town. A street name without
/// municipality is ambiguous across homonyms (BACKLOG 12); with focus it should not be. Same
/// queries, so the two categories compare.
pub fn with_focus(cases: &[Case], canonical: &Path) -> Result<Vec<Case>> {
    let places = read(&canonical.join("places.parquet"), "name", Some("kind"))?;
    let addresses = read(&canonical.join("addresses.parquet"), "street_name", None)?;
    let centre: std::collections::HashMap<&str, (f64, f64)> =
        places.iter().filter(|p| p.extra.as_deref() == Some("municipality")).map(|p| (p.gid.as_str(), (p.lat, p.lon))).collect();
    let municipality: std::collections::HashMap<&str, &str> =
        addresses.iter().filter_map(|a| Some((a.gid.as_str(), a.municipality.as_deref()?))).collect();
    Ok(cases
        .iter()
        .filter(|c| c.category == "address_no_municipality")
        .filter_map(|c| {
            let (lat, lon) = *centre.get(municipality.get(c.gid.as_deref()?)?)?;
            Some(Case { category: "address_focus".into(), focus_lat: Some(lat), focus_lon: Some(lon), ..c.clone() })
        })
        .collect())
}

/// What an app sends while the queries of `cases` are typed: one autocomplete request per
/// keystroke from the third character, up to the whole query. For load tests of the
/// autocomplete route; the expected result is the query's, so short prefixes are not judged.
pub fn keystrokes(cases: &[Case]) -> Vec<Case> {
    let mut out = Vec::new();
    for c in cases.iter().filter(|c| !c.autocomplete) {
        let chars: Vec<char> = c.query.chars().collect();
        for n in 3..=chars.len() {
            let typed: String = chars[..n].iter().collect();
            if typed.ends_with(' ') && n < chars.len() {
                continue;
            }
            out.push(Case { category: format!("keystroke_{}", c.category), query: typed, autocomplete: true, ..c.clone() });
        }
    }
    out
}

/// Up to `n` distinct rows that pass `ok`, drawn at random.
fn sample(rows: &[Row], n: usize, rng: &mut Rng, ok: &dyn Fn(&Row) -> bool) -> Vec<usize> {
    let mut picked = Vec::new();
    for _ in 0..n * 50 {
        if picked.len() >= n || rows.is_empty() {
            break;
        }
        let i = rng.below(rows.len());
        if ok(&rows[i]) && !picked.contains(&i) {
            picked.push(i);
        }
    }
    picked
}

fn case(category: &str, query: String, r: &Row, tolerance_m: f64) -> Case {
    Case {
        category: category.into(),
        query,
        autocomplete: false,
        lat: r.lat,
        lon: r.lon,
        tolerance_m,
        gid: Some(r.gid.clone()),
        focus_lat: None,
        focus_lon: None,
        reverse: None,
        admin: None,
    }
}

/// The English words of the street types of a pack (`golden/english.toml`), for the English
/// reference queries. Types without one are left out of them.
#[derive(Debug, Default, Deserialize)]
pub struct EnglishWords {
    pub street_types: std::collections::BTreeMap<String, String>,
}

/// The English type word and the core of a street name, if its type has one: "Carrer de
/// Femenies" → ("Street", "femenies").
fn english_street(rules: &text_rules::RulesNormalizer, words: &EnglishWords, name: &str) -> Option<(String, String)> {
    let parts = rules.street_parts(name);
    let word = words.street_types.get(parts.type_code?)?;
    (!parts.core.is_empty()).then(|| (word.clone(), parts.core))
}

/// The English name of an entity (`name:en`), if it is not written as any of its other names.
fn english_name(names: Option<&str>) -> Option<String> {
    let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(names?).ok()?;
    let en = map.get("name:en")?.as_str()?;
    let fold = |s: &str| {
        let mut out = String::new();
        text_rules::fold(s, &mut out);
        out
    };
    let folded = fold(en);
    let elsewhere = map.iter().any(|(k, v)| k != "name:en" && v.as_str().is_some_and(|v| fold(v) == folded));
    (!elsewhere && !folded.is_empty()).then(|| en.to_owned())
}

/// Whether a name holds one of the English street words as a word of its own ("Main Street").
fn has_english_word(words: &EnglishWords, name: &str) -> bool {
    let mut folded = String::new();
    text_rules::fold(name, &mut folded);
    let english: Vec<String> = words.street_types.values().map(|w| w.to_lowercase()).collect();
    folded.split(' ').any(|t| english.iter().any(|w| w == t))
}

/// Reference queries in English (PROJECT.md §13.1), as a visitor types them: street types in
/// English before or after the name, and the English names of places. Last, the entities whose
/// own name holds an English street word, which reading those words as a type must not lose.
pub fn english(canonical: &Path, words: &EnglishWords, per_category: usize, seed: u64) -> Result<Vec<Case>> {
    let mut rng = Rng::new(seed);
    let rules = normalizer(canonical)?;
    let places = read(&canonical.join("places.parquet"), "name", Some("kind"))?;
    let muni_name = |gid: &Option<String>| -> String {
        gid.as_ref().and_then(|g| places.iter().find(|p| &p.gid == g)).map(|p| p.name.clone()).unwrap_or_default()
    };
    let addresses = read(&canonical.join("addresses.parquet"), "street_name", None)?;
    let streets = read(&canonical.join("streets.parquet"), "name", None)?;
    let pois = read(&canonical.join("pois.parquet"), "name", None)?;
    let typed_by_people = |r: &Row| !r.name.starts_with("Diseminado");
    let english_typed = |r: &Row| typed_by_people(r) && r.municipality.is_some() && english_street(&rules, words, &r.name).is_some();
    let numbered = |r: &Row| r.number.is_some() && english_typed(r);

    let mut out = Vec::new();
    for i in sample(&addresses, per_category, &mut rng, &numbered) {
        let a = &addresses[i];
        let (word, core) = english_street(&rules, words, &a.name).unwrap();
        out.push(case("en_address", format!("{} {core} {word}, {}", a.number.unwrap(), muni_name(&a.municipality)), a, 50.0));
    }
    for i in sample(&addresses, per_category, &mut rng, &numbered) {
        let a = &addresses[i];
        let (word, core) = english_street(&rules, words, &a.name).unwrap();
        out.push(case("en_type_first", format!("{word} {core} {}, {}", a.number.unwrap(), muni_name(&a.municipality)), a, 50.0));
    }
    for i in sample(&addresses, per_category, &mut rng, &numbered) {
        let a = &addresses[i];
        let (word, core) = english_street(&rules, words, &a.name).unwrap();
        if let Some(t) = typo(&core, &mut rng) {
            out.push(case("en_typo", format!("{word} {t} {}, {}", a.number.unwrap(), muni_name(&a.municipality)), a, 50.0));
        }
    }
    for i in sample(&streets, per_category, &mut rng, &english_typed) {
        let s = &streets[i];
        let (word, core) = english_street(&rules, words, &s.name).unwrap();
        out.push(case("en_street", format!("{core} {word}, {}", muni_name(&s.municipality)), s, 500.0));
    }
    for i in sample(&streets, per_category, &mut rng, &english_typed) {
        let s = &streets[i];
        let (word, core) = english_street(&rules, words, &s.name).unwrap();
        out.push(case("en_street_type_first", format!("{word} {core}, {}", muni_name(&s.municipality)), s, 500.0));
    }
    let named = |r: &Row| english_name(r.names.as_deref()).is_some();
    for i in sample(&places, per_category, &mut rng, &named) {
        let p = &places[i];
        out.push(case("en_name_place", english_name(p.names.as_deref()).unwrap(), p, 3000.0));
    }
    for i in sample(&pois, per_category, &mut rng, &|r| named(r) && r.municipality.is_some()) {
        let p = &pois[i];
        out.push(case("en_name_poi", format!("{}, {}", english_name(p.names.as_deref()).unwrap(), muni_name(&p.municipality)), p, 150.0));
    }
    for i in sample(&streets, per_category, &mut rng, &|r| named(r) && r.municipality.is_some()) {
        let s = &streets[i];
        out.push(case("en_name_street", format!("{}, {}", english_name(s.names.as_deref()).unwrap(), muni_name(&s.municipality)), s, 500.0));
    }
    let literal = |r: &Row| r.municipality.is_some() && has_english_word(words, &r.name);
    for i in sample(&pois, per_category, &mut rng, &literal) {
        let p = &pois[i];
        out.push(case("en_literal_poi", format!("{}, {}", p.name, muni_name(&p.municipality)), p, 150.0));
    }
    for i in sample(&streets, per_category, &mut rng, &literal) {
        let s = &streets[i];
        out.push(case("en_literal_street", format!("{}, {}", s.name, muni_name(&s.municipality)), s, 500.0));
    }
    Ok(out)
}

/// With `only_source`, only entities that source also has: a fair engine comparison against an
/// engine fed with that source alone.
pub fn generate(canonical: &Path, per_category: usize, seed: u64, only_source: Option<&str>) -> Result<Vec<Case>> {
    let in_source = |r: &Row| only_source.is_none_or(|s| r.sources.split(',').any(|x| x == s));
    // Rural "Diseminados" addresses are real but nobody types them.
    let typed_by_people = |r: &Row| !r.name.starts_with("Diseminado");
    let mut rng = Rng::new(seed);
    let rules = normalizer(canonical)?;
    let places = read(&canonical.join("places.parquet"), "name", Some("kind"))?;
    let muni_name = |gid: &Option<String>| -> Option<String> {
        let gid = gid.as_ref()?;
        places.iter().find(|p| &p.gid == gid).map(|p| p.name.clone())
    };
    let addresses = read(&canonical.join("addresses.parquet"), "street_name", Some("postcode"))?;
    let streets = read(&canonical.join("streets.parquet"), "name", None)?;
    let pois = read(&canonical.join("pois.parquet"), "name", Some("wikidata"))?;
    let postcodes = read(&canonical.join("postcodes.parquet"), "postcode", None)?;

    let mut out = Vec::new();
    let mut push = |category: &str, query: String, r: &Row, tolerance_m: f64, autocomplete: bool| {
        out.push(Case {
            category: category.into(),
            query,
            autocomplete,
            lat: r.lat,
            lon: r.lon,
            tolerance_m,
            gid: Some(r.gid.clone()),
            focus_lat: None,
            focus_lon: None,
            reverse: None,
            admin: None,
        });
    };
    let numbered = |r: &Row| r.number.is_some() && r.municipality.is_some() && in_source(r) && typed_by_people(r);
    for i in sample(&addresses, per_category, &mut rng, &numbered) {
        let a = &addresses[i];
        let muni = muni_name(&a.municipality).unwrap_or_default();
        push("address_full", format!("{} {}, {muni}", a.name, a.number.unwrap()), a, 50.0, false);
    }
    for i in sample(&addresses, per_category, &mut rng, &numbered) {
        let a = &addresses[i];
        push("address_no_municipality", format!("{} {}", a.name, a.number.unwrap()), a, 50.0, false);
    }
    for i in sample(&addresses, per_category, &mut rng, &|r| numbered(r) && r.extra.is_some()) {
        let a = &addresses[i];
        push("address_postcode", format!("{} {}, {}", a.name, a.number.unwrap(), a.extra.as_deref().unwrap()), a, 50.0, false);
    }
    for i in sample(&addresses, per_category, &mut rng, &|r| numbered(r) && type_variant(&rules, &r.name).is_some()) {
        let a = &addresses[i];
        let muni = muni_name(&a.municipality).unwrap_or_default();
        push("address_type_variant", format!("{} {} {muni}", type_variant(&rules, &a.name).unwrap(), a.number.unwrap()), a, 50.0, false);
    }
    for i in sample(&addresses, per_category, &mut rng, &numbered) {
        let a = &addresses[i];
        let muni = muni_name(&a.municipality).unwrap_or_default();
        if let Some(t) = typo(&a.name, &mut rng) {
            push("address_typo", format!("{t} {}, {muni}", a.number.unwrap()), a, 50.0, false);
        }
    }
    for i in sample(&streets, per_category, &mut rng, &|r| r.municipality.is_some() && in_source(r) && typed_by_people(r)) {
        let s = &streets[i];
        push("street", format!("{}, {}", s.name, muni_name(&s.municipality).unwrap_or_default()), s, 500.0, false);
    }
    for i in sample(&places, per_category, &mut rng, &|r| r.extra.as_deref() == Some("municipality")) {
        let p = &places[i];
        push("municipality", p.name.clone(), p, 3000.0, false);
    }
    for i in sample(&pois, per_category, &mut rng, &|r| r.extra.is_some() && r.municipality.is_some() && in_source(r)) {
        let p = &pois[i];
        push("poi", format!("{}, {}", p.name, muni_name(&p.municipality).unwrap_or_default()), p, 150.0, false);
    }
    for i in sample(&postcodes, if only_source.is_some() { 0 } else { per_category }, &mut rng, &|_| true) {
        let p = &postcodes[i];
        push("postcode", p.name.clone(), p, 1500.0, false);
    }
    let prefixes = autocomplete_prefixes(&places);
    for i in sample(&places, per_category, &mut rng, &|r| r.name.chars().count() >= 6 && prefixes.contains_key(&r.gid)) {
        let p = &places[i];
        push("autocomplete_municipality", prefixes[&p.gid].clone(), p, 3000.0, true);
    }
    // Last, so the samples of the other categories stay those of earlier sets.
    for i in sample(&addresses, per_category, &mut rng, &numbered) {
        let a = &addresses[i];
        let muni = muni_name(&a.municipality).unwrap_or_default();
        push("address_number_first", format!("{} {}, {muni}", a.number.unwrap(), a.name), a, 50.0, false);
    }
    Ok(out)
}

/// Reverse queries (PROJECT.md §13.1) from a point two metres from a real address: the address
/// itself among the addresses, its street among the streets, and its municipality among the
/// administrative units.
pub fn reverse(canonical: &Path, per_category: usize, seed: u64) -> Result<Vec<Case>> {
    const OFFSET_M: f64 = 2.0;
    let mut rng = Rng::new(seed);
    let places = read(&canonical.join("places.parquet"), "name", Some("kind"))?;
    let municipalities: std::collections::HashMap<&str, &str> =
        places.iter().filter(|p| p.extra.as_deref() == Some("municipality")).map(|p| (p.gid.as_str(), p.name.as_str())).collect();
    let addresses = read(&canonical.join("addresses.parquet"), "street_name", Some("street_id"))?;
    let usable: Vec<&Row> = addresses
        .iter()
        .filter(|a| a.number.is_some() && a.extra.is_some() && a.municipality.as_deref().is_some_and(|m| municipalities.contains_key(m)))
        .collect();
    let mut out = Vec::new();
    if usable.is_empty() {
        return Ok(out);
    }
    for category in ["reverse_address", "reverse_street", "reverse_municipality"] {
        for _ in 0..per_category {
            let a = usable[rng.below(usable.len())];
            let point = format!("{:.7},{:.7}", a.lat + OFFSET_M / 111_320.0, a.lon);
            let (layers, gid, admin) = match category {
                "reverse_address" => ("address", Some(a.gid.clone()), None),
                "reverse_street" => ("street", a.extra.clone(), None),
                _ => ("address", None, a.municipality.as_deref().map(|m| municipalities[m].to_owned())),
            };
            out.push(Case {
                category: category.into(),
                query: point,
                autocomplete: false,
                lat: a.lat,
                lon: a.lon,
                tolerance_m: 10.0,
                gid,
                focus_lat: None,
                focus_lon: None,
                reverse: Some(layers.into()),
                admin,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autocomplete_prefixes_grow_until_the_municipality_comes_first() {
        let muni = |gid: &str, name: &str, importance: u16| Row {
            gid: gid.into(),
            name: name.into(),
            lat: 0.0,
            lon: 0.0,
            municipality: None,
            extra: Some("municipality".into()),
            number: None,
            sources: String::new(),
            importance,
            names: None,
        };
        let rows = vec![
            muni("a", "Villanueva de la Serena", 30_000),
            muni("b", "Villanueva del Fresno", 20_000),
            muni("c", "Villafranca", 25_000),
            muni("d", "Ávila", 40_000),
            muni("e", "Ávila", 10_000),
            muni("f", "63 Fake", 10_000),
        ];
        let p = autocomplete_prefixes(&rows);
        assert_eq!(p["a"], "Villanueva d");
        assert_eq!(p["b"], "Villanueva del");
        assert_eq!(p["c"], "Villaf");
        assert_eq!(p["d"], "Ávil");
        assert!(!p.contains_key("e"), "a more important homonym comes first");
        assert!(!p.contains_key("f"));
    }

    #[test]
    fn deterministic_rng() {
        let (mut a, mut b) = (Rng::new(7), Rng::new(7));
        assert_eq!((0..5).map(|_| a.next()).collect::<Vec<_>>(), (0..5).map(|_| b.next()).collect::<Vec<_>>());
    }

    #[test]
    fn street_type_in_another_language() {
        let es = country_pack_es::normalizer();
        assert_eq!(type_variant(&es, "Carrer de Mallorca").as_deref(), Some("calle mallorca"));
        assert_eq!(type_variant(&es, "Avinguda de Jaume III").as_deref(), Some("avenida jaume iii"));
        assert_ne!(type_variant(&es, "Calle Mayor").as_deref(), Some("calle mayor"), "another language than the name's");
        assert_eq!(type_variant(&es, "Mallorca"), None, "no street type");
        let lu = country_pack_lu::normalizer();
        assert_eq!(type_variant(&lu, "Rue de la Gare").as_deref(), Some("strooss gare"));
    }

    #[test]
    fn english_street_types_and_names() {
        let es = country_pack_es::normalizer();
        let words = EnglishWords { street_types: [("CL".to_owned(), "Street".to_owned())].into() };
        assert_eq!(english_street(&es, &words, "Carrer de Femenies"), Some(("Street".into(), "femenies".into())));
        assert_eq!(english_street(&es, &words, "Plaça Major"), None, "a type without an English word");
        assert_eq!(english_street(&es, &words, "Calle"), None, "no name");
        assert!(has_english_word(&words, "Main Street Madrid"));
        assert!(!has_english_word(&words, "Streetfood"));
        assert_eq!(english_name(Some(r#"{"name":"La Seu","name:en":"Palma Cathedral"}"#)).as_deref(), Some("Palma Cathedral"));
        assert_eq!(english_name(Some(r#"{"name":"Palma","name:en":"Palma"}"#)), None, "the same as another name");
        assert_eq!(english_name(Some(r#"{"name":"Sóller"}"#)), None);
    }

    #[test]
    fn typo_swaps_letters_in_the_longest_word() {
        let mut rng = Rng::new(1);
        let t = typo("Carrer de Mallorca", &mut rng).unwrap();
        assert_ne!(t, "Carrer de Mallorca");
        assert_eq!(t.len(), "Carrer de Mallorca".len());
        assert!(typo("Sa Pobla", &mut rng).is_none());
    }
}

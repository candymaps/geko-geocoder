//! Builds a country segment from the canonical tables (PROJECT.md §9.4, §9.6–9.8).

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;

use fst::MapBuilder;
use geko_core::model::PlaceKind;
use geko_core::ports::{CanonicalPartition, GeocodeIndexBuilder, IndexFormat, IndexManifest, TextNormalizer};
use geko_core::{GekoError, Result};
use rayon::prelude::*;
use serde_json::{Value, json};

use crate::canonical::{self, Canonical, DocIn};
use crate::equivalences;
use crate::format::{self, AddressMeta, Manifest, NO_MUNICIPALITY, PREFIX_LEN, PREFIX_TOP_K, fragment};
use crate::store::{BlobWriter, Blobs, write_array, write_bytes};

pub struct FstIndexBuilder;

#[derive(Default)]
struct Postings {
    data: Vec<u32>,
    start: Vec<u32>,
    len: Vec<u32>,
}

impl Postings {
    fn add(&mut self, ids: &[u32]) -> u64 {
        self.start.push(self.data.len() as u32);
        self.len.push(ids.len() as u32);
        self.data.extend_from_slice(ids);
        (self.start.len() - 1) as u64
    }
}

fn fst_err(e: fst::Error) -> GekoError {
    GekoError::Other(Box::new(e))
}

fn write_fst<'a>(path: &Path, entries: impl Iterator<Item = (&'a str, u64)>) -> Result<()> {
    let mut builder = MapBuilder::memory();
    for (k, v) in entries {
        builder.insert(k, v).map_err(fst_err)?;
    }
    write_bytes(path, &builder.into_inner().map_err(fst_err)?)
}

/// Keeps the `k` smallest ids of the union of `into` and `from`, both sorted.
fn merge_smallest(into: &mut Vec<u32>, from: &[u32], k: usize) {
    let mut merged = Vec::with_capacity(k);
    let (mut a, mut b) = (0, 0);
    while merged.len() < k && (a < into.len() || b < from.len()) {
        let next = match (into.get(a), from.get(b)) {
            (Some(x), Some(y)) if x == y => {
                a += 1;
                b += 1;
                *x
            }
            (Some(x), Some(y)) if x < y => {
                a += 1;
                *x
            }
            (_, Some(y)) => {
                b += 1;
                *y
            }
            (Some(x), None) => {
                a += 1;
                *x
            }
            (None, None) => break,
        };
        merged.push(next);
    }
    *into = merged;
}

/// Distinct strings, each stored once as a blob and referred to by index.
#[derive(Default)]
struct Interner {
    index: HashMap<String, u32>,
    blobs: Blobs,
}

impl Interner {
    fn get(&mut self, s: &str) -> u32 {
        if let Some(&i) = self.index.get(s) {
            return i;
        }
        let i = self.index.len() as u32;
        self.blobs.push(s.as_bytes());
        self.index.insert(s.to_owned(), i);
        i
    }
}

/// Distinct values listed in the manifest, referred to as 1 + their index; 0 is none.
#[derive(Default)]
struct CodeTable {
    index: HashMap<String, u16>,
    values: Vec<String>,
}

impl CodeTable {
    fn code(&mut self, v: Option<&str>) -> Result<u16> {
        let Some(v) = v else { return Ok(0) };
        if let Some(&c) = self.index.get(v) {
            return Ok(c);
        }
        let c = u16::try_from(self.values.len() + 1).map_err(|_| GekoError::Data(format!("more than 65535 distinct values like {v}")))?;
        self.values.push(v.to_owned());
        self.index.insert(v.to_owned(), c);
        Ok(c)
    }
}

fn tokens_of(text: &str, normalizer: &dyn TextNormalizer, buf: &mut String, out: &mut Vec<String>) {
    normalizer.normalize(text, buf);
    normalizer.tokenize(buf, out);
}

/// Entities analysed or serialized in parallel per chunk: big enough to share the work, small
/// enough that the results of a chunk stay a few megabytes.
const CHUNK: usize = 65_536;

/// The searchable text of an entity: its significant word count and the tokens of each name variant.
struct Analysed {
    ntok: u8,
    /// Significant words of its shortest name: "Huesca/Uesca" is also "Huesca".
    ltok: u8,
    variants: Vec<(String, Vec<String>)>,
}

fn analyse(d: &DocIn, normalizer: &dyn TextNormalizer) -> Analysed {
    let (mut buf, mut toks, mut variants) = (String::new(), Vec::new(), Vec::new());
    // Significant words only: "la Sagrada Família" has two, like the query "sagrada familia".
    let core = normalizer.street_parts(&d.name).core;
    tokens_of(&core, normalizer, &mut buf, &mut toks);
    let ntok = toks.len().clamp(1, u8::MAX as usize) as u8;
    let mut ltok = ntok;
    let mut out = Vec::new();
    for name in d.search_names() {
        tokens_of(&normalizer.street_parts(name).core, normalizer, &mut buf, &mut toks);
        if !toks.is_empty() {
            ltok = ltok.min(toks.len().min(u8::MAX as usize) as u8);
        }
        normalizer.variants(name, &mut variants);
        for v in variants.drain(..) {
            let mut tokens = Vec::new();
            normalizer.tokenize(&v, &mut tokens);
            out.push((v, tokens));
        }
    }
    Analysed { ntok, ltok, variants: out }
}

/// Posting lists in key order, as the FSTs need them.
fn sorted(map: HashMap<String, Vec<u32>>) -> Vec<(String, Vec<u32>)> {
    let mut entries: Vec<(String, Vec<u32>)> = map.into_iter().collect();
    entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    entries
}

/// Adds `id` to the posting list of `key`, once: ids arrive in ascending order.
fn push_id(map: &mut HashMap<String, Vec<u32>>, key: String, id: u32) {
    let list = map.entry(key).or_default();
    if list.last() != Some(&id) {
        list.push(id);
    }
}

impl GeocodeIndexBuilder for FstIndexBuilder {
    fn format(&self) -> IndexFormat {
        format::format()
    }

    fn build(&self, data: &CanonicalPartition, normalizer: &dyn TextNormalizer, dir: &Path) -> Result<IndexManifest> {
    let canonical = canonical::read(&data.dir)?;
        build_segment(&canonical, data, normalizer, dir)
    }
}

fn build_segment(c: &Canonical, data: &CanonicalPartition, normalizer: &dyn TextNormalizer, dir: &Path) -> Result<IndexManifest> {
    let docs = c.docs.len();
    if c.docs.iter().enumerate().any(|(i, d)| d.local as usize != i) {
        return Err(GekoError::Data("canonical local ids are not dense: docs first, then addresses".into()));
    }
    let hot = dir.join("hot");
    let cold = dir.join("cold");
    fs::create_dir_all(&hot)?;
    fs::create_dir_all(&cold)?;

    let by_pid: HashMap<&str, usize> = c.docs.iter().enumerate().map(|(i, d)| (d.public_id.as_str(), i)).collect();
    let name_of = |pid: &Option<String>| pid.as_deref().and_then(|p| by_pid.get(p)).map(|&i| c.docs[i].name.as_str());
    let muni_local = |pid: &Option<String>| pid.as_deref().and_then(|p| by_pid.get(p)).map_or(NO_MUNICIPALITY, |&i| i as u32);

    // Words that are the same in the languages of the country, read by the engine.
    let equivalents = equivalences::learn(&c.multilingual, normalizer);
    write_bytes(&cold.join(equivalences::FILE), equivalences::listing(&equivalents).as_bytes())?;

    // Text index: words, full names and short prefixes, each mapping to a posting list. Names are
    // analysed in parallel, a chunk of entities at a time, and merged in id order: posting lists
    // stay sorted and the index is the same as built one entity after another. Keys are gathered in
    // hash maps and sorted once: inserting millions of them into ordered maps took most of the time.
    let mut words: HashMap<String, Vec<u32>> = HashMap::new();
    let mut names: HashMap<String, Vec<u32>> = HashMap::new();
    let (mut ntok, mut ltok) = (vec![0u8; docs], vec![0u8; docs]);
    let mut id = 0u32;
    for chunk in c.docs.chunks(CHUNK) {
        let analysed: Vec<Analysed> = chunk.par_iter().map(|d| analyse(d, normalizer)).collect();
        for a in analysed {
            ntok[id as usize] = a.ntok;
            ltok[id as usize] = a.ltok;
            for (variant, tokens) in a.variants {
                for t in tokens {
                    push_id(&mut words, t, id);
                }
                push_id(&mut names, variant, id);
            }
            id += 1;
        }
    }
    let words = sorted(words);
    let names = sorted(names);
    let mut prefixes: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for (word, list) in &words {
        for len in 1..=PREFIX_LEN.min(word.len()) {
            if word.is_char_boundary(len) {
                merge_smallest(prefixes.entry(word[..len].to_owned()).or_default(), &list[..list.len().min(PREFIX_TOP_K)], PREFIX_TOP_K);
            }
        }
    }

    let mut postings = Postings::default();
    let word_entries: Vec<(&str, u64)> = words.iter().map(|(k, v)| (k.as_str(), postings.add(v))).collect();
    let name_entries: Vec<(&str, u64)> = names.iter().map(|(k, v)| (k.as_str(), postings.add(v))).collect();
    let prefix_entries: Vec<(&str, u64)> = prefixes.iter().map(|(k, v)| (k.as_str(), postings.add(v))).collect();
    write_fst(&hot.join("words.fst"), word_entries.into_iter())?;
    write_fst(&hot.join("names.fst"), name_entries.into_iter())?;
    write_fst(&hot.join("prefixes.fst"), prefix_entries.into_iter())?;
    write_array(&hot.join("postings.bin"), &postings.data)?;
    write_array(&hot.join("posting_start.bin"), &postings.start)?;
    write_array(&hot.join("posting_len.bin"), &postings.len)?;

    // Per-entity columns (struct of arrays).
    write_array(&hot.join("kind.bin"), &c.docs.iter().map(|d| format::kind_code(d.kind)).collect::<Vec<u8>>())?;
    write_array(&hot.join("importance.bin"), &c.docs.iter().map(|d| d.importance).collect::<Vec<u16>>())?;
    write_array(&hot.join("lat.bin"), &c.docs.iter().map(|d| d.lat).collect::<Vec<i32>>())?;
    write_array(&hot.join("lon.bin"), &c.docs.iter().map(|d| d.lon).collect::<Vec<i32>>())?;
    write_array(&hot.join("muni.bin"), &c.docs.iter().map(|d| muni_local(&d.municipality)).collect::<Vec<u32>>())?;
    write_array(&hot.join("ntok.bin"), &ntok)?;
    write_array(&hot.join("ltok.bin"), &ltok)?;
    let mut street_types: Vec<String> = Vec::new();
    let stype: Vec<u16> = c
        .docs
        .iter()
        .map(|d| match &d.street_type {
            None => 0,
            Some(t) => match street_types.iter().position(|x| x == t) {
                Some(p) => p as u16 + 1,
                None => {
                    street_types.push(t.clone());
                    street_types.len() as u16
                }
            },
        })
        .collect();
    write_array(&hot.join("stype.bin"), &stype)?;

    // Addresses, read row by row into compact columns: a country has tens of millions. Their
    // properties are rebuilt from their street and small tables at query time (see `format`).
    struct Addr {
        local: u32,
        street: u32,
        number: u32,
        suffix: u16,
        postcode: u16,
        lat: i32,
        lon: i32,
        tail: u32,
        label: u32,
        meta: u16,
    }
    let (mut suffixes, mut postcodes) = (CodeTable::default(), CodeTable::default());
    let (mut tails, mut labels) = (Interner::default(), Interner::default());
    let mut metas: Vec<AddressMeta> = Vec::new();
    let mut meta_index: HashMap<String, u16> = HashMap::new();
    let mut key = String::new();
    let mut addrs: Vec<Addr> = Vec::new();
    canonical::for_each_address(&data.dir, |a| {
        let street = *by_pid.get(a.street).ok_or_else(|| GekoError::Data(format!("address {} has unknown street {}", a.public_id, a.street)))?;
        let tail = a.public_id.rsplit_once(':').map_or("", |(_, t)| t);
        if !format::is_address_id(a.public_id, a.street, tail) || a.street_name != c.docs[street].name {
            return Err(GekoError::Data(format!("address {} does not derive from its street {}", a.public_id, a.street)));
        }
        key.clear();
        for part in [a.municipality, a.postcode_source, a.sources] {
            key.push_str(part.unwrap_or("\0"));
            key.push('\u{1f}');
        }
        let meta = match meta_index.get(key.as_str()) {
            Some(&m) => m,
            None => {
                let m = u16::try_from(metas.len()).map_err(|_| GekoError::Data("more than 65536 address metadata combinations".into()))?;
                metas.push(AddressMeta {
                    municipality_id: a.municipality.map(str::to_owned),
                    municipality: a.municipality.and_then(|p| by_pid.get(p)).map(|&i| c.docs[i].name.clone()),
                    props: canonical::address_props(a.postcode_source, a.sources),
                });
                meta_index.insert(key.clone(), m);
                m
            }
        };
        addrs.push(Addr {
            local: a.local,
            street: street as u32,
            number: a.number.unwrap_or(0),
            suffix: suffixes.code(a.suffix)?,
            postcode: postcodes.code(a.postcode)?,
            lat: a.lat,
            lon: a.lon,
            tail: tails.get(tail),
            label: labels.get(a.label),
            meta,
        });
        Ok(())
    })?;
    addrs.sort_unstable_by_key(|a| a.local);
    if addrs.iter().enumerate().any(|(i, a)| a.local as usize != docs + i) {
        return Err(GekoError::Data("canonical local ids are not dense: docs first, then addresses".into()));
    }

    // Address blocks: addresses are sorted by street, then number.
    let mut addr_start = vec![0u32; docs];
    let mut addr_len = vec![0u32; docs];
    for (i, a) in addrs.iter().enumerate() {
        if addr_len[a.street as usize] == 0 {
            addr_start[a.street as usize] = i as u32;
        }
        addr_len[a.street as usize] += 1;
    }
    write_array(&hot.join("addr_start.bin"), &addr_start)?;
    write_array(&hot.join("addr_len.bin"), &addr_len)?;
    let column = |f: fn(&Addr) -> u32| addrs.iter().map(f).collect::<Vec<u32>>();
    let column16 = |f: fn(&Addr) -> u16| addrs.iter().map(f).collect::<Vec<u16>>();
    let column_i32 = |f: fn(&Addr) -> i32| addrs.iter().map(f).collect::<Vec<i32>>();
    write_array(&hot.join("a_number.bin"), &column(|a| a.number))?;
    write_array(&hot.join("a_suffix.bin"), &column16(|a| a.suffix))?;
    write_array(&hot.join("a_lat.bin"), &column_i32(|a| a.lat))?;
    write_array(&hot.join("a_lon.bin"), &column_i32(|a| a.lon))?;
    write_array(&hot.join("a_postcode.bin"), &column16(|a| a.postcode))?;
    write_array(&hot.join("a_street.bin"), &column(|a| a.street))?;
    write_array(&cold.join("a_tail.bin"), &column(|a| a.tail))?;
    write_array(&cold.join("a_label.bin"), &column(|a| a.label))?;
    write_array(&cold.join("a_meta.bin"), &column16(|a| a.meta))?;
    tails.blobs.write(&cold, "atail")?;
    labels.blobs.write(&cold, "alabel")?;
    let addresses = addrs.len();
    drop(addrs);

    // Cold part: pre-serialized properties, public ids and display names of searchable entities.
    let (mut json_blobs, mut pid_blobs, mut name_blobs) = (BlobWriter::create(&cold, "json")?, BlobWriter::create(&cold, "pid")?, BlobWriter::create(&cold, "name")?);
    let properties = |d: &DocIn| {
        let mut props = d.props();
        let municipality = name_of(&d.municipality);
        let label = match d.kind {
            PlaceKind::Municipality => match name_of(&d.province) {
                Some(p) if p != d.name => format!("{}, {p}", d.name),
                _ => d.name.clone(),
            },
            PlaceKind::Postcode => format!("{} {}", d.name, municipality.unwrap_or_default()).trim().to_owned(),
            PlaceKind::Street => {
                let locality = props.get("locality").and_then(Value::as_str).filter(|l| Some(*l) != municipality && *l != d.name);
                [Some(d.name.as_str()), locality, municipality.filter(|m| *m != d.name)].into_iter().flatten().collect::<Vec<_>>().join(", ")
            }
            _ => match municipality {
                Some(m) if m != d.name => format!("{}, {m}", d.name),
                _ => d.name.clone(),
            },
        };
        props.insert("gid".into(), json!(d.public_id));
        props.insert("layer".into(), json!(d.kind.as_str()));
        props.insert("name".into(), json!(d.name));
        props.insert("label".into(), json!(label));
        props.insert("municipality".into(), json!(municipality));
        props.insert("municipality_id".into(), json!(d.municipality));
        fragment(props)
    };
    for chunk in c.docs.chunks(CHUNK) {
        let fragments: Vec<Vec<u8>> = chunk.par_iter().map(properties).collect();
        for (d, json) in chunk.iter().zip(fragments) {
            json_blobs.push(&json)?;
            pid_blobs.push(d.public_id.as_bytes())?;
            name_blobs.push(d.name.as_bytes())?;
        }
    }
    json_blobs.finish()?;
    pid_blobs.finish()?;
    name_blobs.finish()?;

    // Public ids of searchable entities; an address id is resolved through its street's.
    let mut ids: Vec<(&str, u64)> = c.docs.iter().map(|d| (d.public_id.as_str(), d.local as u64)).collect();
    ids.sort_unstable();
    write_fst(&cold.join("ids.fst"), ids.into_iter())?;

    let manifest = Manifest {
        format: format::format().into(),
        country: data.country.to_string(),
        docs: docs as u32,
        addresses: addresses as u32,
        words: words.len() as u32,
        suffixes: suffixes.values,
        postcodes: postcodes.values,
        street_types,
        address_meta: metas,
    };
    manifest.write(dir)?;
    Ok(IndexManifest { format: format::format(), country: data.country, entities: (docs + addresses) as u64 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_keeps_smallest_unique() {
        let mut a = vec![1, 4, 9];
        merge_smallest(&mut a, &[2, 4, 5, 10], 4);
        assert_eq!(a, [1, 2, 4, 5]);
        let mut empty = Vec::new();
        merge_smallest(&mut empty, &[3, 7], 4);
        assert_eq!(empty, [3, 7]);
    }

    #[test]
    fn interner_stores_each_string_once() {
        let mut i = Interner::default();
        assert_eq!((i.get("30"), i.get("38a"), i.get("30")), (0, 1, 0));
        assert_eq!(i.blobs.data, b"3038a");
    }
}

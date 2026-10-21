//! Reads the canonical tables of a country (`schemas/canonical.md`).

use std::fs::File;
use std::path::Path;

use arrow_array::cast::AsArray;
use arrow_array::types::{Int32Type, Int64Type, UInt16Type, UInt32Type};
use arrow_array::{Array, RecordBatch};
use geko_core::model::PlaceKind;
use geko_core::{GekoError, Result};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde_json::{Map, Value, json};

use crate::format::fragment;

pub struct DocIn {
    pub local: u32,
    pub kind: PlaceKind,
    pub public_id: String,
    pub name: String,
    /// Every searchable name, `NAMES_SEP`-separated: one allocation per entity, not one per name.
    names: String,
    pub lat: i32,
    pub lon: i32,
    pub importance: u16,
    pub municipality: Option<String>,
    pub province: Option<String>,
    /// Street type code, for streets.
    pub street_type: Option<String>,
    /// Properties from the canonical tables, as a JSON fragment without nulls: millions of
    /// entities each holding a `Map` took several GB.
    props: Box<str>,
}

const NAMES_SEP: char = '\0';

impl DocIn {
    pub fn search_names(&self) -> impl Iterator<Item = &str> {
        self.names.split(NAMES_SEP)
    }

    pub fn props(&self) -> Map<String, Value> {
        serde_json::from_str(&format!("{{{}}}", self.props)).unwrap_or_default()
    }
}

fn names_of(mut names: Vec<String>, name: &str) -> String {
    names.push(name.to_owned());
    names.join(&NAMES_SEP.to_string())
}

fn compact(props: Map<String, Value>) -> Box<str> {
    String::from_utf8(fragment(props)).unwrap_or_default().into_boxed_str()
}

/// One row of `addresses.parquet`, borrowed from its batch: a country has tens of millions,
/// which must not each become an object with its own strings.
pub struct AddrRow<'a> {
    pub local: u32,
    pub public_id: &'a str,
    pub street: &'a str,
    pub street_name: &'a str,
    pub number: Option<u32>,
    pub suffix: Option<&'a str>,
    pub label: &'a str,
    pub lat: i32,
    pub lon: i32,
    pub postcode: Option<&'a str>,
    pub postcode_source: Option<&'a str>,
    pub sources: Option<&'a str>,
    pub municipality: Option<&'a str>,
}

#[derive(Default)]
pub struct Canonical {
    pub docs: Vec<DocIn>,
    /// The names of each entity named in two or more languages (`name:<lang>`), by language.
    pub multilingual: Vec<Vec<(String, String)>>,
}

fn err(path: &Path, e: impl std::fmt::Display) -> GekoError {
    GekoError::Data(format!("{}: {e}", path.display()))
}

/// Calls `f` with each batch of `path`, one in memory at a time.
fn for_each_batch(path: &Path, mut f: impl FnMut(&RecordBatch) -> Result<()>) -> Result<()> {
    let file = File::open(path).map_err(|e| err(path, e))?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file).and_then(|b| b.build()).map_err(|e| err(path, e))?;
    for b in reader {
        f(&b.map_err(|e| err(path, e))?)?;
    }
    Ok(())
}

struct Row<'a> {
    batch: &'a RecordBatch,
    path: &'a Path,
}

impl<'a> Row<'a> {
    fn col(&self, name: &str) -> Result<&'a dyn Array> {
        self.batch.column_by_name(name).map(|c| c.as_ref()).ok_or_else(|| err(self.path, format!("missing column `{name}`")))
    }

    fn s(&self, name: &str) -> Result<impl Fn(usize) -> Option<&'a str> + 'a> {
        let c = self.col(name)?.as_string::<i32>();
        Ok(move |i| c.is_valid(i).then(|| c.value(i)))
    }

    fn i32(&self, name: &str) -> Result<impl Fn(usize) -> i32 + 'a> {
        let c = self.col(name)?.as_primitive::<Int32Type>();
        Ok(move |i| c.value(i))
    }

    fn i64(&self, name: &str) -> Result<impl Fn(usize) -> Option<i64> + 'a> {
        let c = self.col(name)?.as_primitive::<Int64Type>();
        Ok(move |i| c.is_valid(i).then(|| c.value(i)))
    }

    fn u32(&self, name: &str) -> Result<impl Fn(usize) -> Option<u32> + 'a> {
        let c = self.col(name)?.as_primitive::<UInt32Type>();
        Ok(move |i| c.is_valid(i).then(|| c.value(i)))
    }

    fn u16(&self, name: &str) -> Result<impl Fn(usize) -> u16 + 'a> {
        let c = self.col(name)?.as_primitive::<UInt16Type>();
        Ok(move |i| c.value(i))
    }
}

fn json_names(json: Option<&str>) -> Vec<String> {
    match json.and_then(|j| serde_json::from_str::<Value>(j).ok()) {
        Some(Value::Object(m)) => m.into_values().filter_map(|v| v.as_str().map(str::to_owned)).collect(),
        Some(Value::Array(a)) => a.into_iter().filter_map(|v| v.as_str().map(str::to_owned)).collect(),
        _ => Vec::new(),
    }
}

/// The names of `json` in each language (`name:<lang>`) that differ from `name`, for labels in
/// the language asked for (`lang`); null when there are none, so the fragment leaves it out.
fn names_by_language(json: Option<&str>, name: &str) -> Value {
    let Some(Value::Object(m)) = json.and_then(|j| serde_json::from_str::<Value>(j).ok()) else { return Value::Null };
    let by_lang: Map<String, Value> = m
        .into_iter()
        .filter_map(|(k, v)| {
            let lang = k.strip_prefix("name:")?;
            let valid = !lang.is_empty() && lang.len() <= 8 && lang.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
            (valid && v.as_str().is_some_and(|s| s != name)).then(|| (lang.to_owned(), v))
        })
        .collect();
    if by_lang.is_empty() { Value::Null } else { Value::Object(by_lang) }
}

/// Keeps the names of `json` tagged with a language (`name:<lang>`) if there are two or more.
fn collect_multilingual(json: Option<&str>, out: &mut Vec<Vec<(String, String)>>) {
    let Some(Value::Object(m)) = json.and_then(|j| serde_json::from_str::<Value>(j).ok()) else { return };
    let tagged: Vec<(String, String)> = m
        .into_iter()
        .filter_map(|(k, v)| Some((k.strip_prefix("name:")?.to_owned(), v.as_str()?.to_owned())))
        .collect();
    if tagged.len() >= 2 {
        out.push(tagged);
    }
}

fn json_value(json: Option<&str>) -> Value {
    json.and_then(|j| serde_json::from_str(j).ok()).unwrap_or(Value::Null)
}

fn sources(s: Option<&str>) -> Value {
    json!(s.unwrap_or_default().split(',').filter(|x| !x.is_empty()).collect::<Vec<_>>())
}

pub fn read(dir: &Path) -> Result<Canonical> {
    let mut out = Canonical::default();

    let path = dir.join("places.parquet");
    for_each_batch(&path, |b| {
        let r = Row { batch: b, path: &path };
        let (pid, local, kind, name, names) = (r.s("public_id")?, r.u32("local_id")?, r.s("kind")?, r.s("name")?, r.s("names")?);
        let (lat, lon, pop, muni, prov) = (r.i32("lat_e7")?, r.i32("lon_e7")?, r.i64("population")?, r.s("municipality_id")?, r.s("province_id")?);
        let (pcs, imp, src, oref, wd) = (r.s("postcodes")?, r.u16("importance")?, r.s("sources")?, r.s("official_ref")?, r.s("wikidata")?);
        let (pc, pc_source) = (r.s("postcode")?, r.s("postcode_source")?);
        for i in 0..b.num_rows() {
            let kind: PlaceKind = kind(i).unwrap_or_default().parse()?;
            let name = name(i).unwrap_or_default().to_owned();
            let mut props = Map::new();
            props.insert("population".into(), json!(pop(i)));
            props.insert("postcodes".into(), json_value(pcs(i)));
            props.insert("postcode".into(), json!(pc(i)));
            props.insert("postcode_source".into(), json!(pc_source(i)));
            props.insert("official_ref".into(), json!(oref(i)));
            props.insert("wikidata".into(), json!(wd(i)));
            props.insert("sources".into(), sources(src(i)));
            props.insert("names".into(), names_by_language(names(i), &name));
            collect_multilingual(names(i), &mut out.multilingual);
            out.docs.push(DocIn {
                local: local(i).unwrap_or_default(),
                kind,
                public_id: pid(i).unwrap_or_default().to_owned(),
                names: names_of(json_names(names(i)), &name),
                name,
                lat: lat(i),
                lon: lon(i),
                importance: imp(i),
                municipality: muni(i).map(str::to_owned),
                province: prov(i).map(str::to_owned),
                street_type: None,
                props: compact(props),
            });
        }
        Ok(())
    })?;

    let path = dir.join("streets.parquet");
    for_each_batch(&path, |b| {
        let r = Row { batch: b, path: &path };
        let (pid, local, name, names, aliases) = (r.s("public_id")?, r.u32("local_id")?, r.s("name")?, r.s("names")?, r.s("aliases")?);
        let (lat, lon, muni, pcs, imp, src) =
            (r.i32("lat_e7")?, r.i32("lon_e7")?, r.s("municipality_id")?, r.s("postcodes")?, r.u16("importance")?, r.s("sources")?);
        let (stype, locality) = (r.s("type_code")?, r.s("locality")?);
        for i in 0..b.num_rows() {
            let name = name(i).unwrap_or_default().to_owned();
            collect_multilingual(names(i), &mut out.multilingual);
            let mut search_names = json_names(names(i));
            search_names.extend(json_names(aliases(i)));
            let mut props = Map::new();
            props.insert("locality".into(), json!(locality(i)));
            props.insert("postcodes".into(), json_value(pcs(i)));
            props.insert("sources".into(), sources(src(i)));
            props.insert("names".into(), names_by_language(names(i), &name));
            out.docs.push(DocIn {
                local: local(i).unwrap_or_default(),
                kind: PlaceKind::Street,
                public_id: pid(i).unwrap_or_default().to_owned(),
                names: names_of(search_names, &name),
                name,
                lat: lat(i),
                lon: lon(i),
                importance: imp(i),
                municipality: muni(i).map(str::to_owned),
                province: None,
                street_type: stype(i).map(str::to_owned),
                props: compact(props),
            });
        }
        Ok(())
    })?;

    let path = dir.join("pois.parquet");
    for_each_batch(&path, |b| {
        let r = Row { batch: b, path: &path };
        let (pid, local, name, names, category) = (r.s("public_id")?, r.u32("local_id")?, r.s("name")?, r.s("names")?, r.s("category")?);
        let (lat, lon, muni, pc, pcs) = (r.i32("lat_e7")?, r.i32("lon_e7")?, r.s("municipality_id")?, r.s("postcode")?, r.s("postcode_source")?);
        let (addr, imp, src, wd) = (r.s("address_id")?, r.u16("importance")?, r.s("sources")?, r.s("wikidata")?);
        for i in 0..b.num_rows() {
            let name = name(i).unwrap_or_default().to_owned();
            let mut props = Map::new();
            props.insert("category".into(), json!(category(i)));
            props.insert("postcode".into(), json!(pc(i)));
            props.insert("postcode_source".into(), json!(pcs(i)));
            props.insert("address_id".into(), json!(addr(i)));
            props.insert("wikidata".into(), json!(wd(i)));
            props.insert("names".into(), names_by_language(names(i), &name));
            props.insert("sources".into(), sources(src(i)));
            collect_multilingual(names(i), &mut out.multilingual);
            out.docs.push(DocIn {
                local: local(i).unwrap_or_default(),
                kind: PlaceKind::Poi,
                public_id: pid(i).unwrap_or_default().to_owned(),
                names: names_of(json_names(names(i)), &name),
                name,
                lat: lat(i),
                lon: lon(i),
                importance: imp(i),
                municipality: muni(i).map(str::to_owned),
                province: None,
                street_type: None,
                props: compact(props),
            });
        }
        Ok(())
    })?;

    let path = dir.join("postcodes.parquet");
    for_each_batch(&path, |b| {
        let r = Row { batch: b, path: &path };
        let (pid, local, pc, lat, lon) = (r.s("public_id")?, r.u32("local_id")?, r.s("postcode")?, r.i32("lat_e7")?, r.i32("lon_e7")?);
        let (muni, imp, count) = (r.s("municipality_id")?, r.u16("importance")?, r.u32("address_count")?);
        for i in 0..b.num_rows() {
            let name = pc(i).unwrap_or_default().to_owned();
            let mut props = Map::new();
            props.insert("postcode".into(), json!(name));
            props.insert("address_count".into(), json!(count(i)));
            out.docs.push(DocIn {
                local: local(i).unwrap_or_default(),
                kind: PlaceKind::Postcode,
                public_id: pid(i).unwrap_or_default().to_owned(),
                names: name.clone(),
                name,
                lat: lat(i),
                lon: lon(i),
                importance: imp(i),
                municipality: muni(i).map(str::to_owned),
                province: None,
                street_type: None,
                props: compact(props),
            });
        }
        Ok(())
    })?;

    out.docs.sort_by_key(|d| d.local);
    Ok(out)
}

/// Calls `f` with every address, one batch in memory at a time.
pub fn for_each_address(dir: &Path, mut f: impl FnMut(AddrRow<'_>) -> Result<()>) -> Result<()> {
    let path = dir.join("addresses.parquet");
    let file = File::open(&path).map_err(|e| err(&path, e))?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file).and_then(|b| b.with_batch_size(65_536).build()).map_err(|e| err(&path, e))?;
    for b in reader {
        let b = b.map_err(|e| err(&path, e))?;
        let r = Row { batch: &b, path: &path };
        let (pid, local, street, street_name, number) =
            (r.s("public_id")?, r.u32("local_id")?, r.s("street_id")?, r.s("street_name")?, r.u32("number")?);
        let (suffix, label, lat, lon, pc) = (r.s("suffix")?, r.s("label")?, r.i32("lat_e7")?, r.i32("lon_e7")?, r.s("postcode")?);
        let (pcs, muni, src) = (r.s("postcode_source")?, r.s("municipality_id")?, r.s("sources")?);
        for i in 0..b.num_rows() {
            f(AddrRow {
                local: local(i).unwrap_or_default(),
                public_id: pid(i).unwrap_or_default(),
                street: street(i).unwrap_or_default(),
                street_name: street_name(i).unwrap_or_default(),
                number: number(i),
                suffix: suffix(i),
                label: label(i).unwrap_or_default(),
                lat: lat(i),
                lon: lon(i),
                postcode: pc(i),
                postcode_source: pcs(i),
                sources: src(i),
                municipality: muni(i),
            })?;
        }
    }
    Ok(())
}

/// The properties an address carries itself, as the index stores them.
pub fn address_props(postcode_source: Option<&str>, sources: Option<&str>) -> Map<String, Value> {
    let mut props = Map::new();
    props.insert("postcode_source".into(), json!(postcode_source));
    props.insert("sources".into(), self::sources(sources));
    props
}

//! Joins the canonical datasets fused partition by partition into the one of the country
//! (PROJECT.md §6.5.8), reading a few columns at a time and writing batch by batch, so memory
//! does not grow with the country:
//!
//! - Local ids are renumbered over the whole country: searchable entities by importance, then
//!   addresses grouped by street (PROJECT.md §5.5).
//! - Boundaries above a partition (a region, the country) are fused by every partition they
//!   contain: they are kept once.
//! - A postcode that a partition border splits is combined into one.
//! - Any other public id that two partitions produce is an error: an entity belongs to one.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow_array::{ArrayRef, BooleanArray, RecordBatch, UInt32Array};
use geko_core::country::CountryCode;
use geko_core::model::PlaceKind;
use geko_core::scope::Partition;
use geko_core::{GekoError, Result};
use serde::Serialize;

use super::{FusionReport, REPORT};
use crate::incremental::{self, Key};
use super::ids::POSTCODE_IMPORTANCE;
use super::io::{BatchWriter, Columns, for_each_batch, read_columns, schema_of};
use super::output::SCHEMA_VERSION;

pub fn partition_name(p: &Partition) -> String {
    p.region.as_ref().map_or_else(|| "_all".to_owned(), |r| r.as_str().to_owned())
}

pub fn partition_dir(data_dir: &Path, p: &Partition) -> PathBuf {
    data_dir.join("canonical").join(p.country.as_str()).join("parts").join(partition_name(p))
}

const SKIP: u32 = u32::MAX;

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct CountryReport {
    pub country: String,
    pub entities: BTreeMap<String, u64>,
    /// Boundaries fused by several partitions, kept once.
    pub shared_boundaries: usize,
    /// OSM objects spanning two regions, found in the partitions of both and kept once.
    #[serde(default)]
    pub split_osm_objects: usize,
    /// Postcodes split by a partition border, combined.
    pub split_postcodes: usize,
    pub partitions: Vec<FusionReport>,
    pub elapsed_ms: u128,
}

fn data_err(e: impl std::fmt::Display) -> GekoError {
    GekoError::Data(e.to_string())
}

/// One partition's local ids mapped to the country's.
struct PartMap {
    /// Indexed by the partition's local id; `SKIP` for rows dropped as duplicates.
    docs: Vec<u32>,
    addresses: Vec<u32>,
}

struct Doc {
    importance: u16,
    public_id: String,
    part: usize,
    local: u32,
}

#[derive(Default)]
struct Postcode {
    postcode: String,
    sum_lat: i64,
    sum_lon: i64,
    count: u64,
    /// Municipality of the partition with the most addresses of this postcode.
    municipality: Option<(u32, Option<String>)>,
    locals: Vec<(usize, u32)>,
}

pub fn country_dir(data_dir: &Path, country: CountryCode) -> PathBuf {
    data_dir.join("canonical").join(country.as_str())
}

/// Entries of `country_dir` that are not part of the canonical dataset: the partitions it is
/// merged from and the report.
pub const NOT_CANONICAL: &[&str] = &["parts", REPORT];

/// Merges the partitions unless the merge code (`code`) and every fused partition are unchanged
/// since the last merge (PROJECT.md §8.3). Returns the report and whether the merge was reused.
pub fn merge_incremental(
    data_dir: &Path,
    country: CountryCode,
    partitions: &[Partition],
    reports: Vec<FusionReport>,
    code: &str,
    force: bool,
) -> Result<(CountryReport, bool)> {
    let out_dir = country_dir(data_dir, country);
    let mut key = Key::new("merge").add("code", code);
    for p in partitions {
        key = key.add(&partition_name(p), &incremental::output_of(&partition_dir(data_dir, p), &[REPORT])?);
    }
    let key = key.finish();
    if !force
        && incremental::up_to_date(&out_dir, &key).is_some()
        && let Some(report) = std::fs::read(out_dir.join(REPORT)).ok().and_then(|b| serde_json::from_slice(&b).ok())
    {
        return Ok((report, true));
    }
    incremental::invalidate(&out_dir)?;
    let report = merge(data_dir, country, partitions, reports)?;
    incremental::seal(&out_dir, key, NOT_CANONICAL, serde_json::Value::Null)?;
    Ok((report, false))
}

pub fn merge(data_dir: &Path, country: CountryCode, partitions: &[Partition], reports: Vec<FusionReport>) -> Result<CountryReport> {
    let started = std::time::Instant::now();
    let dirs: Vec<PathBuf> = partitions.iter().map(|p| partition_dir(data_dir, p)).collect();
    let out_dir = country_dir(data_dir, country);

    // Searchable entities: importance and public id decide the order; shared boundaries once.
    let mut docs: Vec<Doc> = Vec::new();
    // Per public id: partition, index in `docs` and size of its geometry.
    let mut seen: HashMap<String, (usize, usize, usize)> = HashMap::new();
    let mut shared_boundaries = 0;
    let mut split_osm_objects = 0;
    let mut part_docs = vec![0u32; dirs.len()];
    let mut street_locals: Vec<HashMap<String, u32>> = vec![HashMap::new(); dirs.len()];
    let mut postcodes: BTreeMap<String, Postcode> = BTreeMap::new();
    for (part, dir) in dirs.iter().enumerate() {
        for table in ["places", "streets", "pois"] {
            let path = dir.join(format!("{table}.parquet"));
            let names: &[&str] = if table == "places" { &["public_id", "local_id", "importance", "kind", "geom"] } else { &["public_id", "local_id", "importance"] };
            for batch in read_columns(&path, names)? {
                let c = Columns::new(&batch, &path);
                let (pid, local, imp) = (c.str("public_id")?, c.u32("local_id")?, c.u16("importance")?);
                let kind = if table == "places" { Some(c.str("kind")?) } else { None };
                let geom = if table == "places" { Some(c.bin("geom")?) } else { None };
                for i in 0..c.rows() {
                    let public_id = pid(i).unwrap_or_default();
                    part_docs[part] = part_docs[part].max(local(i) + 1);
                    let geom_len = geom.as_ref().map_or(0, |g| g(i).len());
                    if let Some(&(first, doc, first_len)) = seen.get(public_id) {
                        // Administrative units contain several partitions (a region) or reach
                        // into a neighbouring extract (a border municipality). An OSM object that
                        // spans two regions is in the extract of each with part of its geometry,
                        // so its point may fall in both partitions. Each is one entity: it is
                        // kept once, with the most complete geometry.
                        let boundary = kind.as_ref().and_then(|k| k(i)).is_some_and(|k| {
                            matches!(k.parse::<PlaceKind>(), Ok(PlaceKind::Country | PlaceKind::Region | PlaceKind::Province | PlaceKind::Municipality))
                        });
                        let osm_object = geko_core::model::PublicId::parse(public_id).is_ok_and(|id| id.key().starts_with("osm-"));
                        if boundary {
                            shared_boundaries += 1;
                        } else if osm_object {
                            split_osm_objects += 1;
                        } else {
                            return Err(GekoError::Data(format!(
                                "{public_id} was fused by partitions {} and {}",
                                partition_name(&partitions[first]),
                                partition_name(&partitions[part])
                            )));
                        }
                        if geom_len > first_len {
                            docs[doc] = Doc { importance: imp(i), public_id: public_id.to_owned(), part, local: local(i) };
                            seen.insert(public_id.to_owned(), (part, doc, geom_len));
                        }
                        continue;
                    }
                    seen.insert(public_id.to_owned(), (part, docs.len(), geom_len));
                    if table == "streets" {
                        street_locals[part].insert(public_id.to_owned(), local(i));
                    }
                    docs.push(Doc { importance: imp(i), public_id: public_id.to_owned(), part, local: local(i) });
                }
            }
        }
        let path = dir.join("postcodes.parquet");
        for batch in read_columns(&path, &["postcode", "local_id", "lat_e7", "lon_e7", "municipality_id", "address_count"])? {
            let c = Columns::new(&batch, &path);
            let (pc, local, lat, lon, muni, count) =
                (c.str("postcode")?, c.u32("local_id")?, c.i32("lat_e7")?, c.i32("lon_e7")?, c.str("municipality_id")?, c.u32("address_count")?);
            for i in 0..c.rows() {
                part_docs[part] = part_docs[part].max(local(i) + 1);
                let code = pc(i).unwrap_or_default();
                let p = postcodes.entry(code.to_owned()).or_insert_with(|| Postcode { postcode: code.to_owned(), ..Default::default() });
                let n = count(i) as i64;
                p.sum_lat += lat(i) as i64 * n;
                p.sum_lon += lon(i) as i64 * n;
                p.count += n as u64;
                if p.municipality.as_ref().is_none_or(|(best, _)| count(i) > *best) {
                    p.municipality = Some((count(i), muni(i).map(str::to_owned)));
                }
                p.locals.push((part, local(i)));
            }
        }
    }
    drop(seen);
    let split_postcodes = postcodes.values().filter(|p| p.locals.len() > 1).count();
    let country_lc = country.lowercase();
    let postcode_docs: Vec<(String, &Postcode)> = postcodes.values().map(|p| (format!("geko:postcode:{country_lc}-{}", p.postcode), p)).collect();

    enum Key<'a> {
        Doc(&'a Doc),
        Postcode(usize),
    }
    let mut order: Vec<(u16, &str, Key)> = docs.iter().map(|d| (d.importance, d.public_id.as_str(), Key::Doc(d))).collect();
    order.extend(postcode_docs.iter().enumerate().map(|(i, (pid, _))| (POSTCODE_IMPORTANCE, pid.as_str(), Key::Postcode(i))));
    order.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
    let mut maps: Vec<PartMap> = part_docs.iter().map(|&n| PartMap { docs: vec![SKIP; n as usize], addresses: Vec::new() }).collect();
    let mut postcode_global = vec![0u32; postcode_docs.len()];
    let mut street_global: Vec<HashMap<&str, u32>> = vec![HashMap::new(); dirs.len()];
    for (global, (_, pid, key)) in order.iter().enumerate() {
        match key {
            Key::Doc(d) => {
                maps[d.part].docs[d.local as usize] = global as u32;
                if street_locals[d.part].contains_key(*pid) {
                    street_global[d.part].insert(*pid, global as u32);
                }
            }
            Key::Postcode(i) => {
                postcode_global[*i] = global as u32;
                for &(part, local) in &postcode_docs[*i].1.locals {
                    maps[part].docs[local as usize] = global as u32;
                }
            }
        }
    }
    let total_docs = order.len() as u32;
    drop(order);
    drop(street_locals);

    // Addresses: grouped by street in the country's street order. Within a partition they are
    // already numbered by street, then number; a street's addresses are all in its partition.
    let mut addresses: Vec<(u32, u32, u32)> = Vec::new();
    for (part, dir) in dirs.iter().enumerate() {
        let path = dir.join("addresses.parquet");
        for batch in read_columns(&path, &["street_id", "local_id"])? {
            let c = Columns::new(&batch, &path);
            let (street, local) = (c.str("street_id")?, c.u32("local_id")?);
            for i in 0..c.rows() {
                let street = street(i).unwrap_or_default();
                let s = *street_global[part].get(street).ok_or_else(|| data_err(format!("address of unknown street {street}")))?;
                addresses.push((s, part as u32, local(i)));
            }
        }
    }
    drop(street_global);
    addresses.sort_unstable();
    let mut per_part = vec![0usize; dirs.len()];
    for a in &addresses {
        per_part[a.1 as usize] += 1;
    }
    for (map, n) in maps.iter_mut().zip(per_part) {
        map.addresses = vec![SKIP; n];
    }
    for (rank, &(_, part, local)) in addresses.iter().enumerate() {
        let map = &mut maps[part as usize];
        map.addresses[(local - part_docs[part as usize]) as usize] = total_docs + rank as u32;
    }
    let total_addresses = addresses.len() as u64;
    drop(addresses);

    // Rewrite every table with the country's local ids.
    let mut entities = BTreeMap::new();
    for table in ["places", "streets", "pois", "addresses"] {
        let rows = rewrite(&dirs, &out_dir, table, |part, local| {
            let m = &maps[part];
            if local < part_docs[part] { m.docs[local as usize] } else { m.addresses[(local - part_docs[part]) as usize] }
        })?;
        entities.insert(table.to_owned(), rows);
    }
    entities.insert("postcodes".into(), write_postcodes(&dirs[0], &out_dir, &postcode_docs, &postcode_global)?);
    for table in ["postcode_cells", "conflicts"] {
        let rows = concat(&dirs, &out_dir, table)?;
        if table == "conflicts" {
            entities.insert(table.to_owned(), rows);
        }
    }
    debug_assert_eq!(entities["addresses"], total_addresses);

    let report = CountryReport {
        country: country.to_string(),
        entities,
        shared_boundaries,
        split_osm_objects,
        split_postcodes,
        partitions: reports,
        elapsed_ms: started.elapsed().as_millis(),
    };
    std::fs::write(out_dir.join(REPORT), serde_json::to_vec_pretty(&report).map_err(|e| GekoError::Other(e.into()))?)?;
    Ok(report)
}

/// Copies `table` of every partition into the country's, with `local_id` mapped by `map`;
/// rows mapped to `SKIP` are dropped.
fn rewrite(dirs: &[PathBuf], out_dir: &Path, table: &str, map: impl Fn(usize, u32) -> u32) -> Result<u64> {
    let file = format!("{table}.parquet");
    let schema = schema_of(&dirs[0].join(&file))?;
    let col = schema.index_of("local_id").map_err(data_err)?;
    let mut writer = BatchWriter::create(&out_dir.join(&file), Arc::clone(&schema), SCHEMA_VERSION)?;
    for (part, dir) in dirs.iter().enumerate() {
        let path = dir.join(&file);
        for_each_batch(&path, |batch| {
            let locals = Columns::new(&batch, &path).u32("local_id")?;
            let mapped: Vec<u32> = (0..batch.num_rows()).map(|i| map(part, locals(i))).collect();
            let keep = BooleanArray::from_iter(mapped.iter().map(|&m| Some(m != SKIP)));
            let mut columns: Vec<ArrayRef> = batch.columns().to_vec();
            columns[col] = Arc::new(UInt32Array::from(mapped));
            let batch = RecordBatch::try_new(Arc::clone(&schema), columns).map_err(data_err)?;
            let batch = if keep.true_count() == batch.num_rows() { batch } else { arrow_select::filter::filter_record_batch(&batch, &keep).map_err(data_err)? };
            writer.write(&batch)
        })?;
    }
    writer.finish()
}

fn concat(dirs: &[PathBuf], out_dir: &Path, table: &str) -> Result<u64> {
    let file = format!("{table}.parquet");
    let mut writer = BatchWriter::create(&out_dir.join(&file), schema_of(&dirs[0].join(&file))?, SCHEMA_VERSION)?;
    for dir in dirs {
        for_each_batch(&dir.join(&file), |batch| writer.write(&batch))?;
    }
    writer.finish()
}

fn write_postcodes(first_part: &Path, out_dir: &Path, postcodes: &[(String, &Postcode)], global: &[u32]) -> Result<u64> {
    use arrow_array::{Int32Array, StringArray, UInt16Array};
    let schema = schema_of(&first_part.join("postcodes.parquet"))?;
    let n = |p: &Postcode| p.count.max(1) as i64;
    let columns: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from_iter_values(postcodes.iter().map(|(pid, _)| pid.as_str()))),
        Arc::new(UInt32Array::from(global.to_vec())),
        Arc::new(StringArray::from_iter_values(postcodes.iter().map(|(_, p)| p.postcode.as_str()))),
        Arc::new(Int32Array::from_iter_values(postcodes.iter().map(|(_, p)| (p.sum_lat / n(p)) as i32))),
        Arc::new(Int32Array::from_iter_values(postcodes.iter().map(|(_, p)| (p.sum_lon / n(p)) as i32))),
        Arc::new(StringArray::from_iter(postcodes.iter().map(|(_, p)| p.municipality.as_ref().and_then(|m| m.1.as_deref())))),
        Arc::new(UInt32Array::from_iter_values(postcodes.iter().map(|(_, p)| p.count as u32))),
        Arc::new(UInt16Array::from(vec![POSTCODE_IMPORTANCE; postcodes.len()])),
    ];
    let mut writer = BatchWriter::create(&out_dir.join("postcodes.parquet"), Arc::clone(&schema), SCHEMA_VERSION)?;
    writer.write(&RecordBatch::try_new(schema, columns).map_err(data_err)?)?;
    writer.finish()
}

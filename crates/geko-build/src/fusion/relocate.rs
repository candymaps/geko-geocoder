//! Records staged in a partition whose boundary does not contain them, sent to the partition that
//! does (PROJECT.md §6.5.8).
//!
//! OSM is staged clipped to the boundary of each region. Other sources are staged however they are
//! published (CartoCiudad, by postcode prefix), so some of their points lie in another partition,
//! where fusion could assign them no municipality. This stage reads the boundary of every partition
//! from its OSM staging and, for each record of another source outside its own partition, moves it
//! to the partition that contains it:
//!
//! - `relocated/<country>/<to>/<source>/<file>`: the rows arriving, with the schema of the staged
//!   file, in partition and row order.
//! - `relocated/<country>/<from>/<source>/<file>.moved.json`: the row numbers leaving.
//!
//! A record that no partition of the scope contains stays where it was staged.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use arrow_array::UInt32Array;
use geko_core::country::CountryCode;
use geko_core::scope::Partition;
use geko_core::{GekoError, Result};
use geko_geom::{PreparedArea, wkb};
use rayon::prelude::*;
use rustc_hash::FxHashSet;
use serde::{Deserialize, Serialize};

use super::io::{BatchWriter, Columns, for_each_batch, read_columns, schema_of};
use super::merge::partition_name;
use super::records::OSM;
use super::staged_dirs;
use crate::fetch::staging_dir;
use crate::incremental::{self, Key};

const LAT: &str = "lat_e7";
const LON: &str = "lon_e7";
const MOVED: &str = ".moved.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RelocationReport {
    /// Records moved, by destination partition.
    pub moved: BTreeMap<String, usize>,
    /// Records outside their partition that no partition of the scope contains, kept in place.
    pub unplaced: usize,
    pub elapsed_ms: u128,
}

impl RelocationReport {
    pub fn total(&self) -> usize {
        self.moved.values().sum()
    }
}

pub fn country_dir(data_dir: &Path, country: CountryCode) -> PathBuf {
    data_dir.join("relocated").join(country.as_str())
}

fn partition_dir(data_dir: &Path, p: &Partition) -> PathBuf {
    country_dir(data_dir, p.country).join(partition_name(p))
}

/// Digest of what relocation gives and takes from `p`, an input of its fusion.
pub fn digest_of(data_dir: &Path, p: &Partition) -> Result<String> {
    let dir = partition_dir(data_dir, p);
    if dir.is_dir() { incremental::digest(&dir, &[]) } else { Ok("none".to_owned()) }
}

/// What relocation changed in one staged file of a partition.
#[derive(Default)]
pub struct Relocated {
    /// Row numbers that left for another partition.
    pub moved: FxHashSet<u64>,
    /// Rows that arrived from other partitions, with the schema of the staged file.
    pub incoming: Option<PathBuf>,
}

pub fn of(data_dir: &Path, p: &Partition, source: &str, file: &str) -> Result<Relocated> {
    let dir = partition_dir(data_dir, p).join(source);
    let moved_path = dir.join(format!("{file}{MOVED}"));
    let moved = match std::fs::read(&moved_path) {
        Ok(bytes) => serde_json::from_slice::<Vec<u64>>(&bytes)
            .map_err(|e| GekoError::Data(format!("{}: {e}", moved_path.display())))?
            .into_iter()
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => FxHashSet::default(),
        Err(e) => return Err(e.into()),
    };
    let incoming = Some(dir.join(file)).filter(|p| p.is_file());
    Ok(Relocated { moved, incoming })
}

/// Relocates the records of `partitions` (all of one country) unless the code and the staging
/// of every partition are unchanged since the last run. Returns the report and whether the
/// earlier run was reused.
pub fn relocate_incremental(
    data_dir: &Path,
    country: CountryCode,
    partitions: &[Partition],
    sources: &[String],
    code: &str,
    force: bool,
) -> Result<(RelocationReport, bool)> {
    let dir = country_dir(data_dir, country);
    let mut key = Key::new("relocate").add("code", code);
    for p in partitions {
        for (source, staged) in staged_dirs(data_dir, p, sources) {
            key = key.add(&format!("{source}/{}", partition_name(p)), &incremental::output_of(&staged, &[])?);
        }
    }
    for (source, staged) in country_staging(data_dir, partitions, sources) {
        key = key.add(&format!("{source}/_all"), &incremental::output_of(&staged, &[])?);
    }
    let key = key.finish();
    if !force
        && let Some(stamp) = incremental::up_to_date(&dir, &key)
        && let Ok(report) = serde_json::from_value(stamp.info)
    {
        return Ok((report, true));
    }
    incremental::invalidate(&dir)?;
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    let report = relocate(data_dir, partitions, sources)?;
    incremental::seal(&dir, key, &[], serde_json::to_value(&report).map_err(|e| GekoError::Other(e.into()))?)?;
    Ok((report, false))
}

/// The boundary of a region partition, from the OSM boundary its staging was clipped to.
fn boundary(data_dir: &Path, p: &Partition) -> Result<Option<PreparedArea>> {
    let Some(region) = &p.region else { return Ok(None) };
    let path = staging_dir(data_dir, &geko_core::model::SourceId(OSM.into()), p).path.join("admins.parquet");
    if !path.is_file() {
        return Ok(None);
    }
    for batch in read_columns(&path, &["iso3166_2", "geom"])? {
        let c = Columns::new(&batch, &path);
        let (iso, geom) = (c.str("iso3166_2")?, c.bin("geom")?);
        for i in 0..c.rows() {
            if iso(i).is_some_and(|c| c.eq_ignore_ascii_case(region.as_str()))
                && let Some(wkb::Geometry::Polygons(polygons)) = wkb::parse(geom(i))
            {
                let rings: Vec<_> = polygons.into_iter().flatten().collect();
                return Ok(Some(PreparedArea::new(&rings)));
            }
        }
    }
    tracing::warn!(partition = %partition_name(p), "no boundary in the OSM staging; its records stay in place");
    Ok(None)
}

/// One staged Parquet file with coordinates, of a source not clipped to its partition.
struct StagedFile {
    /// The partition it was staged in; `None` for a source staged by country (`_all`).
    from: Option<usize>,
    source: String,
    file: String,
    path: PathBuf,
}

fn staged_files(data_dir: &Path, partitions: &[Partition], sources: &[String], areas: &[Option<PreparedArea>]) -> Result<Vec<StagedFile>> {
    let mut out = Vec::new();
    for (from, p) in partitions.iter().enumerate() {
        if areas[from].is_none() {
            continue;
        }
        for (source, dir) in staged_dirs(data_dir, p, sources) {
            if source == OSM {
                continue;
            }
            let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)?
                .map(|e| e.map(|e| e.path()))
                .collect::<std::io::Result<_>>()?;
            files.retain(|f| f.extension().is_some_and(|x| x == "parquet"));
            files.sort();
            for path in files {
                let schema = schema_of(&path)?;
                if schema.field_with_name(LAT).is_ok() && schema.field_with_name(LON).is_ok() {
                    let file = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                    out.push(StagedFile { from: Some(from), source: source.clone(), file, path });
                }
            }
        }
    }
    for (source, dir) in country_staging(data_dir, partitions, sources) {
        for path in parquet_files(&dir)? {
            let schema = schema_of(&path)?;
            if schema.field_with_name(LAT).is_ok() && schema.field_with_name(LON).is_ok() {
                let file = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                out.push(StagedFile { from: None, source: source.clone(), file, path });
            }
        }
    }
    Ok(out)
}

fn parquet_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?.map(|e| e.map(|e| e.path())).collect::<std::io::Result<_>>()?;
    files.retain(|f| f.extension().is_some_and(|x| x == "parquet"));
    files.sort();
    Ok(files)
}

/// Staging of the sources staged by country (`_all`) when the country is partitioned by region:
/// every record goes to the region that contains it.
fn country_staging(data_dir: &Path, partitions: &[Partition], sources: &[String]) -> Vec<(String, PathBuf)> {
    let Some(first) = partitions.first().filter(|_| partitions.iter().all(|p| p.region.is_some())) else { return Vec::new() };
    let whole = Partition { country: first.country, region: None };
    staged_dirs(data_dir, &whole, sources).into_iter().filter(|(s, _)| s != OSM).collect()
}

fn relocate(data_dir: &Path, partitions: &[Partition], sources: &[String]) -> Result<RelocationReport> {
    let started = Instant::now();
    let areas: Vec<Option<PreparedArea>> =
        partitions.par_iter().map(|p| boundary(data_dir, p)).collect::<Result<_>>()?;
    let files = staged_files(data_dir, partitions, sources, &areas)?;

    // Per file, the rows outside its partition and where each goes.
    let found: Vec<(Vec<(u64, usize)>, usize)> = files
        .par_iter()
        .map(|f| {
            let own = f.from.map(|from| areas[from].as_ref().expect("files of partitions with a boundary"));
            let (mut moves, mut unplaced, mut row) = (Vec::new(), 0, 0u64);
            for batch in read_columns(&f.path, &[LAT, LON])? {
                let c = Columns::new(&batch, &f.path);
                let (lat, lon) = (c.i32(LAT)?, c.i32(LON)?);
                for i in 0..c.rows() {
                    let pt = (lon(i), lat(i));
                    if !own.is_some_and(|own| own.contains(pt)) {
                        match areas.iter().enumerate().position(|(k, a)| Some(k) != f.from && a.as_ref().is_some_and(|a| a.contains(pt))) {
                            Some(to) => moves.push((row, to)),
                            None => unplaced += 1,
                        }
                    }
                    row += 1;
                }
            }
            Ok((moves, unplaced))
        })
        .collect::<Result<_>>()?;

    let mut report = RelocationReport::default();
    let mut writers: BTreeMap<(usize, String, String), BatchWriter> = BTreeMap::new();
    for (f, (moves, unplaced)) in files.iter().zip(found) {
        report.unplaced += unplaced;
        if moves.is_empty() {
            continue;
        }
        // Staged by country, nothing is loaded from where it was staged: no rows to leave out there.
        if let Some(from) = f.from {
            let rows: Vec<u64> = moves.iter().map(|&(row, _)| row).collect();
            let out = partition_dir(data_dir, &partitions[from]).join(&f.source);
            std::fs::create_dir_all(&out)?;
            std::fs::write(out.join(format!("{}{MOVED}", f.file)), serde_json::to_vec(&rows).map_err(|e| GekoError::Other(e.into()))?)?;
        }

        let schema = schema_of(&f.path)?;
        let (mut offset, mut next) = (0u64, 0usize);
        for_each_batch(&f.path, |batch| {
            let end = offset + batch.num_rows() as u64;
            let mut by_dest: BTreeMap<usize, Vec<u32>> = BTreeMap::new();
            while next < moves.len() && moves[next].0 < end {
                let (row, to) = moves[next];
                by_dest.entry(to).or_default().push((row - offset) as u32);
                next += 1;
            }
            for (to, rows) in by_dest {
                let taken = arrow_select::take::take_record_batch(&batch, &UInt32Array::from(rows))
                    .map_err(|e| GekoError::Data(format!("{}: {e}", f.path.display())))?;
                *report.moved.entry(partition_name(&partitions[to])).or_default() += taken.num_rows();
                let writer = match writers.entry((to, f.source.clone(), f.file.clone())) {
                    std::collections::btree_map::Entry::Occupied(e) => e.into_mut(),
                    std::collections::btree_map::Entry::Vacant(e) => {
                        let path = partition_dir(data_dir, &partitions[to]).join(&f.source).join(&f.file);
                        e.insert(BatchWriter::create(&path, schema.clone(), "relocated")?)
                    }
                };
                writer.write(&taken)?;
            }
            offset = end;
            Ok(())
        })?;
    }
    for writer in writers.into_values() {
        writer.finish()?;
    }
    report.elapsed_ms = started.elapsed().as_millis();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow_array::{ArrayRef, BinaryArray, Int32Array, StringArray};
    use arrow_schema::{DataType, Field, Schema};
    use geko_core::country::RegionCode;
    use geko_geom::{Polygon, Pt};

    use super::*;
    use crate::fusion::io::write_parquet;

    fn partition(region: &str) -> Partition {
        Partition { country: CountryCode::new("ES").unwrap(), region: Some(RegionCode::new(region).unwrap()) }
    }

    fn square(x0: i32, size: i32) -> Vec<Pt> {
        vec![(x0, 0), (x0 + size, 0), (x0 + size, size), (x0, size), (x0, 0)]
    }

    fn stage_boundary(data_dir: &Path, p: &Partition, ring: Vec<Pt>) {
        let dir = staging_dir(data_dir, &geko_core::model::SourceId(OSM.into()), p).path;
        let geom = wkb::multi_polygon(&[Polygon { outer: ring, inners: vec![] }]);
        let schema = Arc::new(Schema::new(vec![
            Field::new("iso3166_2", DataType::Utf8, true),
            Field::new("geom", DataType::Binary, false),
        ]));
        let columns: Vec<ArrayRef> = vec![
            Arc::new(StringArray::from(vec![Some(p.region.as_ref().unwrap().as_str())])),
            Arc::new(BinaryArray::from(vec![geom.as_slice()])),
        ];
        write_parquet(&dir.join("admins.parquet"), schema, columns, "test").unwrap();
    }

    fn stage_points(data_dir: &Path, p: &Partition, points: &[(&str, Pt)]) {
        let dir = staging_dir(data_dir, &geko_core::model::SourceId("other".into()), p).path;
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Utf8, false),
            Field::new(LAT, DataType::Int32, false),
            Field::new(LON, DataType::Int32, false),
        ]));
        let columns: Vec<ArrayRef> = vec![
            Arc::new(StringArray::from(points.iter().map(|(id, _)| *id).collect::<Vec<_>>())),
            Arc::new(Int32Array::from(points.iter().map(|(_, p)| p.1).collect::<Vec<_>>())),
            Arc::new(Int32Array::from(points.iter().map(|(_, p)| p.0).collect::<Vec<_>>())),
        ];
        write_parquet(&dir.join("addresses.parquet"), schema, columns, "test").unwrap();
    }

    fn ids(path: &Path) -> Vec<String> {
        crate::fusion::io::read_batches(path)
            .unwrap()
            .iter()
            .flat_map(|b| {
                let c = Columns::new(b, path);
                let id = c.str("id").unwrap();
                (0..c.rows()).map(|i| id(i).unwrap().to_owned()).collect::<Vec<_>>()
            })
            .collect()
    }

    #[test]
    fn records_outside_their_partition_move_to_the_one_that_contains_them() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path();
        let (a, b) = (partition("ES-A"), partition("ES-MU"));
        stage_boundary(data, &a, square(0, 100));
        stage_boundary(data, &b, square(100, 100));
        stage_points(data, &a, &[("a1", (10, 10)), ("b1", (150, 50)), ("sea", (500, 500)), ("b2", (120, 20))]);
        stage_points(data, &b, &[("a2", (50, 50)), ("b3", (110, 10))]);
        let partitions = [a.clone(), b.clone()];
        let sources = ["osm".to_owned(), "other".to_owned()];

        let (report, reused) = relocate_incremental(data, a.country, &partitions, &sources, "1", false).unwrap();
        assert!(!reused);
        assert_eq!(report.moved, BTreeMap::from([("ES-A".to_owned(), 1), ("ES-MU".to_owned(), 2)]));
        assert_eq!(report.unplaced, 1);

        let in_a = of(data, &a, "other", "addresses.parquet").unwrap();
        assert_eq!(in_a.moved, FxHashSet::from_iter([1, 3]));
        assert_eq!(ids(in_a.incoming.as_ref().unwrap()), ["a2"]);
        let in_b = of(data, &b, "other", "addresses.parquet").unwrap();
        assert_eq!(in_b.moved, FxHashSet::from_iter([0]));
        assert_eq!(ids(in_b.incoming.as_ref().unwrap()), ["b1", "b2"]);

        let digest = digest_of(data, &b).unwrap();
        assert!(relocate_incremental(data, a.country, &partitions, &sources, "1", false).unwrap().1);
        assert!(!relocate_incremental(data, a.country, &partitions, &sources, "1", true).unwrap().1);
        assert_eq!(digest_of(data, &b).unwrap(), digest);
    }

    #[test]
    fn records_staged_by_country_go_to_the_region_that_contains_them() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path();
        let (a, b) = (partition("ES-A"), partition("ES-MU"));
        stage_boundary(data, &a, square(0, 100));
        stage_boundary(data, &b, square(100, 100));
        let whole = Partition { country: a.country, region: None };
        stage_points(data, &whole, &[("a1", (10, 10)), ("b1", (150, 50)), ("sea", (500, 500))]);
        let partitions = [a.clone(), b.clone()];
        let sources = ["osm".to_owned(), "other".to_owned()];

        let (report, _) = relocate_incremental(data, a.country, &partitions, &sources, "1", false).unwrap();
        assert_eq!(report.moved, BTreeMap::from([("ES-A".to_owned(), 1), ("ES-MU".to_owned(), 1)]));
        assert_eq!(report.unplaced, 1, "outside every region, left out");
        let in_a = of(data, &a, "other", "addresses.parquet").unwrap();
        assert!(in_a.moved.is_empty());
        assert_eq!(ids(in_a.incoming.as_ref().unwrap()), ["a1"]);
        assert_eq!(ids(of(data, &b, "other", "addresses.parquet").unwrap().incoming.as_ref().unwrap()), ["b1"]);
    }
}

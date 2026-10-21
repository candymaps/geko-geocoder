//! Quality of the data a build produced (PROJECT.md §13.4 and §13.6): what the fusion report
//! counts, and the integrity of the canonical dataset, so that a later build can be compared
//! with it. A source that silently loses records, or a fusion rule that starts leaving
//! duplicates or orphans, changes these numbers before it changes any reference query.

use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::hash::BuildHasher;
use std::path::Path;

use anyhow::{Context, Result};
use arrow_array::Array;
use arrow_array::cast::AsArray;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DataReport {
    /// Fused entities by kind.
    pub entities: BTreeMap<String, usize>,
    /// Records of each kind and source that went into the fusion (`addresses.osm`…).
    pub records: BTreeMap<String, usize>,
    /// Attributes inferred instead of taken from a source.
    pub inferred: BTreeMap<String, usize>,
    /// Pairs of duplicates and homonyms that the fusion metrics count.
    pub duplicates: BTreeMap<String, usize>,
    /// Integrity problems of the canonical dataset: each should be 0 or explained.
    pub problems: BTreeMap<String, usize>,
}

fn add_counts(into: &mut BTreeMap<String, usize>, value: &serde_json::Value) {
    for (k, v) in value.as_object().into_iter().flatten() {
        *into.entry(k.clone()).or_default() += v.as_u64().unwrap_or(0) as usize;
    }
}

/// Columns `names` of every row of a canonical file, batch by batch.
fn for_each(path: &Path, names: &[&str], mut f: impl FnMut(&[Option<&str>])) -> Result<()> {
    let file = File::open(path).with_context(|| path.display().to_string())?;
    for batch in ParquetRecordBatchReaderBuilder::try_new(file)?.with_batch_size(65_536).build()? {
        let batch = batch?;
        let columns = names
            .iter()
            .map(|n| batch.column_by_name(n).with_context(|| format!("{}: no column {n}", path.display())).map(|c| c.as_string::<i32>().clone()))
            .collect::<Result<Vec<_>>>()?;
        let mut row = vec![None; names.len()];
        for i in 0..batch.num_rows() {
            for (cell, column) in row.iter_mut().zip(&columns) {
                *cell = column.is_valid(i).then(|| column.value(i));
            }
            f(&row);
        }
    }
    Ok(())
}

/// Ids are kept as 64-bit hashes: the 16 million addresses of Spain would take gigabytes as
/// strings, and a collision among them is about one in a hundred thousand.
fn hash(text: &str) -> u64 {
    std::hash::BuildHasherDefault::<std::collections::hash_map::DefaultHasher>::default().hash_one(text)
}

pub fn report(canonical: &Path) -> Result<DataReport> {
    let path = canonical.join("fusion_report.json");
    let fusion: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).with_context(|| path.display().to_string())?)?;
    let mut r = DataReport::default();
    add_counts(&mut r.entities, &fusion["entities"]);
    let partitions = fusion["partitions"].as_array().cloned().unwrap_or_else(|| vec![fusion.clone()]);
    for p in &partitions {
        add_counts(&mut r.records, &p["records"]);
        add_counts(&mut r.inferred, &p["inferred"]);
        for key in ["duplicate_addresses", "duplicate_streets", "homonymous_streets"] {
            *r.duplicates.entry(key.into()).or_default() += p["metrics"][key].as_u64().unwrap_or(0) as usize;
        }
    }
    validate(canonical, &mut r.problems)?;
    Ok(r)
}

/// The checks of PROJECT.md §13.4 that the canonical dataset can answer by itself. Coordinates
/// outside their municipality cannot happen: the municipality is always assigned from them.
fn validate(dir: &Path, problems: &mut BTreeMap<String, usize>) -> Result<()> {
    let mut ids: HashSet<u64> = HashSet::new();
    let mut repeated = 0;
    let mut seen = |id: Option<&str>| {
        if let Some(id) = id
            && !ids.insert(hash(id))
        {
            repeated += 1;
        }
    };
    let (mut municipalities, mut postcodes, mut streets) = (HashSet::new(), HashSet::new(), HashSet::new());
    for_each(&dir.join("places.parquet"), &["public_id", "kind"], |row| {
        seen(row[0]);
        if row[1] == Some("municipality") {
            municipalities.insert(hash(row[0].unwrap_or_default()));
        }
    })?;
    for_each(&dir.join("postcodes.parquet"), &["public_id", "postcode"], |row| {
        seen(row[0]);
        postcodes.extend(row[1].map(hash));
    })?;
    // Every check is listed, also at 0, so that the report says what was checked.
    let mut count = |key: &str, problem: bool| *problems.entry(key.into()).or_default() += usize::from(problem);
    let known = |set: &HashSet<u64>, id: Option<&str>| id.is_none_or(|id| set.contains(&hash(id)));
    for_each(&dir.join("streets.parquet"), &["public_id", "municipality_id"], |row| {
        seen(row[0]);
        streets.insert(hash(row[0].unwrap_or_default()));
        count("streets_without_municipality", row[1].is_none());
        count("streets_in_an_unknown_municipality", !known(&municipalities, row[1]));
    })?;
    for_each(&dir.join("addresses.parquet"), &["public_id", "street_id", "municipality_id", "postcode"], |row| {
        seen(row[0]);
        count("addresses_without_street", !known(&streets, row[1]));
        count("addresses_without_municipality", row[2].is_none());
        count("addresses_in_an_unknown_municipality", !known(&municipalities, row[2]));
        count("addresses_with_an_unknown_postcode", !known(&postcodes, row[3]));
    })?;
    for_each(&dir.join("pois.parquet"), &["public_id", "municipality_id", "postcode"], |row| {
        seen(row[0]);
        count("pois_without_municipality", row[1].is_none());
        count("pois_in_an_unknown_municipality", !known(&municipalities, row[1]));
        count("pois_with_an_unknown_postcode", !known(&postcodes, row[2]));
    })?;
    problems.insert("repeated_public_ids".into(), repeated);
    Ok(())
}

/// A count that drops by more than this share is a regression: a source losing records, or a
/// kind of entity vanishing. Fusing better also lowers entity counts, but by far less.
pub const DROP: f64 = 0.02;

/// What got worse in `report` than in `baseline`: records or entities that dropped by more than
/// [`DROP`], and any more duplicates or integrity problems.
pub fn regressions(baseline: &DataReport, report: &DataReport) -> Vec<String> {
    let mut out = Vec::new();
    for (what, b, r) in [("records", &baseline.records, &report.records), ("entities", &baseline.entities, &report.entities)] {
        for (key, &before) in b {
            let now = r.get(key).copied().unwrap_or(0);
            if (now as f64) < before as f64 * (1.0 - DROP) {
                out.push(format!("{what} {key}: {before} -> {now}"));
            }
        }
    }
    for (what, b, r) in [("duplicates", &baseline.duplicates, &report.duplicates), ("problems", &baseline.problems, &report.problems)] {
        for (key, &now) in r {
            let before = b.get(key).copied().unwrap_or(0);
            if now > before {
                out.push(format!("{what} {key}: {before} -> {now}"));
            }
        }
    }
    out
}

pub fn table(baseline: &DataReport, report: &DataReport) -> String {
    let mut out = String::new();
    let sections = [
        ("entities", &baseline.entities, &report.entities),
        ("records", &baseline.records, &report.records),
        ("inferred", &baseline.inferred, &report.inferred),
        ("duplicates", &baseline.duplicates, &report.duplicates),
        ("problems", &baseline.problems, &report.problems),
    ];
    for (what, b, r) in sections {
        let keys: std::collections::BTreeSet<&String> = b.keys().chain(r.keys()).collect();
        for key in keys {
            let (before, now) = (b.get(key).copied().unwrap_or(0), r.get(key).copied().unwrap_or(0));
            let mark = if before == now { "" } else { "  *" };
            out.push_str(&format!("{:<48} {before:>12} {now:>12}{mark}\n", format!("{what} {key}")));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(records: usize, duplicates: usize, orphans: usize) -> DataReport {
        DataReport {
            records: BTreeMap::from([("addresses.osm".to_string(), records)]),
            duplicates: BTreeMap::from([("duplicate_streets".to_string(), duplicates)]),
            problems: BTreeMap::from([("addresses_without_street".to_string(), orphans)]),
            ..Default::default()
        }
    }

    #[test]
    fn lost_records_and_new_problems_are_regressions() {
        let base = report(1000, 3, 0);
        assert!(regressions(&base, &report(990, 3, 0)).is_empty(), "within the allowed drop");
        assert!(regressions(&base, &report(1200, 2, 0)).is_empty(), "more records, fewer duplicates");
        assert_eq!(regressions(&base, &report(700, 3, 0)), ["records addresses.osm: 1000 -> 700"]);
        assert_eq!(regressions(&base, &report(1000, 4, 0)), ["duplicates duplicate_streets: 3 -> 4"]);
        assert_eq!(regressions(&base, &report(1000, 3, 1)), ["problems addresses_without_street: 0 -> 1"]);
    }
}

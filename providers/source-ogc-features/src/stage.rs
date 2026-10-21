//! Staging table `addresses.parquet`: the columns of the common address schema
//! (`schemas/staging/addresses.md`) and any others the declaration adds, from the properties of
//! the features, then the coordinates.

use std::fs::{self, File};
use std::path::Path;
use std::sync::Arc;

use arrow_array::{ArrayRef, Int32Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use geko_core::{GekoError, Result};
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use parquet::format::KeyValue;

use crate::Column;
use crate::api::{Feature, Page};

const BATCH_ROWS: usize = 200_000;

fn err(e: impl std::fmt::Display) -> GekoError {
    GekoError::Data(format!("cannot write the address staging: {e}"))
}

fn schema(columns: &[Column]) -> SchemaRef {
    let mut fields = vec![Field::new("source_id", DataType::Utf8, false)];
    fields.extend(columns.iter().map(|c| Field::new(&c.name, DataType::Utf8, true)));
    fields.push(Field::new("lat_e7", DataType::Int32, false));
    fields.push(Field::new("lon_e7", DataType::Int32, false));
    Arc::new(Schema::new(fields))
}

/// A property as text: a string as it is, a number or a boolean written out, null as nothing.
fn text(value: Option<&serde_json::Value>) -> Option<String> {
    match value? {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

struct Rows<'a> {
    columns: &'a [Column],
    source_id: Vec<String>,
    values: Vec<Vec<Option<String>>>,
    lat_e7: Vec<i32>,
    lon_e7: Vec<i32>,
}

impl<'a> Rows<'a> {
    fn new(columns: &'a [Column]) -> Self {
        Self { columns, source_id: Vec::new(), values: vec![Vec::new(); columns.len()], lat_e7: Vec::new(), lon_e7: Vec::new() }
    }

    fn len(&self) -> usize {
        self.source_id.len()
    }

    fn push(&mut self, f: Feature) -> bool {
        let Some([lon, lat]) = f.geometry.as_ref().and_then(|g| <[f64; 2]>::try_from(g.coordinates.as_slice()).ok()) else {
            return false;
        };
        if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
            return false;
        }
        let Some(id) = text(Some(&f.id)) else { return false };
        self.source_id.push(id);
        for (column, values) in self.columns.iter().zip(&mut self.values) {
            let value = text(f.properties.get(&column.property)).filter(|v| !column.null_values.contains(v));
            values.push(value);
        }
        self.lat_e7.push((lat * 1e7).round() as i32);
        self.lon_e7.push((lon * 1e7).round() as i32);
        true
    }

    fn take_batch(&mut self, schema: &SchemaRef) -> Result<RecordBatch> {
        let mut arrays: Vec<ArrayRef> = vec![Arc::new(StringArray::from(std::mem::take(&mut self.source_id)))];
        for values in &mut self.values {
            arrays.push(Arc::new(StringArray::from(std::mem::take(values))));
        }
        arrays.push(Arc::new(Int32Array::from(std::mem::take(&mut self.lat_e7))));
        arrays.push(Arc::new(Int32Array::from(std::mem::take(&mut self.lon_e7))));
        RecordBatch::try_new(Arc::clone(schema), arrays).map_err(err)
    }
}

/// Streams pages into `dir/addresses.parquet` in batches, with `schema_version` as its
/// `geko.schema`. Returns the number of rows.
pub fn write_addresses(
    dir: &Path,
    schema_version: &str,
    columns: &[Column],
    pages: impl Iterator<Item = Result<Page>>,
) -> Result<u64> {
    fs::create_dir_all(dir)?;
    let schema = schema(columns);
    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::default()))
        .set_key_value_metadata(Some(vec![KeyValue::new("geko.schema".into(), schema_version.to_string())]))
        .build();
    let path = dir.join("addresses.parquet");
    let tmp = path.with_extension("parquet.tmp");
    let mut writer = ArrowWriter::try_new(File::create(&tmp)?, Arc::clone(&schema), Some(props)).map_err(err)?;
    let mut rows = Rows::new(columns);
    let mut total = 0u64;
    for page in pages {
        for f in page?.features {
            if rows.push(f) {
                total += 1;
            }
        }
        if rows.len() >= BATCH_ROWS {
            writer.write(&rows.take_batch(&schema)?).map_err(err)?;
        }
    }
    if rows.len() > 0 {
        writer.write(&rows.take_batch(&schema)?).map_err(err)?;
    }
    writer.close().map_err(err)?;
    fs::rename(&tmp, &path)?;
    Ok(total)
}

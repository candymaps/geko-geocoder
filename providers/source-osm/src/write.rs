//! Staging tables in Parquet (PROJECT.md §5.2). Schemas are explicit so that every
//! partition has exactly the same contract, documented in `schemas/staging/osm.md`.

use std::fs::{self, File};
use std::path::Path;
use std::sync::Arc;

use arrow_array::{ArrayRef, BinaryArray, Int32Array, Int64Array, RecordBatch, StringArray, UInt8Array};
use arrow_schema::{DataType, Field, Schema};
use geko_core::{GekoError, Result};
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use parquet::format::KeyValue;

use geko_geom::wkb;
use crate::read::{Common, Extracted};

pub const SCHEMA_VERSION: &str = "osm-staging/1";

fn err(e: impl std::fmt::Display) -> GekoError {
    GekoError::Data(format!("cannot write staging Parquet: {e}"))
}

fn common_fields() -> Vec<Field> {
    vec![
        Field::new("osm_type", DataType::Utf8, false),
        Field::new("osm_id", DataType::Int64, false),
        Field::new("lat_e7", DataType::Int32, false),
        Field::new("lon_e7", DataType::Int32, false),
        Field::new("name", DataType::Utf8, true),
        Field::new("names", DataType::Utf8, false),
        Field::new("wikidata", DataType::Utf8, true),
    ]
}

fn common_columns<'a>(rows: impl Iterator<Item = &'a Common> + Clone) -> Vec<ArrayRef> {
    vec![
        Arc::new(StringArray::from_iter_values(rows.clone().map(|c| c.osm_type.as_str()))),
        Arc::new(Int64Array::from_iter_values(rows.clone().map(|c| c.osm_id))),
        Arc::new(Int32Array::from_iter_values(rows.clone().map(|c| c.point.1))),
        Arc::new(Int32Array::from_iter_values(rows.clone().map(|c| c.point.0))),
        Arc::new(StringArray::from_iter(rows.clone().map(|c| c.name.as_deref()))),
        Arc::new(StringArray::from_iter_values(
            rows.clone().map(|c| serde_json::to_string(&c.names).expect("string map serializes")),
        )),
        Arc::new(StringArray::from_iter(rows.map(|c| c.wikidata.as_deref()))),
    ]
}

fn opt_str<'a>(values: impl Iterator<Item = Option<&'a str>>) -> ArrayRef {
    Arc::new(StringArray::from_iter(values))
}

fn write_table(path: &Path, mut fields: Vec<Field>, extra_fields: Vec<Field>, mut columns: Vec<ArrayRef>, extra: Vec<ArrayRef>) -> Result<u64> {
    fields.extend(extra_fields);
    columns.extend(extra);
    let schema = Arc::new(Schema::new(fields));
    let batch = RecordBatch::try_new(Arc::clone(&schema), columns).map_err(err)?;
    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::default()))
        .set_key_value_metadata(Some(vec![KeyValue::new("geko.schema".into(), SCHEMA_VERSION.to_string())]))
        .build();
    let tmp = path.with_extension("parquet.tmp");
    let mut writer = ArrowWriter::try_new(File::create(&tmp)?, schema, Some(props)).map_err(err)?;
    writer.write(&batch).map_err(err)?;
    writer.close().map_err(err)?;
    fs::rename(&tmp, path)?;
    Ok(batch.num_rows() as u64)
}

pub fn write_all(dir: &Path, data: &Extracted) -> Result<u64> {
    fs::create_dir_all(dir)?;
    let mut rows = 0;

    let s = &data.streets;
    rows += write_table(
        &dir.join("streets.parquet"),
        common_fields(),
        vec![Field::new("highway", DataType::Utf8, false), Field::new("geom", DataType::Binary, false)],
        common_columns(s.iter().map(|r| &r.common)),
        vec![
            Arc::new(StringArray::from_iter_values(s.iter().map(|r| r.highway.as_str()))),
            Arc::new(BinaryArray::from_iter_values(s.iter().map(|r| wkb::line_string(&r.geom)))),
        ],
    )?;

    let a = &data.addresses;
    rows += write_table(
        &dir.join("addresses.parquet"),
        common_fields(),
        vec![
            Field::new("housenumber", DataType::Utf8, false),
            Field::new("street", DataType::Utf8, true),
            Field::new("place", DataType::Utf8, true),
            Field::new("postcode", DataType::Utf8, true),
            Field::new("city", DataType::Utf8, true),
        ],
        common_columns(a.iter().map(|r| &r.common)),
        vec![
            Arc::new(StringArray::from_iter_values(a.iter().map(|r| r.housenumber.as_str()))),
            opt_str(a.iter().map(|r| r.street.as_deref())),
            opt_str(a.iter().map(|r| r.place.as_deref())),
            opt_str(a.iter().map(|r| r.postcode.as_deref())),
            opt_str(a.iter().map(|r| r.city.as_deref())),
        ],
    )?;

    let p = &data.pois;
    rows += write_table(
        &dir.join("pois.parquet"),
        common_fields(),
        vec![
            Field::new("category", DataType::Utf8, false),
            Field::new("phone", DataType::Utf8, true),
            Field::new("website", DataType::Utf8, true),
        ],
        common_columns(p.iter().map(|r| &r.common)),
        vec![
            Arc::new(StringArray::from_iter_values(p.iter().map(|r| r.category.as_str()))),
            opt_str(p.iter().map(|r| r.phone.as_deref())),
            opt_str(p.iter().map(|r| r.website.as_deref())),
        ],
    )?;

    let pl = &data.places;
    rows += write_table(
        &dir.join("places.parquet"),
        common_fields(),
        vec![Field::new("place", DataType::Utf8, false), Field::new("population", DataType::Int64, true)],
        common_columns(pl.iter().map(|r| &r.common)),
        vec![
            Arc::new(StringArray::from_iter_values(pl.iter().map(|r| r.place.as_str()))),
            Arc::new(Int64Array::from_iter(pl.iter().map(|r| r.population))),
        ],
    )?;

    let ad = &data.admins;
    rows += write_table(
        &dir.join("admins.parquet"),
        common_fields(),
        vec![
            Field::new("admin_level", DataType::UInt8, false),
            Field::new("iso3166_1", DataType::Utf8, true),
            Field::new("iso3166_2", DataType::Utf8, true),
            Field::new("official_ref", DataType::Utf8, true),
            Field::new("population", DataType::Int64, true),
            Field::new("geom", DataType::Binary, false),
        ],
        common_columns(ad.iter().map(|r| &r.common)),
        vec![
            Arc::new(UInt8Array::from_iter_values(ad.iter().map(|r| r.admin_level))),
            opt_str(ad.iter().map(|r| r.iso3166_1.as_deref())),
            opt_str(ad.iter().map(|r| r.iso3166_2.as_deref())),
            opt_str(ad.iter().map(|r| r.official_ref.as_deref())),
            Arc::new(Int64Array::from_iter(ad.iter().map(|r| r.population))),
            Arc::new(BinaryArray::from_iter_values(ad.iter().map(|r| wkb::multi_polygon(&r.polygons)))),
        ],
    )?;

    Ok(rows)
}

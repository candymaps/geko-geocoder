//! Canonical tables (schema `canonical/1`, documented in `schemas/canonical.md`).

use std::path::Path;
use std::sync::Arc;

use arrow_array::{ArrayRef, BinaryArray, BooleanArray, Int32Array, Int64Array, StringArray, UInt16Array, UInt32Array, UInt64Array};
use arrow_schema::{DataType, Field, Schema};
use geko_core::Result;
use geko_geom::wkb;
use serde_json::json;

use super::Fused;
use super::ids::{Ids, Importance, LocalIds, POSTCODE_IMPORTANCE};
use super::io::write_parquet;

pub const SCHEMA_VERSION: &str = "canonical/1";

fn utf8(name: &str, nullable: bool) -> Field {
    Field::new(name, DataType::Utf8, nullable)
}

fn strings<'a>(v: impl Iterator<Item = &'a str>) -> ArrayRef {
    Arc::new(StringArray::from_iter_values(v))
}

fn opt_strings<'a>(v: impl Iterator<Item = Option<&'a str>>) -> ArrayRef {
    Arc::new(StringArray::from_iter(v))
}

fn opt_owned(v: impl Iterator<Item = Option<String>>) -> ArrayRef {
    Arc::new(StringArray::from_iter(v))
}

fn owned_strings(v: impl Iterator<Item = String>) -> ArrayRef {
    Arc::new(StringArray::from_iter_values(v))
}

fn coords(points: impl Iterator<Item = (i32, i32)> + Clone) -> [ArrayRef; 2] {
    [
        Arc::new(Int32Array::from_iter_values(points.clone().map(|p| p.1))),
        Arc::new(Int32Array::from_iter_values(points.map(|p| p.0))),
    ]
}

fn coord_fields() -> [Field; 2] {
    [Field::new("lat_e7", DataType::Int32, false), Field::new("lon_e7", DataType::Int32, false)]
}

fn join_sources<'a>(s: impl Iterator<Item = &'a &'static str>) -> String {
    s.copied().collect::<Vec<_>>().join(",")
}

pub fn write(dir: &Path, f: &Fused, ids: &Ids, imp: &Importance, local: &LocalIds) -> Result<u64> {
    let mut rows = 0;
    let admin_place: rustc_hash::FxHashMap<usize, usize> =
        f.places.iter().enumerate().filter_map(|(i, p)| p.admin.map(|a| (a, i))).collect();
    let admin_id = |a: Option<usize>| a.and_then(|a| admin_place.get(&a)).map(|&i| ids.places[i].as_str());

    // places
    let p = &f.places;
    let mut fields = vec![utf8("public_id", false), Field::new("local_id", DataType::UInt32, false), utf8("kind", false), utf8("name", false), utf8("names", false)];
    fields.extend(coord_fields());
    fields.extend([
        Field::new("population", DataType::Int64, true),
        utf8("wikidata", true),
        utf8("official_ref", true),
        utf8("iso3166_2", true),
        utf8("place_type", true),
        utf8("municipality_id", true),
        utf8("province_id", true),
        utf8("region_id", true),
        utf8("postcodes", true),
        utf8("postcode", true),
        utf8("postcode_source", true),
        Field::new("importance", DataType::UInt16, false),
        utf8("sources", false),
        utf8("source_ids", false),
        Field::new("geom", DataType::Binary, true),
    ]);
    let mut cols = vec![
        strings(ids.places.iter().map(String::as_str)),
        Arc::new(UInt32Array::from(local.places.clone())) as ArrayRef,
        strings(p.iter().map(|x| x.kind.as_str())),
        strings(p.iter().map(|x| x.name.as_str())),
        owned_strings(p.iter().map(|x| json!(x.names).to_string())),
    ];
    cols.extend(coords(p.iter().map(|x| x.point)));
    cols.extend([
        Arc::new(Int64Array::from_iter(p.iter().map(|x| x.population))) as ArrayRef,
        opt_strings(p.iter().map(|x| x.wikidata.as_deref())),
        opt_strings(p.iter().map(|x| x.official_ref.as_deref())),
        opt_strings(p.iter().map(|x| x.iso3166_2.as_deref())),
        opt_strings(p.iter().map(|x| x.place_type.as_deref())),
        opt_strings(p.iter().map(|x| admin_id(x.chain.municipality))),
        opt_strings(p.iter().map(|x| admin_id(x.chain.province))),
        opt_strings(p.iter().map(|x| admin_id(x.chain.region))),
        opt_owned(p.iter().map(|x| (!x.postcodes.is_empty()).then(|| json!(x.postcodes).to_string()))),
        opt_strings(p.iter().map(|x| x.postcode.as_deref())),
        opt_strings(p.iter().map(|x| x.postcode.as_ref().map(|_| "inferred"))),
        Arc::new(UInt16Array::from(imp.places.clone())),
        owned_strings(p.iter().map(|x| join_sources(x.sources.iter()))),
        owned_strings(p.iter().map(|x| json!(x.source_ids).to_string())),
        Arc::new(BinaryArray::from_iter(p.iter().map(|x| {
            x.admin.map(|a| {
                let polygons: Vec<geko_geom::Polygon> = f.admins[a]
                    .rings
                    .iter()
                    .map(|r| geko_geom::Polygon { outer: r.clone(), inners: Vec::new() })
                    .collect();
                wkb::multi_polygon(&polygons)
            })
        }))),
    ]);
    rows += write_parquet(&dir.join("places.parquet"), Arc::new(Schema::new(fields)), cols, SCHEMA_VERSION)?;

    // streets
    let s = &f.streets;
    let mut addr_count = vec![0u32; s.len()];
    for a in &f.addresses {
        addr_count[a.street] += 1;
    }
    let mut fields = vec![
        utf8("public_id", false),
        Field::new("local_id", DataType::UInt32, false),
        utf8("name", false),
        utf8("type_code", true),
        utf8("core", false),
        utf8("names", false),
        utf8("aliases", false),
        utf8("municipality_id", true),
        utf8("locality", true),
    ];
    fields.extend(coord_fields());
    fields.extend([
        utf8("highway", true),
        utf8("postcodes", true),
        Field::new("address_count", DataType::UInt32, false),
        Field::new("importance", DataType::UInt16, false),
        utf8("sources", false),
        utf8("source_ids", false),
        Field::new("geom", DataType::Binary, true),
    ]);
    let rep: Vec<(i32, i32)> = s.iter().map(|x| x.representative_point().unwrap_or((0, 0))).collect();
    let mut cols = vec![
        strings(ids.streets.iter().map(String::as_str)),
        Arc::new(UInt32Array::from(local.streets.clone())) as ArrayRef,
        strings(s.iter().map(|x| x.display_name.as_str())),
        opt_strings(s.iter().map(|x| x.type_code)),
        strings(s.iter().map(|x| x.core.as_str())),
        owned_strings(s.iter().map(|x| json!(x.names).to_string())),
        owned_strings(s.iter().map(|x| json!(x.aliases).to_string())),
        opt_strings(s.iter().map(|x| admin_id(x.municipality))),
        opt_strings(s.iter().map(|x| x.locality.as_deref())),
    ];
    cols.extend(coords(rep.iter().copied()));
    cols.extend([
        opt_strings(s.iter().map(|x| x.highway.as_deref())),
        opt_owned(f.street_postcodes.iter().map(|pcs| (!pcs.is_empty()).then(|| json!(pcs).to_string()))),
        Arc::new(UInt32Array::from(addr_count)) as ArrayRef,
        Arc::new(UInt16Array::from(imp.streets.clone())),
        owned_strings(s.iter().map(|x| join_sources(x.sources.iter()))),
        owned_strings(s.iter().map(|x| json!(x.source_ids).to_string())),
        Arc::new(BinaryArray::from_iter(s.iter().map(|x| (!x.lines.is_empty()).then(|| wkb::multi_line_string(&x.lines))))),
    ]);
    rows += write_parquet(&dir.join("streets.parquet"), Arc::new(Schema::new(fields)), cols, SCHEMA_VERSION)?;

    // addresses
    let a = &f.addresses;
    let mut fields = vec![
        utf8("public_id", false),
        Field::new("local_id", DataType::UInt32, false),
        utf8("street_id", false),
        utf8("street_name", false),
        Field::new("number", DataType::UInt32, true),
        utf8("suffix", true),
        Field::new("range_end", DataType::UInt32, true),
        utf8("label", false),
    ];
    fields.extend(coord_fields());
    fields.extend([
        utf8("postcode", true),
        utf8("postcode_source", true),
        utf8("municipality_id", true),
        Field::new("importance", DataType::UInt16, false),
        utf8("sources", false),
        utf8("source_ids", false),
    ]);
    let mut cols = vec![
        strings(ids.addresses.iter().map(String::as_str)),
        Arc::new(UInt32Array::from(local.addresses.clone())) as ArrayRef,
        strings(a.iter().map(|x| ids.streets[x.street].as_str())),
        strings(a.iter().map(|x| s[x.street].display_name.as_str())),
        Arc::new(UInt32Array::from_iter(a.iter().map(|x| x.number.as_ref().map(|n| n.number)))) as ArrayRef,
        opt_strings(a.iter().map(|x| x.number.as_ref().and_then(|n| n.suffix.as_deref()))),
        Arc::new(UInt32Array::from_iter(a.iter().map(|x| x.number.as_ref().and_then(|n| n.range_end)))),
        owned_strings(a.iter().map(|x| x.number.as_ref().map_or_else(|| "s/n".to_string(), |n| n.label()))),
    ];
    cols.extend(coords(a.iter().map(|x| x.point)));
    cols.extend([
        opt_strings(a.iter().map(|x| x.postcode.as_deref())),
        opt_strings(a.iter().map(|x| x.postcode_source.map(|s| s.as_str()))),
        opt_strings(a.iter().map(|x| admin_id(s[x.street].municipality))),
        Arc::new(UInt16Array::from(imp.addresses.clone())) as ArrayRef,
        owned_strings(a.iter().map(|x| join_sources(x.sources.iter()))),
        owned_strings(a.iter().map(|x| json!(x.source_ids).to_string())),
    ]);
    rows += write_parquet(&dir.join("addresses.parquet"), Arc::new(Schema::new(fields)), cols, SCHEMA_VERSION)?;

    // pois
    let o = &f.pois;
    let mut fields = vec![utf8("public_id", false), Field::new("local_id", DataType::UInt32, false), utf8("name", false), utf8("names", false), utf8("category", false)];
    fields.extend(coord_fields());
    fields.extend([
        utf8("wikidata", true),
        utf8("municipality_id", true),
        utf8("postcode", true),
        utf8("postcode_source", true),
        utf8("address_id", true),
        Field::new("address_inferred", DataType::Boolean, false),
        Field::new("importance", DataType::UInt16, false),
        utf8("sources", false),
        utf8("source_ids", false),
    ]);
    let mut cols = vec![
        strings(ids.pois.iter().map(String::as_str)),
        Arc::new(UInt32Array::from(local.pois.clone())) as ArrayRef,
        strings(o.iter().map(|x| x.name.as_str())),
        owned_strings(o.iter().map(|x| json!(x.names).to_string())),
        strings(o.iter().map(|x| x.category.as_str())),
    ];
    cols.extend(coords(o.iter().map(|x| x.point)));
    cols.extend([
        opt_strings(o.iter().map(|x| x.wikidata.as_deref())),
        opt_strings(o.iter().map(|x| admin_id(x.chain.municipality))),
        opt_strings(o.iter().map(|x| x.postcode.as_deref())),
        opt_strings(o.iter().map(|x| x.postcode_source.map(|s| s.as_str()))),
        opt_strings(o.iter().map(|x| x.address.map(|i| ids.addresses[i].as_str()))),
        Arc::new(BooleanArray::from_iter(o.iter().map(|x| Some(x.address.is_some())))) as ArrayRef,
        Arc::new(UInt16Array::from(imp.pois.clone())),
        owned_strings(o.iter().map(|x| join_sources(x.sources.iter()))),
        owned_strings(o.iter().map(|x| json!(x.source_ids).to_string())),
    ]);
    rows += write_parquet(&dir.join("pois.parquet"), Arc::new(Schema::new(fields)), cols, SCHEMA_VERSION)?;

    // postcodes
    let pc = &f.postcodes;
    let mut fields = vec![utf8("public_id", false), Field::new("local_id", DataType::UInt32, false), utf8("postcode", false)];
    fields.extend(coord_fields());
    fields.extend([
        utf8("municipality_id", true),
        Field::new("address_count", DataType::UInt32, false),
        Field::new("importance", DataType::UInt16, false),
    ]);
    let mut cols = vec![
        owned_strings(pc.iter().map(|x| format!("geko:postcode:{}-{}", f.country.lowercase(), x.postcode))),
        Arc::new(UInt32Array::from(local.postcodes.clone())) as ArrayRef,
        strings(pc.iter().map(|x| x.postcode.as_str())),
    ];
    cols.extend(coords(pc.iter().map(|x| x.point)));
    cols.extend([
        opt_strings(pc.iter().map(|x| admin_id(x.municipality))),
        Arc::new(UInt32Array::from_iter_values(pc.iter().map(|x| x.address_count))) as ArrayRef,
        Arc::new(UInt16Array::from(vec![POSTCODE_IMPORTANCE; pc.len()])),
    ]);
    rows += write_parquet(&dir.join("postcodes.parquet"), Arc::new(Schema::new(fields)), cols, SCHEMA_VERSION)?;

    // postcode_cells
    let mut cells: Vec<(u64, &str)> = f.zones.cells.iter().map(|(c, p)| (u64::from(*c), p.as_str())).collect();
    cells.sort();
    rows += write_parquet(
        &dir.join("postcode_cells.parquet"),
        Arc::new(Schema::new(vec![Field::new("cell", DataType::UInt64, false), utf8("postcode", false)])),
        vec![
            Arc::new(UInt64Array::from_iter_values(cells.iter().map(|c| c.0))),
            strings(cells.iter().map(|c| c.1)),
        ],
        SCHEMA_VERSION,
    )?;

    // conflicts
    let c = &f.conflicts;
    rows += write_parquet(
        &dir.join("conflicts.parquet"),
        Arc::new(Schema::new(vec![
            utf8("entity_kind", false),
            utf8("public_id", false),
            utf8("attribute", false),
            utf8("chosen", false),
            utf8("chosen_source", false),
            utf8("discarded", false),
            utf8("discarded_source", false),
        ])),
        vec![
            strings(c.iter().map(|x| x.entity_kind)),
            strings(c.iter().map(|x| ids.addresses[x.entity].as_str())),
            strings(c.iter().map(|x| x.attribute)),
            strings(c.iter().map(|x| x.chosen.as_str())),
            strings(c.iter().map(|x| x.chosen_source)),
            strings(c.iter().map(|x| x.discarded.as_str())),
            strings(c.iter().map(|x| x.discarded_source)),
        ],
        SCHEMA_VERSION,
    )?;
    Ok(rows)
}

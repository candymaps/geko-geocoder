//! On-disk layout of a `geocode-fst` segment.
//!
//! ```text
//! manifest.json
//! hot/   words.fst names.fst prefixes.fst postings.bin posting_start.bin posting_len.bin
//!        kind.bin importance.bin lat.bin lon.bin muni.bin ntok.bin ltok.bin addr_start.bin addr_len.bin
//!        a_number.bin a_suffix.bin a_lat.bin a_lon.bin a_postcode.bin a_street.bin
//! cold/  json.{bin,idx} pid.{bin,idx} name.{bin,idx}      searchable entities only
//!        a_tail.bin a_label.bin a_meta.bin atail.{bin,idx} alabel.{bin,idx}   addresses
//!        ids.fst
//! ```
//!
//! Addresses are 86 % of the entities and their properties repeat their street's: instead of a
//! pre-serialized fragment each, they store indices into small tables (id tail, label, metadata)
//! and their fragment is rebuilt on demand, identical to a pre-serialized one.

use std::path::Path;

use geko_core::model::PlaceKind;
use geko_core::ports::IndexFormat;
use geko_core::{GekoError, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

pub const PROVIDER: &str = "geocode-fst";
pub const VERSION: u32 = 4;
pub const NO_MUNICIPALITY: u32 = u32::MAX;
/// Prefixes up to this length use precomputed top results (PROJECT.md §9.8).
pub const PREFIX_LEN: usize = 4;
pub const PREFIX_TOP_K: usize = 64;

pub fn format() -> IndexFormat {
    IndexFormat { provider: PROVIDER.into(), version: VERSION }
}

pub fn kind_code(kind: PlaceKind) -> u8 {
    PlaceKind::ALL.iter().position(|k| *k == kind).expect("every kind is in ALL") as u8
}

pub fn kind_of(code: u8) -> PlaceKind {
    PlaceKind::ALL.get(code as usize).copied().unwrap_or(PlaceKind::Poi)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FormatJson {
    pub provider: String,
    pub version: u32,
}

impl From<IndexFormat> for FormatJson {
    fn from(f: IndexFormat) -> Self {
        Self { provider: f.provider, version: f.version }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub format: FormatJson,
    pub country: String,
    pub docs: u32,
    pub addresses: u32,
    pub words: u32,
    pub suffixes: Vec<String>,
    pub postcodes: Vec<String>,
    /// Street type codes; `stype.bin` holds 1 + index, 0 for none.
    pub street_types: Vec<String>,
    /// Distinct municipality and source properties of addresses; `a_meta.bin` indexes them.
    pub address_meta: Vec<AddressMeta>,
}

impl Manifest {
    pub fn write(&self, dir: &Path) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| GekoError::Other(e.into()))?;
        crate::store::write_bytes(&dir.join("manifest.json"), &bytes)
    }

    pub fn read(dir: &Path) -> Result<Self> {
        let path = dir.join("manifest.json");
        let bytes = std::fs::read(&path).map_err(|e| GekoError::Data(format!("{}: {e}", path.display())))?;
        let m: Manifest = serde_json::from_slice(&bytes).map_err(|e| GekoError::Data(format!("{}: {e}", path.display())))?;
        if m.format != FormatJson::from(format()) {
            return Err(GekoError::Incompatible(format!(
                "{} was built by {} v{}, this binary reads {PROVIDER} v{VERSION}",
                dir.display(),
                m.format.provider,
                m.format.version
            )));
        }
        Ok(m)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AddressMeta {
    pub municipality_id: Option<String>,
    pub municipality: Option<String>,
    /// Properties of the address itself (sources, postcode source).
    pub props: Map<String, Value>,
}

/// A GeoJSON `properties` object without its braces and without null values, ready to be
/// spliced into a response.
pub fn fragment(props: Map<String, Value>) -> Vec<u8> {
    let props: Map<String, Value> = props.into_iter().filter(|(_, v)| !v.is_null()).collect();
    let s = Value::Object(props).to_string();
    s.as_bytes()[1..s.len() - 1].to_vec()
}

/// An address id is its street's key plus a tail: `geko:address:es-07040-cl-sant-miquel:30`.
pub fn address_public_id(street_id: &str, tail: &str) -> String {
    format!("geko:address:{}:{tail}", street_id.strip_prefix("geko:street:").unwrap_or(street_id))
}

/// Whether `public_id` is `address_public_id(street_id, tail)`, without building it.
pub fn is_address_id(public_id: &str, street_id: &str, tail: &str) -> bool {
    let (Some(id), Some(key)) = (public_id.strip_prefix("geko:address:"), street_id.strip_prefix("geko:street:")) else { return false };
    id.strip_prefix(key).and_then(|rest| rest.strip_prefix(':')) == Some(tail)
}

/// The street id and tail of an address id: `geko:address:es-1-cl-x:30` is `geko:street:es-1-cl-x`, `30`.
pub fn split_address_id(public_id: &str) -> Option<(String, &str)> {
    let (key, tail) = public_id.strip_prefix("geko:address:")?.rsplit_once(':')?;
    Some((format!("geko:street:{key}"), tail))
}

/// Everything an address's properties are made of.
pub struct AddressView<'a> {
    pub public_id: &'a str,
    pub street_id: &'a str,
    pub street_name: &'a str,
    pub label: &'a str,
    pub postcode: Option<&'a str>,
    pub meta: &'a AddressMeta,
}

impl AddressView<'_> {
    pub fn name(&self) -> String {
        format!("{} {}", self.street_name, self.label)
    }

    pub fn fragment(&self) -> Vec<u8> {
        let name = self.name();
        let place = [self.postcode, self.meta.municipality.as_deref()].into_iter().flatten().collect::<Vec<_>>().join(" ");
        let label = if place.is_empty() { name.clone() } else { format!("{name}, {place}") };
        let mut props = self.meta.props.clone();
        props.insert("gid".into(), json!(self.public_id));
        props.insert("layer".into(), json!("address"));
        props.insert("name".into(), json!(name));
        props.insert("label".into(), json!(label));
        props.insert("street".into(), json!(self.street_name));
        props.insert("street_id".into(), json!(self.street_id));
        props.insert("housenumber".into(), json!(self.label));
        props.insert("postcode".into(), json!(self.postcode));
        props.insert("municipality".into(), json!(self.meta.municipality));
        props.insert("municipality_id".into(), json!(self.meta.municipality_id));
        fragment(props)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragments_drop_braces_and_nulls() {
        let mut m = Map::new();
        m.insert("a".into(), json!(1));
        m.insert("b".into(), Value::Null);
        assert_eq!(fragment(m), br#""a":1"#);
    }

    #[test]
    fn address_fragment() {
        let meta = AddressMeta {
            municipality_id: Some("geko:municipality:es-07040".into()),
            municipality: Some("Palma".into()),
            props: Map::from_iter([("sources".to_string(), json!(["osm"]))]),
        };
        let view = AddressView {
            public_id: &address_public_id("geko:street:es-07040-cl-sant-miquel", "30"),
            street_id: "geko:street:es-07040-cl-sant-miquel",
            street_name: "Carrer de Sant Miquel",
            label: "30",
            postcode: None,
            meta: &meta,
        };
        let props: Value = serde_json::from_slice(&[b"{", view.fragment().as_slice(), b"}"].concat()).unwrap();
        assert_eq!(props["gid"], "geko:address:es-07040-cl-sant-miquel:30");
        assert_eq!(props["label"], "Carrer de Sant Miquel 30, Palma");
        assert!(props.get("postcode").is_none());
    }

    #[test]
    fn address_ids_derive_from_streets() {
        let street = "geko:street:es-07040-cl-sant-miquel";
        assert!(is_address_id("geko:address:es-07040-cl-sant-miquel:30", street, "30"));
        assert!(!is_address_id("geko:address:es-07040-cl-sant-miquel:30", street, "3"));
        assert!(!is_address_id("geko:address:es-07040-cl-sant-miquel-2:30", street, "30"));
        assert_eq!(split_address_id("geko:address:es-07040-cl-sant-miquel:10-2"), Some((street.to_owned(), "10-2")));
        assert_eq!(split_address_id("geko:street:es-07040-cl-sant-miquel"), None);
    }
}

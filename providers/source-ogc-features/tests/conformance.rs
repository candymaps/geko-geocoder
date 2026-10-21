use std::io::Write;

use geko_core::country::RegionCode;
use geko_core::ports::RawDir;
use geko_core::scope::Partition;
use source_ogc_features::{OgcFeaturesConfig, OgcFeaturesSource};

/// A downloaded province as the API leaves it: one zstd page and the marker of a complete
/// download (`raw/cartociudad/07/`).
fn raw_fixture(raw: &std::path::Path) {
    let dir = raw.join("07");
    std::fs::create_dir_all(&dir).unwrap();
    let features: Vec<String> = (1..=5)
        .map(|n| {
            format!(
                r#"{{"type":"Feature","id":"AD_ADDRESS_PPK_04.07.PML_07002000000{n}","properties":{{
                "locator_designator_addressNumber":"{n}","locator_designator_addressNumberExtension":null,
                "component_PostalDescriptor":"07210","component_ThoroughfareName":"CALLE MAJOR",
                "component_AddressAreaName":"Algaida","component_AdminUnitName_4":"Algaida",
                "component_AdminUnitName_3":"Illes Balears","dataProvider":"IDEIB","beginLifespanVersion":"2026-06-15"}},
                "geometry":{{"type":"Point","coordinates":[2.89{n},39.56]}}}}"#
            )
        })
        .collect();
    let page = format!(r#"{{"type":"FeatureCollection","numberMatched":5,"features":[{}]}}"#, features.join(","));
    let mut encoder = zstd::Encoder::new(std::fs::File::create(dir.join("07210-0000000.json.zst")).unwrap(), 3).unwrap();
    encoder.write_all(page.as_bytes()).unwrap();
    encoder.finish().unwrap();
    std::fs::write(dir.join("_complete.json"), r#"{"fetched_at_unix":1792000000,"addresses":5,"pages":1}"#).unwrap();
}

#[test]
fn conformance() {
    // CartoCiudad, as the pack of Spain declares it.
    let pack = geko_country_pack::repo("ES");
    let config: OgcFeaturesConfig = toml::Value::Table(pack.sources["cartociudad"].clone()).try_into().unwrap();
    let source = OgcFeaturesSource::new("cartociudad", pack.country, config, pack.regions.clone()).unwrap();
    let es = pack.country;
    geko_conformance::source::check_metadata(&source, es);

    let tmp = tempfile::tempdir().unwrap();
    let raw = RawDir { path: tmp.path().join("cartociudad") };
    raw_fixture(&raw.path);
    let partition = Partition { country: es, region: Some(RegionCode::new("ES-PM").unwrap()) };
    geko_conformance::source::check_stage(&source, &partition, &raw);
}

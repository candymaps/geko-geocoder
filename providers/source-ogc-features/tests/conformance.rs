use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use geko_core::GekoError;
use geko_core::country::{CountryCode, RegionCode};
use geko_core::ports::{DownloadOutcome, DownloadRequest, Downloader, RawDir, SourceProvider};
use geko_core::scope::{Partition, Scope};
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

/// CartoCiudad, as the pack of Spain declares it.
fn cartociudad() -> (OgcFeaturesSource, CountryCode) {
    let pack = geko_country_pack::repo("ES");
    let config: OgcFeaturesConfig = toml::Value::Table(pack.sources["cartociudad"].clone()).try_into().unwrap();
    (OgcFeaturesSource::new("cartociudad", pack.country, config, pack.regions.clone()).unwrap(), pack.country)
}

/// A service that is down, counting what it is asked.
#[derive(Default)]
struct Down {
    asked: AtomicUsize,
    restoring: bool,
}

impl Downloader for Down {
    fn fetch(&self, req: &DownloadRequest, _: &Path) -> geko_core::Result<DownloadOutcome> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        Err(GekoError::Data(format!("GET {} returned HTTP 503", req.url)))
    }

    fn keeps_removed(&self) -> bool {
        !self.restoring
    }
}

fn scope(country: CountryCode, regions: &[&str]) -> Scope {
    let mut scope = Scope::default();
    scope.add(country, regions.iter().map(|r| RegionCode::new(r).unwrap()).collect()).unwrap();
    scope
}

/// The same complete download under another province.
fn copy_fixture(raw: &Path, to: &str) {
    let dir = raw.join(to);
    std::fs::create_dir_all(&dir).unwrap();
    for file in ["07210-0000000.json.zst", "_complete.json"] {
        std::fs::copy(raw.join("07").join(file), dir.join(file)).unwrap();
    }
}

#[test]
fn a_complete_download_is_kept_when_the_service_is_down() {
    let (source, es) = cartociudad();
    let tmp = tempfile::tempdir().unwrap();
    let raw = RawDir { path: tmp.path().join("cartociudad") };
    raw_fixture(&raw.path);
    let one = Down::default();
    let report = source.fetch(&scope(es, &["ES-PM"]), &one, &raw).unwrap();
    assert_eq!(report.kept_after_failure.len(), 1);
    assert_eq!(report.not_modified, report.kept_after_failure);
    assert!(report.downloaded.is_empty());
    assert_eq!(report.identities.len(), 1);
    assert!(raw.path.join("07/_complete.json").exists());

    copy_fixture(&raw.path, "08");
    let two = Down::default();
    let report = source.fetch(&scope(es, &["ES-PM", "ES-B"]), &two, &raw).unwrap();
    assert_eq!(report.kept_after_failure.len(), 2);
    assert_eq!(two.asked.load(Ordering::SeqCst), one.asked.load(Ordering::SeqCst), "the second province does not wait for the service too");
}

#[test]
fn without_a_complete_download_the_source_fails() {
    let (source, es) = cartociudad();
    let tmp = tempfile::tempdir().unwrap();
    let raw = RawDir { path: tmp.path().join("cartociudad") };
    std::fs::create_dir_all(&raw.path).unwrap();
    assert!(source.fetch(&scope(es, &["ES-PM"]), &Down::default(), &raw).is_err());
}

#[test]
fn pages_a_staging_needs_back_are_not_kept_by_their_marker() {
    let (source, es) = cartociudad();
    let tmp = tempfile::tempdir().unwrap();
    let raw = RawDir { path: tmp.path().join("cartociudad") };
    raw_fixture(&raw.path);
    std::fs::remove_file(raw.path.join("07/07210-0000000.json.zst")).unwrap();
    assert!(source.fetch(&scope(es, &["ES-PM"]), &Down { restoring: true, ..Down::default() }, &raw).is_err());
}

#[test]
fn conformance() {
    let (source, es) = cartociudad();
    geko_conformance::source::check_metadata(&source, es);

    let tmp = tempfile::tempdir().unwrap();
    let raw = RawDir { path: tmp.path().join("cartociudad") };
    raw_fixture(&raw.path);
    let partition = Partition { country: es, region: Some(RegionCode::new("ES-PM").unwrap()) };
    geko_conformance::source::check_stage(&source, &partition, &raw);
}

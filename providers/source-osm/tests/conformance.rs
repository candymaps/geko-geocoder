use geko_core::country::{CountryCode, RegionCode};
use geko_core::ports::{RawDir, SourceProvider, StagingDir};
use geko_core::scope::Partition;
use source_osm::{OsmConfig, OsmSource};

#[test]
fn conformance() {
    let source = OsmSource::new(OsmConfig::default(), Default::default());
    let es = CountryCode::new("ES").unwrap();
    geko_conformance::source::check_metadata(&source, es);

    // Staging from a fixture needs a small `.osm.pbf` (BACKLOG 18); until then, only the error
    // without downloads.
    let tmp = tempfile::tempdir().unwrap();
    let partition = Partition { country: es, region: Some(RegionCode::new("ES-PM").unwrap()) };
    let raw = RawDir { path: tmp.path().join("raw") };
    std::fs::create_dir_all(&raw.path).unwrap();
    assert!(source.stage(&partition, &raw, &StagingDir { path: tmp.path().join("out") }).is_err());
}

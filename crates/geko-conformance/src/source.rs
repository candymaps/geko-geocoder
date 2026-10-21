//! Conformance of `SourceProvider`. The provider passes a raw directory with a small fixture of
//! its source for one partition; the suite never touches the network.

use std::path::Path;

use geko_core::country::CountryCode;
use geko_core::ports::{DownloadOutcome, DownloadRequest, Downloader, RawDir, SourceProvider, StagingDir};
use geko_core::scope::{Coverage, Partition, Scope};
use geko_core::{GekoError, Result};

/// A downloader without network: every request fails.
struct Offline;

impl Downloader for Offline {
    fn fetch(&self, req: &DownloadRequest, _: &Path) -> Result<DownloadOutcome> {
        Err(GekoError::Data(format!("offline: {}", req.url)))
    }
}

/// Metadata, and planning and fetching without network: an error, never a panic, and nothing
/// written outside `raw`.
pub fn check_metadata(p: &dyn SourceProvider, country: CountryCode) {
    let id = p.id();
    assert!(!id.0.is_empty() && id.0.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'), "id {:?}", id.0);
    let license = p.license();
    assert!(!license.name.is_empty() && !license.attribution.is_empty(), "every source declares its license");
    assert!(!p.used_by().is_empty(), "a source is used by some module");
    if let Coverage::Countries(list) = p.coverage() {
        assert!(!list.is_empty(), "coverage lists its countries");
    }

    let tmp = tempfile::tempdir().unwrap();
    let raw = RawDir { path: tmp.path().join("raw") };
    std::fs::create_dir_all(&raw.path).unwrap();
    let mut scope = Scope::default();
    scope.add(country, vec![]).unwrap();
    let _ = p.plan(&scope, &Offline, &raw);
    assert!(p.fetch(&scope, &Offline, &raw).is_err(), "fetching without network fails");
    let outside: Vec<_> = std::fs::read_dir(tmp.path()).unwrap().filter_map(|e| e.ok()).filter(|e| e.file_name() != "raw").collect();
    assert!(outside.is_empty(), "plan and fetch write only into raw: {outside:?}");
}

/// Staging of `partition` from the fixture in `raw`: it writes something, it is deterministic
/// (incremental builds rely on it, PROJECT.md §8.3), its declared inputs exist under `raw`, and
/// an empty raw directory is an error, not a panic.
pub fn check_stage(p: &dyn SourceProvider, partition: &Partition, raw: &RawDir) {
    let tmp = tempfile::tempdir().unwrap();
    let stage = |name: &str| {
        let out = StagingDir { path: tmp.path().join(name) };
        let stats = p.stage(partition, raw, &out).expect("the fixture stages");
        (stats, out.path)
    };
    let (first, a) = stage("a");
    assert!(first.records > 0, "the fixture yields records");
    let (second, b) = stage("b");
    assert_eq!(first.records, second.records);
    assert_eq!(
        geko_build::incremental::digest(&a, &[]).unwrap(),
        geko_build::incremental::digest(&b, &[]).unwrap(),
        "staging the same input twice writes the same bytes"
    );

    let inputs = p.stage_inputs(partition, raw).expect("inputs of the fixture");
    assert!(!inputs.is_empty(), "staging reads something");
    for input in &inputs {
        assert!(input.starts_with(&raw.path), "{} is outside raw", input.display());
        assert!(input.exists(), "declared input {} exists", input.display());
    }

    let empty = RawDir { path: tmp.path().join("empty-raw") };
    std::fs::create_dir_all(&empty.path).unwrap();
    let out = StagingDir { path: tmp.path().join("c") };
    assert!(p.stage(partition, &empty, &out).is_err(), "staging without downloads is an error");
}

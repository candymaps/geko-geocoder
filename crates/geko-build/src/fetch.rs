use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use geko_core::model::SourceId;
use geko_core::progress;
use geko_core::ports::{DatasetRequest, Downloader, FetchReport, RawDir, SourceStats, StagingDir};
use geko_core::scope::{Partition, Scope};
use geko_core::{GekoError, Result};

use crate::incremental;
use crate::selection::SelectedSource;

pub fn raw_dir(data_dir: &Path, source: &SourceId) -> RawDir {
    RawDir { path: data_dir.join("raw").join(&source.0) }
}

pub fn plan_datasets(
    selected: &[SelectedSource],
    scope: &Scope,
    dl: &dyn Downloader,
    data_dir: &Path,
) -> Vec<(SourceId, Result<Vec<DatasetRequest>>)> {
    selected
        .iter()
        .map(|s| {
            let id = s.provider.id();
            let raw = raw_dir(data_dir, &id);
            (id, s.provider.plan(scope, dl, &raw))
        })
        .collect()
}

/// Fetches every selected source, `parallelism` sources at a time.
/// Every source is attempted even if another one fails.
pub fn fetch_all(
    selected: &[SelectedSource],
    scope: &Scope,
    dl: &dyn Downloader,
    data_dir: &Path,
    parallelism: usize,
) -> Vec<(SourceId, Result<FetchReport>)> {
    progress::expect(selected.len() as u64);
    let queue = Mutex::new(selected.iter().collect::<Vec<_>>());
    let results = Mutex::new(Vec::with_capacity(selected.len()));
    std::thread::scope(|threads| {
        for _ in 0..parallelism.clamp(1, selected.len().max(1)) {
            threads.spawn(|| {
                loop {
                    // Apart: in the `while let`, the lock would last the whole download, one source at a time.
                    let next = queue.lock().unwrap().pop();
                    let Some(source) = next else { break };
                    let id = source.provider.id();
                    let raw = raw_dir(data_dir, &id);
                    let result = std::fs::create_dir_all(&raw.path)
                        .map_err(GekoError::from)
                        .and_then(|_| source.provider.fetch(scope, dl, &raw));
                    results.lock().unwrap().push((id, result));
                    progress::advance(1);
                }
            });
        }
    });
    let mut results = results.into_inner().unwrap();
    results.sort_by(|a, b| a.0.cmp(&b.0));
    results
}

pub fn staging_dir(data_dir: &Path, source: &SourceId, partition: &Partition) -> StagingDir {
    let region = partition.region.as_ref().map_or("_all".to_string(), |r| r.as_str().to_string());
    StagingDir { path: data_dir.join("staging").join(&source.0).join(partition.country.as_str()).join(region) }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageOutcome {
    pub records: u64,
    /// The staging of an earlier build was up to date (PROJECT.md §8.3).
    pub reused: bool,
}

/// What a build knows of the downloads of each source: the identities that its fetch reported.
pub type Identities = BTreeMap<SourceId, BTreeMap<PathBuf, String>>;

/// Collects the identities of every source fetched successfully.
pub fn identities(fetched: &[(SourceId, Result<FetchReport>)]) -> Identities {
    fetched
        .iter()
        .filter_map(|(id, report)| Some((id.clone(), report.as_ref().ok()?.identities.iter().cloned().collect())))
        .collect()
}

/// The partitions in which a source is staged.
pub fn stage_partitions(source: &SelectedSource, scope: &Scope) -> Vec<Partition> {
    let mut partitions: Vec<Partition> = scope.partitions().into_iter().filter(|p| source.countries.contains(&p.country)).collect();
    if source.provider.stage_by_country() {
        partitions = partitions.into_iter().map(|p| Partition { country: p.country, region: None }).collect();
        partitions.dedup();
    }
    partitions
}

/// Stages every (source, partition) pair the source covers, in order, except those whose inputs
/// and code (`code` of the source) are unchanged since they were last staged, unless `force`.
/// Inputs are keyed by `identities` where their source gave one, so a staging is reused even if
/// its downloads were removed after publishing (`[data] keep_downloads = false`). Before
/// staging a pair again, the source fetches what those partitions read with `restore`, which
/// brings back removed downloads, and nothing else. Every pair is attempted even if another one
/// fails.
pub fn stage_all(
    selected: &[SelectedSource],
    scope: &Scope,
    data_dir: &Path,
    code: &dyn Fn(&SourceId) -> String,
    force: bool,
    identities: &Identities,
    restore: &dyn Downloader,
) -> Vec<(SourceId, Partition, Result<StageOutcome>)> {
    let mut results = Vec::new();
    let none = BTreeMap::new();
    for source in selected {
        let provider = source.provider.as_ref();
        let id = provider.id();
        let raw = raw_dir(data_dir, &id);
        let code = code(&id);
        let key = |partition: &Partition, identities: &BTreeMap<PathBuf, String>| -> Result<String> {
            Ok(incremental::Key::new("stage")
                .add("source", &id.0)
                .add("code", &code)
                .inputs(&provider.stage_inputs(partition, &raw)?, identities)?
                .finish())
        };
        let known = identities.get(&id).unwrap_or(&none);
        let mut stale = Vec::new();
        for partition in stage_partitions(source, scope) {
            let out = staging_dir(data_dir, &id, &partition);
            let up_to_date = key(&partition, known).map(|key| incremental::up_to_date(&out.path, &key).filter(|_| !force));
            match up_to_date {
                Ok(Some(stamp)) => {
                    let records = stamp.info["records"].as_u64().unwrap_or(0);
                    results.push((id.clone(), partition, Ok(StageOutcome { records, reused: true })));
                }
                Ok(None) => stale.push(partition),
                Err(e) => results.push((id.clone(), partition, Err(e))),
            }
        }
        if stale.is_empty() {
            continue;
        }
        // Only what was removed after publishing ([data] keep_downloads = false), which can take
        // longer than the staging itself.
        progress::doing(Some(format!("downloading {} again", id.0)));
        let restored = restore_scope(scope, &stale).and_then(|sub| provider.fetch(&sub, restore, &raw));
        progress::doing(None);
        let restored = match restored {
            Ok(report) => report,
            Err(e) => {
                let message = e.to_string();
                for partition in stale {
                    let error = GekoError::Data(format!(
                        "its staging has to be repeated and reads downloads removed after publishing ([data] keep_downloads = false); downloading them again failed: {message}"
                    ));
                    results.push((id.clone(), partition, Err(error)));
                }
                continue;
            }
        };
        let mut current = known.clone();
        current.extend(restored.identities);
        // Each partition weighs what its inputs take on disk: a provider may count what it reads
        // of them as it goes (`progress::Reading`), and the rest counts when it ends.
        let weights: Vec<u64> = stale.iter().map(|p| input_bytes(provider, p, &raw)).collect();
        progress::expect(weights.iter().sum());
        let mut staged = progress::done();
        for (partition, weight) in stale.into_iter().zip(weights) {
            let out = staging_dir(data_dir, &id, &partition);
            let result = key(&partition, &current).and_then(|key| stage_one(provider, &partition, &raw, &out, key, &code));
            results.push((id.clone(), partition, result));
            staged += weight;
            progress::reach(staged);
        }
    }
    results
}

/// What the inputs of a partition take on disk; 1 at least, so that a partition without inputs still
/// counts.
fn input_bytes(provider: &dyn geko_core::ports::SourceProvider, partition: &Partition, raw: &RawDir) -> u64 {
    provider.stage_inputs(partition, raw).map_or(0, |inputs| inputs.iter().map(|p| progress::disk_bytes(p)).sum::<u64>()).max(1)
}

/// The scope of the partitions to stage again: a partition staged by country stands for every
/// region of its country in `scope`.
fn restore_scope(scope: &Scope, partitions: &[Partition]) -> Result<Scope> {
    let mut regions: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for partition in partitions {
        let entry = regions.entry(partition.country).or_default();
        match &partition.region {
            Some(region) => entry.push(region.clone()),
            None => entry.extend(scope.partitions().into_iter().filter(|p| p.country == partition.country).filter_map(|p| p.region)),
        }
    }
    let mut sub = Scope::default();
    for (country, regions) in regions {
        sub.add(country, regions)?;
    }
    Ok(sub)
}

fn stage_one(
    provider: &dyn geko_core::ports::SourceProvider,
    partition: &Partition,
    raw: &RawDir,
    out: &StagingDir,
    key: String,
    code: &str,
) -> Result<StageOutcome> {
    incremental::invalidate(&out.path)?;
    let SourceStats { records } = provider.stage(partition, raw, out)?;
    if out.path.is_dir() {
        incremental::seal(&out.path, key, &[], serde_json::json!({ "records": records, "code": code }))?;
    }
    Ok(StageOutcome { records, reused: false })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use geko_core::country::CountryCode;
    use geko_core::model::License;
    use geko_core::ports::{DownloadOutcome, DownloadRequest, SourceProvider, SourceStats, StagingDir};
    use geko_core::scope::{Coverage, Partition};

    struct NoNetwork;

    impl Downloader for NoNetwork {
        fn fetch(&self, _: &DownloadRequest, _: &Path) -> Result<DownloadOutcome> {
            Err(GekoError::Data("no network in tests".into()))
        }
    }

    struct Fake {
        id: &'static str,
        fail: bool,
        calls: Arc<AtomicUsize>,
    }

    impl SourceProvider for Fake {
        fn id(&self) -> SourceId {
            SourceId(self.id.into())
        }
        fn license(&self) -> License {
            License { name: "test".into(), attribution: "test".into(), url: None }
        }
        fn coverage(&self) -> Coverage {
            Coverage::Global
        }
        fn plan(&self, _: &Scope, _: &dyn Downloader, raw: &RawDir) -> Result<Vec<DatasetRequest>> {
            Ok(vec![DatasetRequest {
                source: self.id(),
                name: raw.path.display().to_string(),
                url: format!("https://example.org/{}", self.id),
                estimated_size: Some(1),
            }])
        }
        fn fetch(&self, _: &Scope, _: &dyn Downloader, raw: &RawDir) -> Result<FetchReport> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert!(raw.path.is_dir());
            if self.fail {
                return Err(GekoError::Data(format!("{} failed", self.id)));
            }
            Ok(FetchReport { downloaded: vec![self.id.into()], ..FetchReport::default() })
        }
        fn stage(&self, _: &Partition, _: &RawDir, _: &StagingDir) -> Result<SourceStats> {
            Ok(SourceStats::default())
        }
    }

    fn selected(calls: &Arc<AtomicUsize>) -> Vec<SelectedSource> {
        ["a", "b", "c", "d"]
            .into_iter()
            .map(|id| SelectedSource {
                provider: Arc::new(Fake { id, fail: id == "b", calls: Arc::clone(calls) }),
                countries: vec![CountryCode::new("ES").unwrap()],
            })
            .collect()
    }

    #[test]
    fn fetches_every_source_even_if_one_fails() {
        let calls = Arc::new(AtomicUsize::new(0));
        let dir = tempfile::tempdir().unwrap();

        let results = fetch_all(&selected(&calls), &Scope::default(), &NoNetwork, dir.path(), 3);

        assert_eq!(calls.load(Ordering::SeqCst), 4);
        let summary: Vec<_> = results.iter().map(|(id, r)| (id.0.as_str(), r.is_ok())).collect();
        assert_eq!(summary, [("a", true), ("b", false), ("c", true), ("d", true)]);
        assert!(dir.path().join("raw/a").is_dir());
    }

    #[test]
    fn staging_directories_per_partition() {
        let es = CountryCode::new("ES").unwrap();
        let id = SourceId("osm".into());
        let region = Partition { country: es, region: Some(geko_core::country::RegionCode::new("ES-B").unwrap()) };
        let whole = Partition { country: es, region: None };
        assert!(staging_dir(Path::new("/d"), &id, &region).path.ends_with("staging/osm/ES/ES-B"));
        assert!(staging_dir(Path::new("/d"), &id, &whole).path.ends_with("staging/osm/ES/_all"));
    }

    /// Stages a copy of `raw/w/input`.
    struct Copier {
        staged: Arc<AtomicUsize>,
    }

    impl SourceProvider for Copier {
        fn id(&self) -> SourceId {
            SourceId("w".into())
        }
        fn license(&self) -> License {
            License { name: "test".into(), attribution: "test".into(), url: None }
        }
        fn coverage(&self) -> Coverage {
            Coverage::Global
        }
        fn plan(&self, _: &Scope, _: &dyn Downloader, _: &RawDir) -> Result<Vec<DatasetRequest>> {
            Ok(vec![])
        }
        fn fetch(&self, _: &Scope, _: &dyn Downloader, _: &RawDir) -> Result<FetchReport> {
            Ok(FetchReport::default())
        }
        fn stage(&self, _: &Partition, raw: &RawDir, out: &StagingDir) -> Result<SourceStats> {
            self.staged.fetch_add(1, Ordering::SeqCst);
            std::fs::create_dir_all(&out.path)?;
            std::fs::copy(raw.path.join("input"), out.path.join("output"))?;
            Ok(SourceStats { records: 7 })
        }
        fn stage_inputs(&self, _: &Partition, raw: &RawDir) -> Result<Vec<std::path::PathBuf>> {
            Ok(vec![raw.path.join("input")])
        }
    }

    #[test]
    fn restages_only_when_inputs_or_code_change() {
        let es = CountryCode::new("ES").unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("raw/w")).unwrap();
        std::fs::write(dir.path().join("raw/w/input"), b"v1").unwrap();
        let staged = Arc::new(AtomicUsize::new(0));
        let selected = [SelectedSource { provider: Arc::new(Copier { staged: Arc::clone(&staged) }), countries: vec![es] }];
        let mut scope = Scope::default();
        scope.add(es, vec![geko_core::country::RegionCode::new("ES-PM").unwrap()]).unwrap();
        let run = |code: &str, force: bool| -> Vec<bool> {
            stage_all(&selected, &scope, dir.path(), &|_| code.to_owned(), force, &Identities::new(), &NoNetwork)
                .into_iter()
                .map(|(_, _, r)| {
                    let r = r.unwrap();
                    assert_eq!(r.records, 7);
                    r.reused
                })
                .collect()
        };

        assert_eq!(run("1", false), [false]);
        assert_eq!(run("1", false), [true]);
        assert_eq!(staged.load(Ordering::SeqCst), 1);
        assert_eq!(run("2", false), [false], "new code");
        assert_eq!(run("2", true), [false], "forced");
        std::fs::write(dir.path().join("raw/w/input"), b"v2, longer").unwrap();
        assert_eq!(run("2", false), [false], "new input");
        assert_eq!(run("2", false), [true]);
        assert_eq!(staged.load(Ordering::SeqCst), 4);

        // A failed stage leaves no stamp: the next build stages again.
        std::fs::remove_file(dir.path().join("raw/w/input")).unwrap();
        assert!(stage_all(&selected, &scope, dir.path(), &|_| "2".into(), false, &Identities::new(), &NoNetwork)[0].2.is_err());
        std::fs::write(dir.path().join("raw/w/input"), b"v2, longer").unwrap();
        assert_eq!(run("2", false), [false]);
    }

    /// Brings back removed downloads (`Downloader::keeps_removed`), without any network.
    struct Restorer;

    impl Downloader for Restorer {
        fn fetch(&self, _: &DownloadRequest, _: &Path) -> Result<DownloadOutcome> {
            Err(GekoError::Data("no network in tests".into()))
        }
        fn keeps_removed(&self) -> bool {
            false
        }
    }

    /// Like `Copier`, but its input has an identity and can be removed: its fetch brings it back.
    struct Removable {
        staged: Arc<AtomicUsize>,
        restored: Arc<AtomicUsize>,
    }

    impl SourceProvider for Removable {
        fn id(&self) -> SourceId {
            SourceId("w".into())
        }
        fn license(&self) -> License {
            License { name: "test".into(), attribution: "test".into(), url: None }
        }
        fn coverage(&self) -> Coverage {
            Coverage::Global
        }
        fn plan(&self, _: &Scope, _: &dyn Downloader, _: &RawDir) -> Result<Vec<DatasetRequest>> {
            Ok(vec![])
        }
        fn fetch(&self, _: &Scope, dl: &dyn Downloader, raw: &RawDir) -> Result<FetchReport> {
            let input = raw.path.join("input");
            if !input.exists() && !dl.keeps_removed() {
                self.restored.fetch_add(1, Ordering::SeqCst);
                std::fs::write(&input, b"v1")?;
            }
            Ok(FetchReport { identities: vec![(input, "v1".into())], ..FetchReport::default() })
        }
        fn stage(&self, p: &Partition, raw: &RawDir, out: &StagingDir) -> Result<SourceStats> {
            Copier { staged: Arc::clone(&self.staged) }.stage(p, raw, out)
        }
        fn stage_inputs(&self, _: &Partition, raw: &RawDir) -> Result<Vec<std::path::PathBuf>> {
            Ok(vec![raw.path.join("input")])
        }
        fn removable(&self, raw: &RawDir) -> Result<Vec<std::path::PathBuf>> {
            Ok(vec![raw.path.join("input")])
        }
    }

    #[test]
    fn a_staging_keyed_by_identity_survives_its_removed_downloads() {
        let es = CountryCode::new("ES").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("raw/w");
        std::fs::create_dir_all(&raw).unwrap();
        std::fs::write(raw.join("input"), b"v1").unwrap();
        let (staged, restored) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let provider = Removable { staged: Arc::clone(&staged), restored: Arc::clone(&restored) };
        let selected = [SelectedSource { provider: Arc::new(provider), countries: vec![es] }];
        let mut scope = Scope::default();
        scope.add(es, vec![geko_core::country::RegionCode::new("ES-PM").unwrap()]).unwrap();
        let run = |code: &str| -> bool {
            let fetched = fetch_all(&selected, &scope, &NoNetwork, dir.path(), 1);
            let ids = identities(&fetched);
            let results = stage_all(&selected, &scope, dir.path(), &|_| code.to_owned(), false, &ids, &Restorer);
            results[0].2.as_ref().unwrap().reused
        };

        assert!(!run("1"));
        std::fs::remove_file(raw.join("input")).unwrap();
        assert!(run("1"), "same identity: reused without the download");
        assert_eq!(restored.load(Ordering::SeqCst), 0);
        assert!(!run("2"), "new code: staged again");
        assert_eq!(restored.load(Ordering::SeqCst), 1, "after bringing the download back");
        assert_eq!(staged.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn plans_each_source_in_its_own_raw_directory() {
        let calls = Arc::new(AtomicUsize::new(0));
        let dir = tempfile::tempdir().unwrap();

        let plans = plan_datasets(&selected(&calls), &Scope::default(), &NoNetwork, dir.path());

        assert_eq!(plans.len(), 4);
        let first = plans[0].1.as_ref().unwrap();
        assert_eq!(first[0].name, dir.path().join("raw/a").display().to_string());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

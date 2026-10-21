//! `geko build --purge`: removes the downloads and work files of the countries, regions and
//! sources that the configuration no longer asks for. Published bundles are left to `[build] keep`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use geko_core::Result;
use geko_core::scope::Scope;

use crate::fetch::raw_dir;
use crate::fusion::merge::partition_name;
use crate::incremental;
use crate::selection::SelectedSource;

/// Removes what `scope` and `selected` do not need, and returns the paths removed.
pub fn purge(data_dir: &Path, scope: &Scope, selected: &[SelectedSource]) -> Result<Vec<PathBuf>> {
    let mut parts: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for p in scope.partitions() {
        let names = parts.entry(p.country.as_str().to_owned()).or_default();
        names.insert(partition_name(&p));
        // Sources staged by country use the country's `_all` partition.
        names.insert("_all".to_owned());
    }
    let sources: BTreeSet<String> = selected.iter().map(|s| s.provider.id().0).collect();
    let country = |name: &str| parts.contains_key(name);
    let part = |country: &str, name: &str| parts.get(country).is_some_and(|names| names.contains(name));

    let mut removed = Vec::new();
    remove_unless(&data_dir.join("raw"), |name| sources.contains(name), &mut removed)?;
    for s in selected {
        let raw = raw_dir(data_dir, &s.provider.id());
        for path in s.provider.unused_raw(scope, &raw)? {
            remove_with_siblings(&path, &mut removed)?;
        }
    }
    let staging = data_dir.join("staging");
    remove_unless(&staging, |name| sources.contains(name), &mut removed)?;
    for source in &sources {
        let dir = staging.join(source);
        remove_unless(&dir, country, &mut removed)?;
        for c in parts.keys() {
            remove_unless(&dir.join(c), |name| part(c, name), &mut removed)?;
        }
    }
    let relocated = data_dir.join("relocated");
    remove_unless(&relocated, country, &mut removed)?;
    let canonical = data_dir.join("canonical");
    remove_unless(&canonical, country, &mut removed)?;
    for c in parts.keys() {
        remove_unless(&relocated.join(c), |name| part(c, name), &mut removed)?;
        remove_unless(&canonical.join(c).join("parts"), |name| part(c, name), &mut removed)?;
    }
    remove_unless(&data_dir.join("segments"), |name| country(&name.to_uppercase()), &mut removed)?;
    Ok(removed)
}

/// Removes the entries of `dir` whose name, without the stage stamp, `keep` rejects. Hidden
/// entries are not geko's stages (the golden's `raw/.snapshot`, for one) and stay.
fn remove_unless(dir: &Path, keep: impl Fn(&str) -> bool, removed: &mut Vec<PathBuf>) -> Result<()> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Ok(()) };
    for entry in entries {
        let path = entry?.path();
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if !name.starts_with('.') && !keep(name.strip_suffix(incremental::STAMP_SUFFIX).unwrap_or(&name)) {
            remove(&path, removed)?;
        }
    }
    Ok(())
}

/// Removes `path` and the files beside it named after it (`<name>.md5`, the downloader's
/// `<name>.raw.json`…).
fn remove_with_siblings(path: &Path, removed: &mut Vec<PathBuf>) -> Result<()> {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else { return Ok(()) };
    let prefix = format!("{}.", name.to_string_lossy());
    for entry in std::fs::read_dir(dir)? {
        let sibling = entry?.path();
        if sibling.file_name().is_some_and(|n| n.to_string_lossy().starts_with(&prefix)) {
            remove(&sibling, removed)?;
        }
    }
    remove(path, removed)
}

fn remove(path: &Path, removed: &mut Vec<PathBuf>) -> Result<()> {
    if path.is_dir() {
        std::fs::remove_dir_all(path)?;
    } else if path.exists() {
        std::fs::remove_file(path)?;
    } else {
        return Ok(());
    }
    removed.push(path.to_path_buf());
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use geko_core::country::{CountryCode, RegionCode};
    use geko_core::model::{License, SourceId};
    use geko_core::module::ModuleName;
    use geko_core::ports::{DatasetRequest, Downloader, FetchReport, RawDir, SourceProvider, SourceStats, StagingDir};
    use geko_core::scope::{Coverage, Partition};

    use super::*;

    /// Keeps one file per country in `raw`, named after it.
    struct Fake;

    impl SourceProvider for Fake {
        fn id(&self) -> SourceId {
            SourceId("osm".into())
        }
        fn license(&self) -> License {
            License { name: "test".into(), attribution: "test".into(), url: None }
        }
        fn coverage(&self) -> Coverage {
            Coverage::Global
        }
        fn used_by(&self) -> &'static [ModuleName] {
            &[ModuleName::Geocoder]
        }
        fn plan(&self, _: &Scope, _: &dyn Downloader, _: &RawDir) -> Result<Vec<DatasetRequest>> {
            Ok(vec![])
        }
        fn fetch(&self, _: &Scope, _: &dyn Downloader, _: &RawDir) -> Result<FetchReport> {
            Ok(FetchReport::default())
        }
        fn stage(&self, _: &Partition, _: &RawDir, _: &StagingDir) -> Result<SourceStats> {
            Ok(SourceStats::default())
        }
        fn unused_raw(&self, scope: &Scope, raw: &RawDir) -> Result<Vec<PathBuf>> {
            let used: Vec<String> = scope.countries().map(|c| format!("{c}.pbf")).collect();
            Ok(["ES.pbf", "LU.pbf"].into_iter().filter(|f| !used.iter().any(|u| u == f)).map(|f| raw.path.join(f)).collect())
        }
    }

    #[test]
    fn removes_what_the_scope_no_longer_needs() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path();
        let files = [
            "raw/osm/ES.pbf",
            "raw/osm/LU.pbf",
            "raw/osm/LU.pbf.raw.json",
            "raw/osm/index.json",
            "raw/cartociudad/07/page",
            "raw/.snapshot",
            "staging/osm/ES/ES-PM/x",
            "staging/osm/ES/ES-PM.stamp.json",
            "staging/osm/ES/ES-B/x",
            "staging/osm/ES/ES-B.stamp.json",
            "staging/osm/LU/_all/x",
            "staging/cartociudad/ES/ES-PM/x",
            "relocated/ES/ES-B/x",
            "relocated/LU.stamp.json",
            "canonical/ES/parts/ES-PM/x",
            "canonical/ES/parts/ES-B/x",
            "canonical/ES/streets.parquet",
            "canonical/LU/streets.parquet",
            "canonical/LU.stamp.json",
            "segments/es/x",
            "segments/lu/x",
            "bundles/1/x",
            "golden/lu/raw/osm/LU.pbf",
        ];
        for f in files {
            std::fs::create_dir_all(data.join(f).parent().unwrap()).unwrap();
            std::fs::write(data.join(f), "").unwrap();
        }
        let mut scope = Scope::default();
        scope.add(CountryCode::new("ES").unwrap(), vec![RegionCode::new("ES-PM").unwrap()]).unwrap();
        let selected = [SelectedSource { provider: Arc::new(Fake), countries: vec![CountryCode::new("ES").unwrap()] }];

        let mut removed: Vec<String> = purge(data, &scope, &selected)
            .unwrap()
            .iter()
            .map(|p| p.strip_prefix(data).unwrap().to_string_lossy().into_owned())
            .collect();
        removed.sort();
        assert_eq!(
            removed,
            [
                "canonical/ES/parts/ES-B",
                "canonical/LU",
                "canonical/LU.stamp.json",
                "raw/cartociudad",
                "raw/osm/LU.pbf",
                "raw/osm/LU.pbf.raw.json",
                "relocated/ES/ES-B",
                "relocated/LU.stamp.json",
                "segments/lu",
                "staging/cartociudad",
                "staging/osm/ES/ES-B",
                "staging/osm/ES/ES-B.stamp.json",
                "staging/osm/LU",
            ]
        );
        for kept in ["raw/osm/ES.pbf", "raw/osm/index.json", "raw/.snapshot", "canonical/ES/streets.parquet", "bundles/1/x", "golden/lu/raw/osm/LU.pbf"] {
            assert!(data.join(kept).exists(), "{kept}");
        }
    }
}

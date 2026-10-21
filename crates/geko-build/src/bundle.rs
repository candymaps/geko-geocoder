//! Publication of immutable, versioned bundles (PROJECT.md §12).

use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use geko_core::bundle::{Artifact, BundleManifest, CURRENT, CURRENT_VERSION, LicenseNotice, ProviderFormat, Segment, current_version};
use geko_core::country::CountryCode;
use geko_core::ports::{Checksum, DownloadRequest, Downloader};
use geko_core::{GekoError, Result};
use sha2::{Digest, Sha256};

use crate::incremental::{self, Key, Stamp};

/// The parts of a segment: the indexes, and the country pack they were built with, which
/// `serve` reads from the bundle (PROJECT.md §2.4). A segment built before packs were part of
/// bundles has none.
const PARTS: [&str; 3] = ["geocode", "reverse", "pack"];
const OPTIONAL_PART: &str = "pack";

pub struct SegmentToPublish {
    pub country: CountryCode,
    /// Working directory with `geocode/`, `reverse/` and `pack/`.
    pub dir: PathBuf,
    pub geocode: ProviderFormat,
    pub reverse: ProviderFormat,
}

/// `vYYYY-MM-DDTHHMMSS` in UTC; lexicographic order is chronological order.
pub fn version_name(unix: u64) -> String {
    let (days, secs) = (unix / 86_400, unix % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("v{year:04}-{month:02}-{day:02}T{:02}{:02}{:02}", secs / 3600, secs % 3600 / 60, secs % 60)
}

fn sha256(path: &Path) -> Result<String> {
    let mut reader = BufReader::with_capacity(1 << 20, File::open(path)?);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Hard-links (or copies across filesystems) every file of `src` into `dest`, recording each
/// as an artifact relative to `root`. Segment files are only ever replaced through a rename,
/// so a link never sees a later build.
fn link_tree(src: &Path, dest: &Path, root: &Path, artifacts: &mut Vec<Artifact>) -> Result<()> {
    fs::create_dir_all(dest)?;
    let mut entries: Vec<_> = fs::read_dir(src)?.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let (from, to) = (entry.path(), dest.join(entry.file_name()));
        if entry.file_type()?.is_dir() {
            link_tree(&from, &to, root, artifacts)?;
            continue;
        }
        if from.extension().is_some_and(|e| e == "tmp") {
            continue;
        }
        if fs::hard_link(&from, &to).is_err() {
            fs::copy(&from, &to)?;
        }
        artifacts.push(Artifact {
            path: to.strip_prefix(root).unwrap_or(&to).to_string_lossy().replace('\\', "/"),
            size: fs::metadata(&to)?.len(),
            sha256: sha256(&to)?,
        });
    }
    Ok(())
}

fn point_current(bundles: &Path, version: &str) -> Result<()> {
    let tmp = bundles.join(format!("{CURRENT}.tmp"));
    let _ = fs::remove_file(&tmp);
    #[cfg(unix)]
    std::os::unix::fs::symlink(version, &tmp)?;
    #[cfg(not(unix))]
    fs::write(&tmp, version)?;
    fs::rename(&tmp, bundles.join(CURRENT))?;
    let tmp = bundles.join(format!("{CURRENT_VERSION}.tmp"));
    fs::write(&tmp, version)?;
    fs::rename(&tmp, bundles.join(CURRENT_VERSION))?;
    Ok(())
}

pub fn publish(
    bundles: &Path,
    segments: &[SegmentToPublish],
    modules: Vec<String>,
    licenses: Vec<LicenseNotice>,
    keep: usize,
) -> Result<BundleManifest> {
    fs::create_dir_all(bundles)?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let version = version_name(now);
    let staging = bundles.join(format!(".{version}.tmp"));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    let mut manifest =
        BundleManifest { version: version.clone(), created_unix: now, modules, segments: Vec::new(), licenses, artifacts: Vec::new() };
    for s in segments {
        let rel = format!("segments/{}", s.country.lowercase());
        for part in PARTS {
            let src = s.dir.join(part);
            if !src.is_dir() {
                if part == OPTIONAL_PART {
                    continue;
                }
                return Err(GekoError::Data(format!("{} is missing; run `geko build`", src.display())));
            }
            link_tree(&src, &staging.join(&rel).join(part), &staging, &mut manifest.artifacts)?;
        }
        manifest.segments.push(Segment { country: s.country.to_string(), dir: rel, geocode: s.geocode.clone(), reverse: s.reverse.clone() });
    }
    manifest.write(&staging)?;
    let target = bundles.join(&version);
    if target.exists() {
        return Err(GekoError::Data(format!("bundle {version} already exists")));
    }
    fs::rename(&staging, &target)?;
    point_current(bundles, &version)?;
    prune(bundles, keep.max(1))?;
    Ok(manifest)
}

pub enum Published {
    New(BundleManifest),
    /// Nothing changed since the current version was published (PROJECT.md §8.3).
    UpToDate(String),
}

/// Publishes a new version unless the current one was published from the same segments, modules
/// and licenses, or `force`. A new version with the same content would only make every server
/// reload for nothing.
pub fn publish_incremental(
    bundles: &Path,
    segments: &[SegmentToPublish],
    modules: Vec<String>,
    licenses: Vec<LicenseNotice>,
    keep: usize,
    force: bool,
) -> Result<Published> {
    let mut key = Key::new("publish")
        .add("modules", &modules.join(","))
        .add("licenses", &serde_json::to_string(&licenses).map_err(|e| GekoError::Other(e.into()))?);
    for s in segments {
        key = key.add("country", s.country.as_str()).add("formats", &format!("{:?} {:?}", s.geocode, s.reverse));
        for part in PARTS {
            let dir = s.dir.join(part);
            if !dir.is_dir() {
                if part == OPTIONAL_PART {
                    continue;
                }
                return Err(GekoError::Data(format!("{} is missing; run `geko build`", dir.display())));
            }
            key = key.add(part, &incremental::output_of(&dir, &[])?);
        }
    }
    let key = key.finish();
    if !force
        && let Some(stamp) = incremental::stamp(bundles)
        && stamp.key == key
        && current_version(bundles).as_deref() == Some(stamp.output.as_str())
        && bundles.join(&stamp.output).is_dir()
    {
        // Bundles published before it existed lack the pointer that mirrors read.
        if !bundles.join(CURRENT_VERSION).is_file() {
            point_current(bundles, &stamp.output)?;
        }
        return Ok(Published::UpToDate(stamp.output));
    }
    let manifest = publish(bundles, segments, modules, licenses, keep)?;
    incremental::record(bundles, &Stamp { key, output: manifest.version.clone(), info: serde_json::Value::Null })?;
    Ok(Published::New(manifest))
}

/// Keeps the `keep` newest versions, and never the one being served.
fn prune(bundles: &Path, keep: usize) -> Result<()> {
    let current = current_version(bundles);
    let mut versions: Vec<String> = fs::read_dir(bundles)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with('v'))
        .collect();
    versions.sort();
    let excess = versions.len().saturating_sub(keep);
    for old in versions.into_iter().take(excess) {
        if Some(&old) != current.as_ref() {
            fs::remove_dir_all(bundles.join(old))?;
        }
    }
    Ok(())
}

/// Brings the current version published at `remote` (an HTTP mirror of a bundles directory)
/// into `bundles`, downloading only the artifacts whose hash changed and hard-linking the others
/// from the local current version (PROJECT.md §12). Each download is checked against its hash,
/// and the new version only becomes current once complete. Returns the version pulled, or `None`
/// when the local copy is already current.
pub fn pull(remote: &str, bundles: &Path, downloader: &dyn Downloader, keep: usize) -> Result<Option<String>> {
    let remote = remote.trim_end_matches('/');
    // Downloads land here, with the sidecars of the downloader, and move into the bundle.
    let downloads = bundles.join(".downloads");
    fs::create_dir_all(&downloads)?;
    let pointer = downloads.join(CURRENT_VERSION);
    downloader.fetch(&DownloadRequest { url: format!("{remote}/{CURRENT_VERSION}"), checksum: None }, &pointer)?;
    let version = fs::read_to_string(&pointer)?.trim().to_owned();
    if !version.starts_with('v') || version.contains(['/', '\\', '.']) {
        return Err(GekoError::Data(format!("{remote}/{CURRENT_VERSION}: `{version}` is not a bundle version")));
    }
    if bundles.join(&version).join(geko_core::bundle::MANIFEST).is_file() {
        if current_version(bundles).as_deref() != Some(version.as_str()) {
            point_current(bundles, &version)?;
        }
        return Ok(None);
    }
    let staging = bundles.join(format!(".{version}.pull"));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    fs::create_dir_all(&staging)?;
    let url = |path: &str| format!("{remote}/{version}/{path}");
    let manifest_file = downloads.join(format!("{version}.{}", geko_core::bundle::MANIFEST));
    downloader.fetch(&DownloadRequest { url: url(geko_core::bundle::MANIFEST), checksum: None }, &manifest_file)?;
    fs::copy(&manifest_file, staging.join(geko_core::bundle::MANIFEST))?;
    let manifest = BundleManifest::read(&staging)?;
    if manifest.version != version {
        return Err(GekoError::Data(format!("{}: version {} where {version} was published", url(geko_core::bundle::MANIFEST), manifest.version)));
    }
    let previous: std::collections::HashMap<String, (String, PathBuf)> = current_version(bundles)
        .and_then(|v| BundleManifest::read(&bundles.join(&v)).ok().map(|m| (v, m)))
        .map(|(v, m)| m.artifacts.into_iter().map(|a| (a.path.clone(), (a.sha256, bundles.join(&v).join(&a.path)))).collect())
        .unwrap_or_default();
    for a in &manifest.artifacts {
        if a.path.split('/').any(|p| p.is_empty() || p == "..") {
            return Err(GekoError::Data(format!("{}: artifact path `{}` leaves the bundle", url(geko_core::bundle::MANIFEST), a.path)));
        }
        let dest = staging.join(&a.path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        let reused = previous.get(&a.path).is_some_and(|(sha, from)| sha == &a.sha256 && (fs::hard_link(from, &dest).is_ok() || fs::copy(from, &dest).is_ok()));
        if !reused {
            let file = downloads.join(&a.sha256);
            downloader.fetch(&DownloadRequest { url: url(&a.path), checksum: Some(Checksum::Sha256(a.sha256.clone())) }, &file)?;
            fs::rename(&file, &dest)?;
        }
        if sha256(&dest)? != a.sha256 {
            return Err(GekoError::Data(format!("{}: checksum differs from the manifest", url(&a.path))));
        }
    }
    let target = bundles.join(&version);
    if target.exists() {
        fs::remove_dir_all(&target)?;
    }
    fs::rename(&staging, &target)?;
    point_current(bundles, &version)?;
    prune(bundles, keep.max(1))?;
    Ok(Some(version))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serves files of a local "remote" bundles directory, as an HTTP mirror would.
    struct Mirror {
        root: PathBuf,
        fetched: std::sync::Mutex<Vec<String>>,
    }

    impl Downloader for Mirror {
        fn fetch(&self, req: &DownloadRequest, dest: &Path) -> Result<geko_core::ports::DownloadOutcome> {
            let rel = req.url.strip_prefix("mirror://root/").expect("mirror url");
            self.fetched.lock().unwrap().push(rel.to_owned());
            fs::copy(self.root.join(rel), dest)?;
            Ok(geko_core::ports::DownloadOutcome { downloaded: true, sha256: sha256(dest)?, size: fs::metadata(dest)?.len() })
        }
    }

    #[test]
    fn pulls_only_what_changed() {
        let tmp = tempfile::tempdir().unwrap();
        let (remote, local, work) = (tmp.path().join("remote"), tmp.path().join("local"), tmp.path().join("work"));
        let first = publish(&remote, &[segment(&work)], vec!["geocoder".into()], Vec::new(), 2).unwrap();
        let mirror = Mirror { root: remote.clone(), fetched: Default::default() };
        assert_eq!(pull("mirror://root", &local, &mirror, 2).unwrap(), Some(first.version.clone()));
        assert_eq!(current_version(&local), Some(first.version.clone()));
        assert_eq!(pull("mirror://root", &local, &mirror, 2).unwrap(), None, "already current");

        // A new version where only the reverse file changed: the geocode file is linked, not fetched.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        fs::write(work.join("reverse/points.bin"), b"new").unwrap();
        let second = publish(&remote, &[segment_without_rewrite(&work)], vec!["geocoder".into()], Vec::new(), 2).unwrap();
        mirror.fetched.lock().unwrap().clear();
        assert_eq!(pull("mirror://root", &local, &mirror, 2).unwrap(), Some(second.version.clone()));
        let fetched = mirror.fetched.lock().unwrap().clone();
        assert!(fetched.iter().any(|f| f.ends_with("reverse/points.bin")), "{fetched:?}");
        assert!(!fetched.iter().any(|f| f.ends_with("geocode/hot/a.bin")), "unchanged artifact fetched: {fetched:?}");
        assert_eq!(fs::read(local.join(&second.version).join("segments/es/reverse/points.bin")).unwrap(), b"new");
    }

    fn segment_without_rewrite(dir: &Path) -> SegmentToPublish {
        let fmt = |p: &str| ProviderFormat { provider: p.into(), version: 1 };
        SegmentToPublish { country: CountryCode::new("ES").unwrap(), dir: dir.to_path_buf(), geocode: fmt("g"), reverse: fmt("r") }
    }

    #[test]
    fn version_names_are_utc_dates() {
        assert_eq!(version_name(0), "v1970-01-01T000000");
        assert_eq!(version_name(1_792_059_626), "v2026-10-15T102026");
        assert_eq!(version_name(951_782_400), "v2000-02-29T000000");
    }

    fn segment(dir: &Path) -> SegmentToPublish {
        for part in ["geocode/hot", "reverse"] {
            fs::create_dir_all(dir.join(part)).unwrap();
        }
        fs::write(dir.join("geocode/hot/a.bin"), b"abc").unwrap();
        fs::write(dir.join("reverse/points.bin"), b"xyz").unwrap();
        let fmt = |p: &str| ProviderFormat { provider: p.into(), version: 1 };
        SegmentToPublish { country: CountryCode::new("ES").unwrap(), dir: dir.to_path_buf(), geocode: fmt("g"), reverse: fmt("r") }
    }

    #[test]
    fn publishes_switches_current_and_prunes() {
        let work = tempfile::tempdir().unwrap();
        let bundles = tempfile::tempdir().unwrap();
        let seg = segment(work.path());

        let first = publish(bundles.path(), std::slice::from_ref(&seg), vec!["geocoder".into()], vec![], 2).unwrap();
        assert_eq!(current_version(bundles.path()).as_deref(), Some(first.version.as_str()));
        assert_eq!(first.artifacts.len(), 2);
        assert_eq!(first.artifacts[0].path, "segments/es/geocode/hot/a.bin");
        assert_eq!(BundleManifest::read(&bundles.path().join(&first.version)).unwrap(), first);

        // A rebuild replaces files through a rename: the published copy keeps the old content.
        fs::write(work.path().join("geocode/hot/a.bin.tmp"), b"new").unwrap();
        fs::rename(work.path().join("geocode/hot/a.bin.tmp"), work.path().join("geocode/hot/a.bin")).unwrap();
        let published = bundles.path().join(&first.version).join("segments/es/geocode/hot/a.bin");
        assert_eq!(fs::read(published).unwrap(), b"abc");

        for v in ["v2000-01-01T000000", "v2000-01-02T000000"] {
            fs::create_dir_all(bundles.path().join(v)).unwrap();
        }
        prune(bundles.path(), 2).unwrap();
        assert!(!bundles.path().join("v2000-01-01T000000").exists());
        assert!(bundles.path().join(&first.version).exists(), "the served version is never pruned");
    }

    #[test]
    fn publishes_again_only_when_something_changed() {
        let work = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let bundles = root.path().join("bundles");
        let seg = segment(work.path());
        let publish = |modules: &[&str], force| {
            let modules = modules.iter().map(|m| m.to_string()).collect();
            publish_incremental(&bundles, std::slice::from_ref(&seg), modules, vec![], 5, force).unwrap()
        };

        // Versions are named by the second: wait for a new name before a new publication.
        let republish = |modules: &[&str], force| {
            std::thread::sleep(std::time::Duration::from_millis(1100));
            publish(modules, force)
        };

        let Published::New(first) = publish(&["geocoder"], false) else { panic!("first build publishes") };
        let Published::UpToDate(v) = publish(&["geocoder"], false) else { panic!("nothing changed") };
        assert_eq!(v, first.version);
        assert!(matches!(republish(&["geocoder", "tiles"], false), Published::New(_)), "modules changed");
        assert!(matches!(republish(&["geocoder", "tiles"], true), Published::New(_)), "forced");
        fs::write(work.path().join("reverse/points.bin"), b"new points").unwrap();
        assert!(matches!(republish(&["geocoder", "tiles"], false), Published::New(_)), "a segment changed");
        assert!(matches!(publish(&["geocoder", "tiles"], false), Published::UpToDate(_)));
    }
}

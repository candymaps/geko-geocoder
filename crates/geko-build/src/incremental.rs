//! Incremental generation (PROJECT.md §8.3). Each stage writes one output directory and, once it
//! succeeds, a stamp beside it (`<dir>.stamp.json`) with:
//!
//! - `key`: hash of the stage's code version and inputs. Raw downloads are fingerprinted by path,
//!   size and modification time; the outputs of an earlier stage, by their content digest.
//! - `output`: digest of the content of the directory. A later stage keys on it, so a stage that
//!   reruns and writes the same bytes (builds are deterministic) does not rerun the next ones.
//!
//! A stage reruns when its key changes or its stamp is missing. The stamp is removed before a stage
//! starts, so an interrupted or failed stage is never taken as up to date.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use geko_core::{GekoError, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stamp {
    pub key: String,
    pub output: String,
    /// Stage-specific figures to report when the stage is skipped (e.g. records staged).
    #[serde(default)]
    pub info: serde_json::Value,
}

/// Hash of a stage's inputs, built field by field.
pub struct Key(Sha256);

impl Key {
    pub fn new(stage: &str) -> Self {
        Self(Sha256::new()).add("stage", stage)
    }

    pub fn add(mut self, label: &str, value: &str) -> Self {
        for part in [label, value] {
            self.0.update((part.len() as u64).to_le_bytes());
            self.0.update(part.as_bytes());
        }
        self
    }

    /// Fingerprints files, or every file under a directory, by path, size and modification time.
    /// A missing path counts as an input too, so it appearing later reruns the stage.
    pub fn files(mut self, paths: &[PathBuf]) -> Result<Self> {
        for path in paths {
            let mut files = Vec::new();
            if path.is_dir() {
                walk(path, &mut files)?;
            } else {
                files.push(path.clone());
            }
            files.sort();
            for file in files {
                let fingerprint = match fs::metadata(&file) {
                    Ok(m) => {
                        let modified = m.modified()?.duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
                        format!("{} {modified}", m.len())
                    }
                    Err(_) => "missing".to_owned(),
                };
                self = self.add(&file.to_string_lossy(), &fingerprint);
            }
        }
        Ok(self)
    }

    /// Fingerprints inputs by the identity their source gave them (`FetchReport::identities`),
    /// which holds whether their files are there or not; inputs without one, by their files.
    pub fn inputs(mut self, paths: &[PathBuf], identities: &BTreeMap<PathBuf, String>) -> Result<Self> {
        for path in paths {
            self = match identities.get(path) {
                Some(identity) => self.add(&path.to_string_lossy(), &format!("identity {identity}")),
                None => self.files(std::slice::from_ref(path))?,
            };
        }
        Ok(self)
    }

    pub fn finish(self) -> String {
        hex::encode(self.0.finalize())
    }
}

pub const STAMP_SUFFIX: &str = ".stamp.json";

pub fn stamp_path(dir: &Path) -> PathBuf {
    let mut name = dir.file_name().unwrap_or_default().to_owned();
    name.push(STAMP_SUFFIX);
    dir.with_file_name(name)
}

/// The stamp of `dir`, if a stage completed there.
pub fn stamp(dir: &Path) -> Option<Stamp> {
    serde_json::from_slice(&fs::read(stamp_path(dir)).ok()?).ok()
}

/// The stamp of `dir` if its stage completed with this `key` and the directory is still there.
pub fn up_to_date(dir: &Path, key: &str) -> Option<Stamp> {
    stamp(dir).filter(|s| s.key == key && dir.is_dir())
}

/// Marks `dir` as out of date; call before a stage starts writing to it.
pub fn invalidate(dir: &Path) -> Result<()> {
    match fs::remove_file(stamp_path(dir)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// Records that the stage writing `dir` completed with `key`. `exclude` names entries of `dir`
/// left out of its digest: reports with timings, subdirectories written by other stages.
pub fn seal(dir: &Path, key: String, exclude: &[&str], info: serde_json::Value) -> Result<Stamp> {
    let stamp = Stamp { key, output: digest(dir, exclude)?, info };
    record(dir, &stamp)?;
    Ok(stamp)
}

/// Writes the stamp of `dir` atomically.
pub fn record(dir: &Path, stamp: &Stamp) -> Result<()> {
    let path = stamp_path(dir);
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(stamp).map_err(|e| GekoError::Other(e.into()))?)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// Digest of the output of `dir`: from its stamp if a stage sealed it, else from its content.
pub fn output_of(dir: &Path, exclude: &[&str]) -> Result<String> {
    match stamp(dir) {
        Some(s) => Ok(s.output),
        None => digest(dir, exclude),
    }
}

/// Hash of the relative path and content of every file under `dir`, except `exclude`.
pub fn digest(dir: &Path, exclude: &[&str]) -> Result<String> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if exclude.iter().any(|x| path.file_name().is_some_and(|n| n == *x)) {
            continue;
        }
        if path.is_dir() { walk(&path, &mut files)? } else { files.push(path) }
    }
    files.sort();
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    for file in files {
        let rel = file.strip_prefix(dir).unwrap_or(&file).to_string_lossy().replace('\\', "/");
        hasher.update((rel.len() as u64).to_le_bytes());
        hasher.update(rel.as_bytes());
        let mut reader = File::open(&file)?;
        hasher.update(reader.metadata()?.len().to_le_bytes());
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
    }
    Ok(hex::encode(hasher.finalize()))
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() { walk(&path, out)? } else { out.push(path) }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stage_is_up_to_date_only_after_sealing_with_the_same_key() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("ES-PM");
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("a.parquet"), b"a").unwrap();
        fs::write(dir.join("sub/b.bin"), b"b").unwrap();
        let key = Key::new("stage").add("code", "1").finish();

        assert!(up_to_date(&dir, &key).is_none());
        let sealed = seal(&dir, key.clone(), &[], serde_json::json!({"records": 2})).unwrap();
        assert!(tmp.path().join("ES-PM.stamp.json").is_file());
        assert_eq!(up_to_date(&dir, &key), Some(sealed.clone()));
        assert!(up_to_date(&dir, &Key::new("stage").add("code", "2").finish()).is_none());
        assert_eq!(output_of(&dir, &[]).unwrap(), sealed.output);

        invalidate(&dir).unwrap();
        assert!(up_to_date(&dir, &key).is_none());
        invalidate(&dir).unwrap();
    }

    #[test]
    fn the_digest_depends_on_content_and_names_but_not_on_excluded_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        fs::write(dir.join("a.parquet"), b"a").unwrap();
        fs::create_dir_all(dir.join("parts")).unwrap();
        let base = digest(dir, &["report.json", "parts"]).unwrap();

        fs::write(dir.join("report.json"), b"elapsed 12 ms").unwrap();
        fs::write(dir.join("parts/x"), b"x").unwrap();
        assert_eq!(digest(dir, &["report.json", "parts"]).unwrap(), base);

        fs::write(dir.join("a.parquet"), b"b").unwrap();
        assert_ne!(digest(dir, &["report.json", "parts"]).unwrap(), base);
        fs::write(dir.join("a.parquet"), b"a").unwrap();
        fs::rename(dir.join("a.parquet"), dir.join("b.parquet")).unwrap();
        assert_ne!(digest(dir, &["report.json", "parts"]).unwrap(), base);
    }

    #[test]
    fn file_keys_change_with_size_and_presence() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("spain.osm.pbf");
        let key = || Key::new("s").files(std::slice::from_ref(&file)).unwrap().finish();
        let missing = key();
        fs::write(&file, b"v1").unwrap();
        let v1 = key();
        assert_ne!(missing, v1);
        assert_eq!(key(), v1);
        fs::write(&file, b"v2 longer").unwrap();
        assert_ne!(key(), v1);
    }
}

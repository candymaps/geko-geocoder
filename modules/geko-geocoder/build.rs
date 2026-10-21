//! Code version of every workspace crate, for incremental generation (PROJECT.md §8.3): a hash of
//! its sources and those of the workspace crates it depends on, plus the entries of `Cargo.lock`
//! of the external crates they use. Exposed as
//! `GEKO_CODE_<CRATE>` (e.g. `GEKO_CODE_SOURCE_OSM`), so a build stage reruns exactly when the
//! code that produces its output changes, without version numbers to bump by hand.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

fn main() {
    let root = Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../..").canonicalize().unwrap();
    let manifest = |dir: &Path| -> toml::Table { fs::read_to_string(dir.join("Cargo.toml")).unwrap().parse().unwrap() };

    // Workspace crates by name, with the workspace crates each one depends on.
    let workspace = manifest(&root);
    let paths: BTreeMap<String, PathBuf> = workspace["workspace"]["dependencies"]
        .as_table()
        .unwrap()
        .iter()
        .filter_map(|(name, dep)| Some((name.clone(), root.join(dep.get("path")?.as_str()?))))
        .collect();
    // Normal and build dependencies only: tests and benchmarks do not produce stage outputs.
    let mut external: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let deps: BTreeMap<&str, Vec<&str>> = paths
        .iter()
        .map(|(name, dir)| {
            let m = manifest(dir);
            let keys: Vec<String> = ["dependencies", "build-dependencies"]
                .iter()
                .filter_map(|s| m.get(*s).and_then(|d| d.as_table()))
                .flat_map(|d| d.iter().map(|(k, v)| v.get("package").and_then(|p| p.as_str()).unwrap_or(k).to_owned()))
                .collect();
            let direct = keys.iter().filter_map(|k| paths.get_key_value(k).map(|(k, _)| k.as_str())).collect();
            external.insert(name.as_str(), keys.into_iter().filter(|k| !paths.contains_key(k)).collect());
            (name.as_str(), direct)
        })
        .collect();
    let lock = Lock::read(&root.join("Cargo.lock"));

    for name in deps.keys() {
        let mut closure = BTreeSet::new();
        let mut todo = vec![*name];
        while let Some(n) = todo.pop() {
            if closure.insert(n) {
                todo.extend(&deps[n]);
            }
        }
        let mut hasher = Sha256::new();
        // The locked versions of the external crates the closure uses, not the whole lock: a
        // dependency added to another crate must not rerun this one's stages.
        let entries = lock.closure(closure.iter().flat_map(|n| &external[n]).map(String::as_str));
        for entry in entries {
            hasher.update((entry.len() as u64).to_le_bytes());
            hasher.update(entry.as_bytes());
        }
        for n in closure {
            let dir = &paths[n];
            let mut files = vec![dir.join("Cargo.toml")];
            collect(&dir.join("src"), &mut files);
            // A country pack (PROJECT.md §2.4) embeds its data: it is part of its code.
            collect(&dir.join("data"), &mut files);
            files.extend(Some(dir.join("pack.toml")).filter(|p| p.is_file()));
            files.sort();
            for f in files {
                let rel = f.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
                let bytes = fs::read(&f).unwrap();
                hasher.update((rel.len() as u64).to_le_bytes());
                hasher.update(rel.as_bytes());
                hasher.update((bytes.len() as u64).to_le_bytes());
                hasher.update(&bytes);
            }
        }
        let hash: String = hasher.finalize()[..8].iter().map(|b| format!("{b:02x}")).collect();
        println!("cargo:rustc-env=GEKO_CODE_{}={hash}", name.to_uppercase().replace('-', "_"));
    }

    println!("cargo:rerun-if-changed={}", root.join("Cargo.toml").display());
    println!("cargo:rerun-if-changed={}", root.join("Cargo.lock").display());
    for dir in paths.values() {
        println!("cargo:rerun-if-changed={}", dir.join("Cargo.toml").display());
        println!("cargo:rerun-if-changed={}", dir.join("src").display());
        println!("cargo:rerun-if-changed={}", dir.join("data").display());
        println!("cargo:rerun-if-changed={}", dir.join("pack.toml").display());
    }
}

/// The packages of `Cargo.lock`, each as its whole entry, and the dependencies of each.
struct Lock {
    packages: Vec<(String, String, String, Vec<String>)>,
}

impl Lock {
    fn read(path: &Path) -> Self {
        let table: toml::Table = fs::read_to_string(path).unwrap_or_default().parse().unwrap_or_default();
        let packages = table
            .get("package")
            .and_then(|p| p.as_array())
            .into_iter()
            .flatten()
            .filter_map(|p| {
                let (name, version) = (p.get("name")?.as_str()?.to_owned(), p.get("version")?.as_str()?.to_owned());
                let deps = p.get("dependencies").and_then(|d| d.as_array()).into_iter().flatten().filter_map(|d| d.as_str().map(str::to_owned)).collect();
                Some((name, version, p.to_string(), deps))
            })
            .collect();
        Self { packages }
    }

    /// Packages matching a lock dependency: `name`, `name version` or `name version (source)`.
    fn find<'a>(&'a self, dep: &'a str) -> impl Iterator<Item = usize> + 'a {
        let mut parts = dep.split(' ');
        let (name, version) = (parts.next().unwrap_or(""), parts.next());
        self.packages.iter().enumerate().filter(move |(_, p)| p.0 == name && version.is_none_or(|v| p.1 == v)).map(|(i, _)| i)
    }

    /// The entries of `roots` and of everything they depend on, in lock order.
    fn closure<'a>(&self, roots: impl Iterator<Item = &'a str>) -> Vec<&str> {
        let mut seen = BTreeSet::new();
        let mut todo: Vec<usize> = roots.flat_map(|r| self.find(r).collect::<Vec<_>>()).collect();
        while let Some(i) = todo.pop() {
            if seen.insert(i) {
                todo.extend(self.packages[i].3.iter().flat_map(|d| self.find(d)));
            }
        }
        seen.into_iter().map(|i| self.packages[i].2.as_str()).collect()
    }
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let path = e.path();
        if path.is_dir() { collect(&path, out) } else { out.push(path) }
    }
}

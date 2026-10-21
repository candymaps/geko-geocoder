//! Bundle manifest (PROJECT.md §12): what a published, immutable bundle contains.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{GekoError, Result};

pub const MANIFEST: &str = "manifest.json";
/// Symlink in the bundles directory pointing at the version being served.
pub const CURRENT: &str = "current";
/// File in the bundles directory holding the name of the current version: a symlink cannot be
/// read over HTTP, where another machine pulls bundles from (`geko serve` with `bundles_url`).
pub const CURRENT_VERSION: &str = "current.version";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderFormat {
    pub provider: String,
    pub version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    /// Relative to the bundle root.
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    pub country: String,
    /// Relative to the bundle root, e.g. `segments/es`.
    pub dir: String,
    pub geocode: ProviderFormat,
    pub reverse: ProviderFormat,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LicenseNotice {
    pub source: String,
    pub name: String,
    pub attribution: String,
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleManifest {
    pub version: String,
    pub created_unix: u64,
    pub modules: Vec<String>,
    pub segments: Vec<Segment>,
    pub licenses: Vec<LicenseNotice>,
    pub artifacts: Vec<Artifact>,
}

/// The version `current` points at, if any: a symlink on Unix, a file holding the name elsewhere.
pub fn current_version(bundles: &Path) -> Option<String> {
    let link = bundles.join(CURRENT);
    if let Ok(target) = std::fs::read_link(&link) {
        return target.file_name().map(|n| n.to_string_lossy().into_owned());
    }
    std::fs::read_to_string(link).ok().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

impl BundleManifest {
    pub fn read(bundle: &Path) -> Result<Self> {
        let path = bundle.join(MANIFEST);
        let bytes = std::fs::read(&path).map_err(|e| GekoError::Data(format!("{}: {e}", path.display())))?;
        serde_json::from_slice(&bytes).map_err(|e| GekoError::Data(format!("{}: {e}", path.display())))
    }

    pub fn write(&self, bundle: &Path) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| GekoError::Other(e.into()))?;
        std::fs::write(bundle.join(MANIFEST), bytes)?;
        Ok(())
    }
}

//! Download of the packs of the scope from the registry where they are published, by default
//! `ghcr.io/candymaps/geko/countries/<country>` (PROJECT.md §2.4). The tag is the format this binary
//! reads (`format-1`), so that each geko gets the newest pack it understands.
//!
//! A pack copied by hand into the folder of the packs (`make install`, without the `.digest` that a
//! download leaves) is used as it is: geko never replaces what it did not download.

use std::path::{Path, PathBuf};

use geko_core::country::CountryCode;
use geko_core::{GekoError, Result};
use geko_fetch::oci::{OciClient, Reference};

use crate::signature::{self, FORMAT};

pub const DEFAULT_REGISTRY: &str = "ghcr.io/candymaps/geko/countries";
/// Beside the files of a downloaded pack: the digest of its manifest in the registry.
pub const DIGEST_FILE: &str = ".digest";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Downloaded,
    UpToDate,
    /// The local copy is used: copied by hand, offline, or the registry could not be asked.
    Local(String),
}

pub fn reference(registry: &str, country: CountryCode) -> Result<Reference> {
    Reference::parse(&format!("{}/{}:format-{FORMAT}", registry.trim_end_matches('/'), country.lowercase()))
}

/// Whether the pack in `dir` was downloaded by geko.
pub fn downloaded(dir: &Path) -> bool {
    dir.join(DIGEST_FILE).is_file()
}

/// Makes sure that `packs/<country>` holds the pack of `country` published in `registry`.
pub fn ensure(packs: &Path, country: CountryCode, registry: &str, offline: bool) -> Result<Outcome> {
    let dir = packs.join(country.lowercase());
    let local = dir.join("pack.toml").is_file();
    if local && !downloaded(&dir) {
        return Ok(Outcome::Local(format!("{} was copied by hand", dir.display())));
    }
    if offline {
        return match local {
            true => Ok(Outcome::Local("offline".into())),
            false => Err(GekoError::Config(format!("offline mode: no country pack for {country} in {}", dir.display()))),
        };
    }
    let reference = reference(registry, country)?;
    let client = OciClient::new();
    let digest = match client.digest(&reference) {
        Ok(Some(digest)) => digest,
        Ok(None) if local => {
            tracing::warn!(%country, %reference, "the country pack is no longer published; using the local copy");
            return Ok(Outcome::Local("not published".into()));
        }
        Ok(None) => {
            return Err(GekoError::Config(format!(
                "no country pack for {country}: {reference} is not published, or not public"
            )));
        }
        Err(e) if local => {
            tracing::warn!(%country, error = %e, "cannot reach the registry of the country packs; using the local copy");
            return Ok(Outcome::Local("registry unreachable".into()));
        }
        Err(e) => return Err(GekoError::Data(format!("the country pack of {country} cannot be downloaded: {e}"))),
    };
    if local && std::fs::read_to_string(dir.join(DIGEST_FILE)).is_ok_and(|d| d.trim() == digest) {
        return Ok(Outcome::UpToDate);
    }
    let artifact = client
        .pull(&reference)?
        .ok_or_else(|| GekoError::Data(format!("{reference} disappeared while it was downloaded")))?;
    let partial = packs.join(format!(".{}.partial", country.lowercase()));
    if partial.exists() {
        std::fs::remove_dir_all(&partial)?;
    }
    for (path, bytes) in &artifact.files {
        let file = partial.join(path);
        std::fs::create_dir_all(file.parent().expect("a file has a folder"))?;
        std::fs::write(file, bytes)?;
    }
    if let Err(e) = signature::verify_trusted(&partial) {
        let _ = std::fs::remove_dir_all(&partial);
        return Err(GekoError::Data(format!("{reference} is rejected: {e}")));
    }
    std::fs::write(partial.join(DIGEST_FILE), &artifact.digest)?;
    replace(&partial, &dir)?;
    tracing::info!(%country, %reference, "country pack downloaded");
    Ok(Outcome::Downloaded)
}

/// Swaps `new` into `dir`, keeping the old copy until the new one is in place.
fn replace(new: &Path, dir: &Path) -> Result<()> {
    let old: PathBuf = dir.with_file_name(format!(".{}.old", dir.file_name().unwrap_or_default().to_string_lossy()));
    if old.exists() {
        std::fs::remove_dir_all(&old)?;
    }
    if dir.exists() {
        std::fs::rename(dir, &old)?;
    }
    std::fs::rename(new, dir)?;
    if old.exists() {
        std::fs::remove_dir_all(&old)?;
    }
    Ok(())
}

/// Removes the downloaded packs of the countries not in `keep` (all of them if `keep` is None).
/// Packs copied by hand stay. Returns the folders removed.
pub fn purge(packs: &Path, keep: Option<&[CountryCode]>) -> Result<Vec<PathBuf>> {
    let mut removed = Vec::new();
    let Ok(entries) = std::fs::read_dir(packs) else { return Ok(removed) };
    for entry in entries {
        let dir = entry?.path();
        let name = dir.file_name().unwrap_or_default().to_string_lossy().into_owned();
        if name.starts_with('.') || !downloaded(&dir) {
            continue;
        }
        if keep.is_some_and(|k| k.iter().any(|c| c.lowercase() == name)) {
            continue;
        }
        std::fs::remove_dir_all(&dir)?;
        removed.push(dir);
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn country(code: &str) -> CountryCode {
        CountryCode::new(code).unwrap()
    }

    #[test]
    fn references_name_the_format() {
        let r = reference("ghcr.io/candymaps/geko/countries/", country("ES")).unwrap();
        assert_eq!(r.to_string(), format!("ghcr.io/candymaps/geko/countries/es:format-{FORMAT}"));
    }

    #[test]
    fn a_pack_copied_by_hand_is_used_as_it_is_and_never_purged() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("es")).unwrap();
        std::fs::write(tmp.path().join("es/pack.toml"), "").unwrap();
        let outcome = ensure(tmp.path(), country("ES"), "127.0.0.1:1/none", false).unwrap();
        assert!(matches!(outcome, Outcome::Local(_)), "{outcome:?}");
        for dir in ["lu", "fr"] {
            std::fs::create_dir_all(tmp.path().join(dir)).unwrap();
            std::fs::write(tmp.path().join(dir).join(DIGEST_FILE), "sha256:x").unwrap();
        }
        let removed = purge(tmp.path(), Some(&[country("LU")])).unwrap();
        assert_eq!(removed, [tmp.path().join("fr")]);
        assert_eq!(purge(tmp.path(), None).unwrap(), [tmp.path().join("lu")]);
        assert!(tmp.path().join("es/pack.toml").exists());
    }

    #[test]
    fn offline_without_the_pack_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let err = ensure(tmp.path(), country("FR"), DEFAULT_REGISTRY, true).unwrap_err();
        assert!(err.to_string().contains("offline mode: no country pack for FR"), "{err}");
    }

    #[test]
    fn an_unreachable_registry_without_a_local_copy_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let err = ensure(tmp.path(), country("FR"), "127.0.0.1:1/countries", false).unwrap_err();
        assert!(err.to_string().contains("cannot be downloaded"), "{err}");
    }
}

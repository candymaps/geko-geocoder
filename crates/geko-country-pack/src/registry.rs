//! Download of the packs of the scope from the registry where they are published, by default
//! `ghcr.io/candymaps/geko/countries/<country>` (PROJECT.md §2.4), with `geko_artifact::registry`.
//! The tag is the format this binary reads (`format-2`), so that each geko gets the newest pack it
//! understands.
//!
//! A pack copied by hand into the folder of the packs (`make install`, without the `.digest` that a
//! download leaves) is used as it is: geko never replaces what it did not download.

use std::path::{Path, PathBuf};

use geko_core::Result;
use geko_core::country::CountryCode;
use geko_fetch::oci::Reference;

use crate::signature::{FORMAT, PACK};

pub use geko_artifact::registry::{DIGEST_FILE, Outcome, downloaded};

pub const DEFAULT_REGISTRY: &str = "ghcr.io/candymaps/geko/countries";

pub fn reference(registry: &str, country: CountryCode) -> Result<Reference> {
    Reference::parse(&format!("{}/{}:format-{FORMAT}", registry.trim_end_matches('/'), country.lowercase()))
}

/// Makes sure that `packs/<country>` holds the pack of `country` published in `registry`.
pub fn ensure(packs: &Path, country: CountryCode, registry: &str, offline: bool) -> Result<Outcome> {
    let dir = packs.join(country.lowercase());
    let reference = reference(registry, country)?;
    geko_artifact::registry::ensure(&dir, &reference, PACK, "pack.toml", offline, &format!("country pack for {country}"))
}

/// Removes the downloaded packs of the countries not in `keep` (all of them if `keep` is None).
/// Packs copied by hand stay. Returns the folders removed.
pub fn purge(packs: &Path, keep: Option<&[CountryCode]>) -> Result<Vec<PathBuf>> {
    geko_artifact::registry::purge(packs, |dir| {
        let name = dir.file_name().unwrap_or_default().to_string_lossy();
        keep.is_some_and(|k| k.iter().any(|c| c.lowercase() == name))
    })
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

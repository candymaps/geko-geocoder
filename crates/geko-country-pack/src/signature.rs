//! The signature of a published pack (`signature.json`, `geko_artifact::signature`): the format of
//! the pack, the hash of each of its files and an ed25519 signature over both by a key this binary
//! trusts.

use std::path::Path;

use ed25519_dalek::{SigningKey, VerifyingKey};
use geko_artifact::signature::Kind;
use geko_core::Result;

pub use geko_artifact::signature::{SIGNATURE_FILE, files_of, signing_key};

/// The format of the packs this binary reads. A pack of a newer format needs a newer geko.
/// Raised whenever an older geko cannot read the packs any more (2: `version` in `pack.toml`).
pub const FORMAT: u32 = 2;

pub const PACK: Kind = Kind { domain: "geko-country-pack", format: FORMAT, name: "country pack" };

/// Writes `signature.json` for the pack in `dir`.
pub fn sign(dir: &Path, key: &SigningKey) -> Result<()> {
    geko_artifact::signature::sign(dir, PACK, key)
}

/// Checks that the pack in `dir` is of a format this binary reads, that its files are exactly
/// those signed, and that one of `keys` signed them.
pub fn verify(dir: &Path, keys: &[VerifyingKey]) -> Result<()> {
    geko_artifact::signature::verify(dir, PACK, keys)
}

/// `verify` with the keys this binary trusts.
pub fn verify_trusted(dir: &Path) -> Result<()> {
    geko_artifact::signature::verify_trusted(dir, PACK)
}

//! The signature of a published pack (`signature.json`): the format of the pack, the hash of each
//! of its files and an ed25519 signature over both by a key this binary trusts. A pack from a
//! registry is used only if its signature holds, so that nobody who can push to the registry can
//! slip in a dictionary or a source pointing elsewhere.

use std::collections::BTreeMap;
use std::path::Path;

use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use geko_core::{GekoError, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const SIGNATURE_FILE: &str = "signature.json";

/// The format of the packs this binary reads. A pack of a newer format needs a newer geko.
/// Raised whenever an older geko cannot read the packs any more (2: `version` in `pack.toml`).
pub const FORMAT: u32 = 2;

/// The public keys whose signatures this binary trusts (hex). The private key is a secret of the
/// CI that publishes the packs (`GEKO_PACK_SIGNING_KEY`).
const TRUSTED_KEYS: &[&str] = &["26b041ad43868090b3411186f8ed492442ffd8c8b32c9ca1215d25ae98fe8f79"];

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Signature {
    pub format: u32,
    /// The sha256 of every file of the pack but this one, by relative path.
    pub files: BTreeMap<String, String>,
    /// ed25519 over `message(format, files)`, hex.
    pub signature: String,
}

fn message(format: u32, files: &BTreeMap<String, String>) -> Vec<u8> {
    let mut text = format!("geko-country-pack\nformat {format}\n");
    for (path, sha) in files {
        text.push_str(&format!("{sha} {path}\n"));
    }
    text.into_bytes()
}

/// The sha256 of every file under `dir`, by relative path with `/`: all but the signature and
/// hidden files (what geko keeps beside a downloaded pack).
pub fn files_of(dir: &Path) -> Result<BTreeMap<String, String>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) -> Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            if path.is_dir() {
                walk(root, &path, out)?;
            } else {
                let rel = path.strip_prefix(root).expect("under the root").to_string_lossy().replace('\\', "/");
                if rel != SIGNATURE_FILE {
                    out.insert(rel, hex::encode(Sha256::digest(std::fs::read(&path)?)));
                }
            }
        }
        Ok(())
    }
    let mut files = BTreeMap::new();
    walk(dir, dir, &mut files)?;
    Ok(files)
}

/// Writes `signature.json` for the pack in `dir`.
pub fn sign(dir: &Path, key: &SigningKey) -> Result<()> {
    let files = files_of(dir)?;
    let signature = hex::encode(key.sign(&message(FORMAT, &files)).to_bytes());
    let json = serde_json::to_vec_pretty(&Signature { format: FORMAT, files, signature }).expect("serializable");
    std::fs::write(dir.join(SIGNATURE_FILE), json)?;
    Ok(())
}

/// Checks that the pack in `dir` is of a format this binary reads, that its files are exactly
/// those signed, and that one of `keys` signed them.
pub fn verify(dir: &Path, keys: &[VerifyingKey]) -> Result<()> {
    let invalid = |msg: String| Err(GekoError::Data(format!("country pack {}: {msg}", dir.display())));
    let Ok(text) = std::fs::read(dir.join(SIGNATURE_FILE)) else { return invalid(format!("no {SIGNATURE_FILE}")) };
    let Ok(sig) = serde_json::from_slice::<Signature>(&text) else { return invalid(format!("invalid {SIGNATURE_FILE}")) };
    if sig.format > FORMAT {
        return invalid(format!("format {} needs a newer geko (this one reads up to {FORMAT})", sig.format));
    }
    if files_of(dir)? != sig.files {
        return invalid("its files are not those signed".into());
    }
    let bytes: Option<[u8; 64]> = hex::decode(&sig.signature).ok().and_then(|b| b.try_into().ok());
    let Some(bytes) = bytes else { return invalid("malformed signature".into()) };
    let signature = ed25519_dalek::Signature::from_bytes(&bytes);
    let message = message(sig.format, &sig.files);
    if !keys.iter().any(|k| k.verify(&message, &signature).is_ok()) {
        return invalid("not signed by a trusted key".into());
    }
    Ok(())
}

/// `verify` with the keys this binary trusts.
pub fn verify_trusted(dir: &Path) -> Result<()> {
    verify(dir, &trusted_keys())
}

fn trusted_keys() -> Vec<VerifyingKey> {
    TRUSTED_KEYS
        .iter()
        .filter_map(|k| hex::decode(k).ok()?.try_into().ok())
        .filter_map(|b: [u8; 32]| VerifyingKey::from_bytes(&b).ok())
        .collect()
}

/// A signing key from its 32 bytes in hex.
pub fn signing_key(hex_key: &str) -> Result<SigningKey> {
    let bytes: [u8; 32] = hex::decode(hex_key.trim())
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| GekoError::Config("a signing key is 32 bytes in hex".into()))?;
    Ok(SigningKey::from_bytes(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(dir: &Path) {
        std::fs::create_dir_all(dir.join("data")).unwrap();
        std::fs::write(dir.join("pack.toml"), "country = \"ES\"").unwrap();
        std::fs::write(dir.join("data/words.toml"), "connectors = []").unwrap();
        std::fs::write(dir.join(".digest"), "sha256:x").unwrap();
    }

    #[test]
    fn a_signed_pack_verifies_and_a_changed_one_does_not() {
        let tmp = tempfile::tempdir().unwrap();
        pack(tmp.path());
        let key = signing_key(&"07".repeat(32)).unwrap();
        let other = signing_key(&"08".repeat(32)).unwrap();
        sign(tmp.path(), &key).unwrap();
        assert!(verify(tmp.path(), &[key.verifying_key()]).is_ok());
        assert!(verify(tmp.path(), &[other.verifying_key()]).is_err(), "another key");

        std::fs::write(tmp.path().join("data/words.toml"), "connectors = [\"de\"]").unwrap();
        assert!(verify(tmp.path(), &[key.verifying_key()]).is_err(), "a changed file");
        sign(tmp.path(), &key).unwrap();
        std::fs::write(tmp.path().join("data/extra.toml"), "").unwrap();
        assert!(verify(tmp.path(), &[key.verifying_key()]).is_err(), "an added file");
    }

    #[test]
    fn a_newer_format_needs_a_newer_geko() {
        let tmp = tempfile::tempdir().unwrap();
        pack(tmp.path());
        let key = signing_key(&"07".repeat(32)).unwrap();
        let files = files_of(tmp.path()).unwrap();
        let signature = hex::encode(key.sign(&message(FORMAT + 1, &files)).to_bytes());
        let sig = Signature { format: FORMAT + 1, files, signature };
        std::fs::write(tmp.path().join(SIGNATURE_FILE), serde_json::to_vec(&sig).unwrap()).unwrap();
        let err = verify(tmp.path(), &[key.verifying_key()]).unwrap_err();
        assert!(err.to_string().contains("needs a newer geko"), "{err}");
    }
}

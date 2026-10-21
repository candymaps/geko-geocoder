//! Signs the country packs before they are published (`ci/packs.sh publish`).
//!
//! ```text
//! geko-pack-sign keygen <key file>   a new key: the private one to <key file>, the public one to stdout
//! geko-pack-sign sign <pack folder>  writes signature.json with the key in GEKO_PACK_SIGNING_KEY (hex)
//! geko-pack-sign verify <pack folder>
//! geko-pack-sign format              the format of the packs this geko reads: the tag they are published with
//! geko-pack-sign version <pack folder>  the version of the pack (`pack.toml`): its tag pack-<version>
//! ```

use std::io::Read;
use std::path::Path;
use std::process::ExitCode;

use geko_country_pack::signature;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["keygen", file] => keygen(Path::new(file)),
        ["sign", dir] => std::env::var("GEKO_PACK_SIGNING_KEY")
            .map_err(|_| "GEKO_PACK_SIGNING_KEY is not set".to_owned())
            .and_then(|key| signature::signing_key(&key).map_err(|e| e.to_string()))
            .and_then(|key| signature::sign(Path::new(dir), &key).map_err(|e| e.to_string())),
        ["verify", dir] => signature::verify_trusted(Path::new(dir)).map_err(|e| e.to_string()),
        ["format"] => {
            println!("{}", signature::FORMAT);
            Ok(())
        }
        ["version", dir] => geko_country_pack::CountryPack::load(Path::new(dir))
            .map(|pack| println!("{}", pack.text.version))
            .map_err(|e| e.to_string()),
        _ => Err("usage: geko-pack-sign keygen <key file> | sign <pack folder> | verify <pack folder> | format | version <pack folder>"
            .to_owned()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn keygen(file: &Path) -> Result<(), String> {
    if file.exists() {
        return Err(format!("{} already exists", file.display()));
    }
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes)).map_err(|e| e.to_string())?;
    let key = signature::signing_key(&hex::encode(bytes)).map_err(|e| e.to_string())?;
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(file, hex::encode(key.to_bytes())).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    }
    println!("{}", hex::encode(key.verifying_key().to_bytes()));
    Ok(())
}

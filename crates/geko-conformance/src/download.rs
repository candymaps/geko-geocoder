//! Conformance of `Downloader`, against a local HTTP server: what it writes, the checksums it
//! verifies and that a failed download leaves the destination untouched.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;

use geko_core::ports::{Checksum, DownloadRequest, Downloader};
use sha2::{Digest, Sha256};

const BODY: &[u8] = b"geko conformance payload\n";

/// Serves `BODY` at `/file` and 404 elsewhere, until the process ends. Returns its base URL.
fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            if reader.read_line(&mut request).is_err() {
                continue;
            }
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                line.clear();
            }
            let path = request.split_whitespace().nth(1).unwrap_or("");
            let (status, body): (&str, &[u8]) = if path == "/file" { ("200 OK", BODY) } else { ("404 Not Found", b"") };
            let head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let mut stream = stream;
            let _ = stream.write_all(head.as_bytes()).and_then(|_| stream.write_all(body));
        }
    });
    format!("http://{addr}")
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// `dl` must download over plain HTTP from localhost (no proxy).
pub fn check(dl: &dyn Downloader, scratch: &Path) {
    let base = serve();
    let request = |path: &str, checksum: Option<Checksum>| DownloadRequest { url: format!("{base}{path}"), checksum };

    // A download writes the body and reports its digest and size.
    let dest = scratch.join("file");
    let outcome = dl.fetch(&request("/file", Some(Checksum::Sha256(sha256(BODY)))), &dest).expect("download");
    assert!(outcome.downloaded);
    assert_eq!(std::fs::read(&dest).unwrap(), BODY);
    assert_eq!(outcome.sha256, sha256(BODY));
    assert_eq!(outcome.size, BODY.len() as u64);

    // A wrong checksum or a missing file is an error and leaves the existing copy untouched.
    let kept = scratch.join("kept");
    std::fs::write(&kept, b"previous version").unwrap();
    assert!(dl.fetch(&request("/file", Some(Checksum::Sha256("0".repeat(64)))), &kept).is_err(), "wrong checksum");
    assert_eq!(std::fs::read(&kept).unwrap(), b"previous version");
    assert!(dl.fetch(&request("/missing", None), &kept).is_err(), "404");
    assert_eq!(std::fs::read(&kept).unwrap(), b"previous version");
    let never = scratch.join("never");
    assert!(dl.fetch(&request("/missing", None), &never).is_err());
    assert!(!never.exists(), "a failed download creates nothing");
}

/// A download whose file was removed after publishing (`[data] keep_downloads = false`): `keep`,
/// which keeps removed downloads and checks for no updates, reports it kept, with the digest it
/// had, without bringing it back; `restore` (`keeps_removed` false) downloads it again.
pub fn check_removed(keep: &dyn Downloader, restore: &dyn Downloader, scratch: &Path) {
    assert!(keep.keeps_removed() && !keep.checks_for_updates());
    assert!(!restore.keeps_removed());
    let base = serve();
    let request = DownloadRequest { url: format!("{base}/file"), checksum: None };
    let dest = scratch.join("removed");
    assert!(restore.fetch(&request, &dest).expect("download").downloaded);
    std::fs::remove_file(&dest).unwrap();

    let kept = keep.fetch(&request, &dest).expect("a removed download is kept");
    assert!(!kept.downloaded);
    assert_eq!((kept.sha256.as_str(), kept.size), (sha256(BODY).as_str(), BODY.len() as u64));
    assert!(!dest.exists(), "kept as removed");

    assert!(restore.fetch(&request, &dest).expect("restored").downloaded);
    assert_eq!(std::fs::read(&dest).unwrap(), BODY);
}

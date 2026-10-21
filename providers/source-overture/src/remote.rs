//! The places of an Overture release, read in parts: the footer of each Parquet file, then only
//! the row groups whose bounding box meets the scope, written at their offsets into a local
//! sparse copy that a Parquet reader opens as the whole file.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use geko_core::ports::{DownloadRequest, Downloader};
use geko_core::{GekoError, Result};
use parquet::file::metadata::{ParquetMetaData, ParquetMetaDataReader, RowGroupMetaData};
use parquet::file::statistics::Statistics;
use serde::{Deserialize, Serialize};

use crate::BBox;

/// Places of a release, under the bucket's base URL.
pub fn places_prefix(release: &str) -> String {
    format!("release/{release}/theme=places/type=place/")
}

fn list_url(base: &str, prefix: &str, delimiter: bool) -> String {
    let encoded = prefix.replace('=', "%3D");
    format!("{base}/?list-type=2&prefix={encoded}{}", if delimiter { "&delimiter=/" } else { "" })
}

fn tags<'a>(xml: &'a str, tag: &str) -> Vec<&'a str> {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    xml.split(&open).skip(1).filter_map(|s| s.split_once(&close).map(|(v, _)| v)).collect()
}

/// The newest release in the bucket (`release/<YYYY-MM-DD.N>/`).
pub fn latest_release(dl: &dyn Downloader, base: &str, raw: &Path) -> Result<String> {
    let path = raw.join("releases.xml");
    dl.fetch(&DownloadRequest { url: list_url(base, "release/", true), checksum: None }, &path)?;
    let xml = fs::read_to_string(&path)?;
    tags(&xml, "Prefix")
        .into_iter()
        .filter_map(|p| p.strip_prefix("release/")?.strip_suffix('/'))
        .filter(|r| !r.is_empty())
        .max()
        .map(str::to_owned)
        .ok_or_else(|| GekoError::Data(format!("{base}: no Overture release listed")))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteFile {
    /// Name of the file, the last part of its key.
    pub name: String,
    pub url: String,
    pub size: u64,
}

/// The Parquet files of the places of `release`, by name.
pub fn place_files(dl: &dyn Downloader, base: &str, release: &str, dir: &Path) -> Result<Vec<RemoteFile>> {
    let path = dir.join("files.xml");
    dl.fetch(&DownloadRequest { url: list_url(base, &places_prefix(release), false), checksum: None }, &path)?;
    let xml = fs::read_to_string(&path)?;
    if xml.contains("<IsTruncated>true</IsTruncated>") {
        return Err(GekoError::Data(format!("{base}: the listing of {release} is longer than one page")));
    }
    let mut files: Vec<RemoteFile> = xml
        .split("<Contents>")
        .skip(1)
        .filter_map(|c| {
            let key = *tags(c, "Key").first()?;
            let size = tags(c, "Size").first()?.parse().ok()?;
            let name = key.rsplit('/').next()?.to_owned();
            name.ends_with(".parquet").then(|| RemoteFile { url: format!("{base}/{key}"), name, size })
        })
        .collect();
    files.sort_by(|a, b| a.name.cmp(&b.name));
    if files.is_empty() {
        return Err(GekoError::Data(format!("{base}: no places files in release {release}")));
    }
    Ok(files)
}

/// Row groups of a local sparse copy that hold data, written once they are complete.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Fetched {
    pub size: u64,
    pub row_groups: BTreeSet<usize>,
    /// The boxes (xmin, ymin, xmax, ymax) whose row groups are all here, so that a copy removed
    /// after publishing is known to cover them without its footer.
    #[serde(default)]
    pub boxes: Vec<[f64; 4]>,
}

pub fn fetched_path(file: &Path) -> PathBuf {
    let mut name = file.file_name().unwrap_or_default().to_os_string();
    name.push(".fetched.json");
    file.with_file_name(name)
}

pub fn read_fetched(file: &Path) -> Fetched {
    fs::read(fetched_path(file)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn write_fetched(file: &Path, fetched: &Fetched) -> Result<()> {
    let path = fetched_path(file);
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, serde_json::to_vec(fetched).map_err(|e| GekoError::Other(e.into()))?)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// The footer of a Parquet file: its last 8 bytes give the length of the metadata before them.
fn footer(dl: &dyn Downloader, file: &RemoteFile) -> Result<(u64, Vec<u8>)> {
    let err = |what: &str| GekoError::Data(format!("{}: {what}", file.url));
    if file.size < 12 {
        return Err(err("too small for Parquet"));
    }
    let tail = dl.fetch_range(&file.url, file.size - 8, 8)?;
    if &tail[4..] != b"PAR1" {
        return Err(err("not a Parquet file"));
    }
    let len = u32::from_le_bytes(tail[..4].try_into().expect("four bytes")) as u64;
    let start = file.size.checked_sub(8 + len).ok_or_else(|| err("metadata longer than the file"))?;
    let mut bytes = dl.fetch_range(&file.url, start, len)?;
    bytes.extend_from_slice(&tail);
    Ok((start, bytes))
}


/// The bounding box of a row group from the statistics of its `bbox` columns.
pub fn row_group_box(rg: &RowGroupMetaData) -> Option<BBox> {
    let stat = |path: &str, max: bool| -> Option<f64> {
        let column = rg.columns().iter().find(|c| c.column_path().string() == path)?;
        match column.statistics()? {
            Statistics::Double(s) => if max { s.max_opt() } else { s.min_opt() }.copied(),
            Statistics::Float(s) => if max { s.max_opt() } else { s.min_opt() }.map(|v| *v as f64),
            _ => None,
        }
    };
    Some(BBox { xmin: stat("bbox.xmin", false)?, ymin: stat("bbox.ymin", false)?, xmax: stat("bbox.xmax", true)?, ymax: stat("bbox.ymax", true)? })
}

/// Row groups whose box meets any of `boxes`.
pub fn wanted(metadata: &ParquetMetaData, boxes: &[BBox]) -> Vec<usize> {
    metadata
        .row_groups()
        .iter()
        .enumerate()
        .filter(|(_, rg)| row_group_box(rg).is_some_and(|b| boxes.iter().any(|x| x.meets(&b))))
        .map(|(i, _)| i)
        .collect()
}

/// Bytes of a row group in its file: from its first page to the end of its last column chunk.
fn byte_range(rg: &RowGroupMetaData) -> (u64, u64) {
    let start = rg.columns().iter().map(|c| c.dictionary_page_offset().unwrap_or(c.data_page_offset()).min(c.data_page_offset())).min().unwrap_or(0);
    let end = rg
        .columns()
        .iter()
        .map(|c| c.dictionary_page_offset().unwrap_or(c.data_page_offset()).min(c.data_page_offset()) + c.compressed_size())
        .max()
        .unwrap_or(0);
    (start as u64, (end - start).max(0) as u64)
}

/// Brings into the local sparse copy of `file` the row groups that meet `boxes` and are not
/// there yet. Returns how many bytes it downloaded.
pub fn fetch_file(dl: &dyn Downloader, file: &RemoteFile, local: &Path, boxes: &[BBox]) -> Result<u64> {
    let mut fetched = read_fetched(local);
    let keys: Vec<[f64; 4]> = boxes.iter().map(|b| [b.xmin, b.ymin, b.xmax, b.ymax]).collect();
    let covered = fetched.size == file.size && keys.iter().all(|k| fetched.boxes.contains(k));
    if !local.exists() && covered && dl.keeps_removed() {
        // Removed after publishing, with every row group that the scope needs.
        return Ok(0);
    }
    if fetched.size != file.size || !local.is_file() {
        // Another version of the file, or none yet: start over.
        let _ = fs::remove_file(local);
        fetched = Fetched { size: file.size, ..Fetched::default() };
    }
    let mut downloaded = 0;
    if !local.is_file() {
        let (start, bytes) = footer(dl, file)?;
        let mut out = OpenOptions::new().create(true).truncate(true).write(true).open(local)?;
        out.set_len(file.size)?;
        out.seek(SeekFrom::Start(start))?;
        out.write_all(&bytes)?;
        out.sync_all()?;
        downloaded += bytes.len() as u64;
        write_fetched(local, &fetched)?;
    }
    let metadata = local_metadata(local)?;
    let mut out = OpenOptions::new().write(true).open(local)?;
    for rg in wanted(&metadata, boxes) {
        if fetched.row_groups.contains(&rg) {
            continue;
        }
        let (start, len) = byte_range(metadata.row_group(rg));
        let bytes = dl.fetch_range(&file.url, start, len)?;
        out.seek(SeekFrom::Start(start))?;
        out.write_all(&bytes)?;
        downloaded += len;
        fetched.row_groups.insert(rg);
        // Recorded as it goes: an interrupted fetch resumes after the last complete row group.
        out.sync_data()?;
        write_fetched(local, &fetched)?;
    }
    let before = fetched.boxes.len();
    for key in keys {
        if !fetched.boxes.contains(&key) {
            fetched.boxes.push(key);
        }
    }
    if fetched.boxes.len() != before {
        write_fetched(local, &fetched)?;
    }
    Ok(downloaded)
}

/// The metadata of a local sparse copy, from the footer at its end.
pub fn local_metadata(local: &Path) -> Result<ParquetMetaData> {
    let file = File::open(local)?;
    ParquetMetaDataReader::new().parse_and_finish(&file).map_err(|e| GekoError::Data(format!("{}: {e}", local.display())))
}

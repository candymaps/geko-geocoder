//! Download from an OGC API Features service, one unit of download at a time (PROJECT.md §7.6).
//!
//! Some services have no sorting, and their deep `offset` pages are slow, but filter fast by an
//! indexed property. A unit is then fetched by walking the values of that property it can take
//! (the 999 postcodes of a Spanish province), paging only within a value.

use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use geko_core::ports::{DownloadRequest, Downloader};
use geko_core::{GekoError, Result};
use serde::{Deserialize, Serialize};

use crate::Unit;

const COMPLETE_MARKER: &str = "_complete.json";
const EMPTY_LIST: &str = "_empty.txt";

#[derive(Debug, Deserialize)]
pub struct Page {
    #[serde(default)]
    pub features: Vec<Feature>,
    #[serde(rename = "numberMatched", default)]
    pub number_matched: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct Feature {
    pub id: serde_json::Value,
    #[serde(default)]
    pub properties: serde_json::Map<String, serde_json::Value>,
    pub geometry: Option<Geometry>,
}

#[derive(Debug, Deserialize)]
pub struct Geometry {
    pub coordinates: Vec<f64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CompleteMarker {
    pub fetched_at_unix: u64,
    pub addresses: u64,
    pub pages: u64,
}

pub fn unit_dir(raw: &Path, unit: &Unit) -> PathBuf {
    raw.join(&unit.prefix)
}

/// The items of `collection` whose `property` is `value`, from `offset`.
pub struct Items<'a> {
    pub base: &'a str,
    pub collection: &'a str,
    pub property: &'a str,
    pub page_size: u64,
}

impl Items<'_> {
    pub fn url(&self, value: &str, offset: u64) -> String {
        format!(
            "{}/collections/{}/items?f=json&limit={}&offset={offset}&{}={value}",
            self.base.trim_end_matches('/'),
            self.collection,
            self.page_size,
            self.property
        )
    }
}

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// The marker of a complete download of a unit, whatever its age.
pub fn complete_marker(dir: &Path) -> Option<CompleteMarker> {
    serde_json::from_slice(&fs::read(dir.join(COMPLETE_MARKER)).ok()?).ok()
}

/// Whether a download is still the latest version: made on or after `catalog_date`
/// (`YYYY-MM-DD`), the last date the catalog record gives the dataset, and less than
/// `max_unchanged_days` ago, in case the catalog is not updated with the data.
pub fn still_latest(marker: &CompleteMarker, catalog_date: &str, max_unchanged_days: u64) -> bool {
    now_unix().saturating_sub(marker.fetched_at_unix) < max_unchanged_days.saturating_mul(86_400)
        && day_of(marker.fetched_at_unix).as_str() >= catalog_date
}

/// The latest date (`YYYY-MM-DD`) of an ISO 19139 catalog record: its metadata stamp and the
/// creation, revision and publication dates of the dataset.
pub fn latest_date(record: &str) -> Option<String> {
    record
        .match_indices("<gco:Date")
        .filter_map(|(i, _)| {
            let rest = &record[i..];
            let start = rest.find('>')? + 1;
            let date = rest.get(start..start + 10)?;
            (date.len() == 10 && date.as_bytes()[4] == b'-' && date.as_bytes()[7] == b'-').then(|| date.to_owned())
        })
        .max()
}

/// The UTC date of a Unix time, `YYYY-MM-DD` (proleptic Gregorian calendar).
pub fn day_of(unix: u64) -> String {
    let days = (unix / 86_400) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let doe = days - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

pub fn read_page(path: &Path) -> Result<Page> {
    let decoder = zstd::Decoder::new(BufReader::new(File::open(path)?))?;
    serde_json::from_reader(decoder).map_err(|e| GekoError::Data(format!("{}: {e}", path.display())))
}

pub fn page_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "zst"))
        .collect();
    files.sort();
    Ok(files)
}

pub struct UnitFetch<'a> {
    /// The source, for messages.
    pub source: &'a str,
    pub items: Items<'a>,
    /// The values of the property to walk, without the prefix of the unit.
    pub suffixes: Vec<String>,
    pub dl: &'a dyn Downloader,
    pub parallel: usize,
}

impl UnitFetch<'_> {
    /// Downloads every value of the unit into `<dir>.partial`, then swaps it into `dir`.
    /// An interrupted run resumes from the pages already in `<dir>.partial`.
    pub fn run(&self, unit: &Unit, dir: &Path) -> Result<CompleteMarker> {
        let partial = dir.with_extension("partial");
        fs::create_dir_all(&partial)?;
        let empty_path = partial.join(EMPTY_LIST);
        let known_empty: Vec<String> =
            fs::read_to_string(&empty_path).unwrap_or_default().lines().map(str::to_owned).collect();
        let empty_log = Mutex::new(OpenOptions::new().create(true).append(true).open(&empty_path)?);

        let queue = Mutex::new(
            self.suffixes
                .iter()
                .map(|s| format!("{}{s}", unit.prefix))
                .filter(|value| !known_empty.contains(value))
                .rev()
                .collect::<Vec<_>>(),
        );
        let addresses = AtomicU64::new(0);
        let pages = AtomicU64::new(0);
        let errors = Mutex::new(Vec::new());

        std::thread::scope(|threads| {
            for _ in 0..self.parallel.max(1) {
                threads.spawn(|| {
                    loop {
                        let Some(value) = queue.lock().unwrap().pop() else { break };
                        match self.value(&value, &partial) {
                            Ok((0, _)) => {
                                let _ = writeln!(empty_log.lock().unwrap(), "{value}");
                            }
                            Ok((n, p)) => {
                                addresses.fetch_add(n, Ordering::Relaxed);
                                pages.fetch_add(p, Ordering::Relaxed);
                            }
                            Err(e) => errors.lock().unwrap().push(format!("{value}: {e}")),
                        }
                    }
                });
            }
        });

        let errors = errors.into_inner().unwrap();
        if !errors.is_empty() {
            return Err(GekoError::Data(format!(
                "{} {}: {} values of {} failed; run again to resume. First error: {}",
                self.source,
                unit.name,
                errors.len(),
                self.items.property,
                errors[0]
            )));
        }
        let marker = CompleteMarker {
            fetched_at_unix: now_unix(),
            addresses: addresses.into_inner(),
            pages: pages.into_inner(),
        };
        fs::write(partial.join(COMPLETE_MARKER), serde_json::to_vec_pretty(&marker).expect("serializable"))?;
        if dir.exists() {
            fs::remove_dir_all(dir)?;
        }
        fs::rename(&partial, dir)?;
        tracing::info!(source = self.source, unit = unit.name.as_str(), addresses = marker.addresses, pages = marker.pages, "downloaded");
        Ok(marker)
    }

    /// Fetches every page of a value of the property. Returns (addresses, pages).
    fn value(&self, value: &str, dir: &Path) -> Result<(u64, u64)> {
        let mut offset = 0;
        let mut addresses = 0;
        let mut pages = 0;
        loop {
            let zst = dir.join(format!("{value}-{offset:07}.json.zst"));
            let page = if zst.exists() {
                read_page(&zst)?
            } else {
                let json = dir.join(format!("{value}-{offset:07}.json"));
                self.dl.fetch(&DownloadRequest { url: self.items.url(value, offset), checksum: None }, &json)?;
                let page: Page = serde_json::from_reader(BufReader::new(File::open(&json)?))
                    .map_err(|e| GekoError::Data(format!("invalid {} page {value}@{offset}: {e}", self.source)))?;
                if !page.features.is_empty() {
                    compress(&json, &zst)?;
                }
                let _ = fs::remove_file(&json);
                let _ = fs::remove_file(download_record(&json));
                page
            };
            let returned = page.features.len() as u64;
            if returned == 0 {
                return Ok((addresses, pages));
            }
            addresses += returned;
            pages += 1;
            offset += returned;
            if page.number_matched.is_some_and(|m| offset >= m) || returned < self.items.page_size {
                return Ok((addresses, pages));
            }
        }
    }
}

fn download_record(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".raw.json");
    PathBuf::from(name)
}

fn compress(src: &Path, dest: &Path) -> Result<()> {
    let tmp = dest.with_extension("zst.tmp");
    let mut encoder = zstd::Encoder::new(BufWriter::new(File::create(&tmp)?), 6)?;
    std::io::copy(&mut BufReader::new(File::open(src)?), &mut encoder)?;
    encoder.finish()?.flush()?;
    fs::rename(&tmp, dest)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(day_of(0), "1970-01-01");
        assert_eq!(day_of(1_792_070_336), "2026-10-15");
        assert_eq!(day_of(951_782_400), "2000-02-29");
        let record = "<gmd:dateStamp><gco:Date>2026-05-19</gco:Date></gmd:dateStamp>\
            <gmd:date><gco:Date>2006-12-31</gco:Date></gmd:date><gco:DateTime>2026-06-02T10:00:00</gco:DateTime>";
        assert_eq!(latest_date(record).as_deref(), Some("2026-06-02"));
        assert_eq!(latest_date("<a/>"), None);
        let marker = CompleteMarker { fetched_at_unix: now_unix() - 86_400 * 40, addresses: 1, pages: 1 };
        let downloaded = day_of(marker.fetched_at_unix);
        assert!(still_latest(&marker, &downloaded, 365));
        assert!(!still_latest(&marker, "2999-01-01", 365));
        assert!(!still_latest(&marker, &downloaded, 30), "too old whatever the catalog says");
    }

    #[test]
    fn items_url_filters_by_the_walked_property() {
        let items = Items {
            base: "https://api-features.idee.es/",
            collection: "address",
            property: "component_PostalDescriptor",
            page_size: 10_000,
        };
        assert_eq!(
            items.url("08036", 20000),
            "https://api-features.idee.es/collections/address/items?f=json&limit=10000&offset=20000&component_PostalDescriptor=08036"
        );
    }

    #[test]
    fn parses_a_page() {
        let json = r#"{"type":"FeatureCollection","numberMatched":1,"features":[{"type":"Feature","id":"AD_ADDRESS_PPK_09.08.G08_080190073758",
            "properties":{"locator_designator_addressNumber":"9998","component_ThoroughfareName":"AVENIDA DIAGONAL"},
            "geometry":{"type":"Point","coordinates":[2.163352516481388,41.397715561524045]}}]}"#;
        let page: Page = serde_json::from_str(json).unwrap();
        assert_eq!(page.number_matched, Some(1));
        let f = &page.features[0];
        assert_eq!(f.properties["component_ThoroughfareName"], "AVENIDA DIAGONAL");
        assert_eq!(f.geometry.as_ref().unwrap().coordinates, [2.163352516481388, 41.397715561524045]);
    }

    #[test]
    fn a_complete_download_stays_the_latest_until_the_catalog_moves() {
        let dir = tempfile::tempdir().unwrap();
        assert!(complete_marker(dir.path()).is_none());
        let fetched = now_unix() - 10 * 86_400;
        let marker = CompleteMarker { fetched_at_unix: fetched, addresses: 1, pages: 1 };
        fs::write(dir.path().join(COMPLETE_MARKER), serde_json::to_vec(&marker).unwrap()).unwrap();
        let marker = complete_marker(dir.path()).unwrap();
        assert!(still_latest(&marker, &day_of(fetched - 86_400), 365));
        assert!(!still_latest(&marker, &day_of(fetched + 86_400), 365), "the catalog has newer data");
        assert!(!still_latest(&marker, &day_of(fetched - 86_400), 5), "older than max_unchanged_days");
    }
}

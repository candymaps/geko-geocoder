//! Overture Maps places as POIs, for any country (PROJECT.md §7, sources table).
//!
//! The places of a release are a few world files sorted by place. Of each, only the footer and
//! the row groups whose box meets a partition of the scope are downloaded, into a sparse local
//! copy (`remote`). A partition is boxed by its pack (`data/region-boxes.tsv`) or, without one,
//! by its country: for the Balearic Islands, 11 MB instead of the 379 MB of Spain. Staging reads that copy country by country (`stage_by_country`), and
//! relocation gives every POI to the region that contains it.
//!
//! ```text
//! raw/overture/releases.xml
//! raw/overture/<release>/files.xml
//! raw/overture/<release>/<file>.parquet               sparse: footer and the fetched row groups
//! raw/overture/<release>/<file>.parquet.fetched.json  which row groups are there
//! ```

mod remote;
mod stage;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::OnceLock;

use geko_core::country::CountryCode;
use geko_core::regions::Regions;
use geko_core::model::{License, SourceId};
use geko_core::ports::{DatasetRequest, Downloader, FetchReport, RawDir, SourceProvider, SourceStats, StagingDir};
use geko_core::scope::{Coverage, Partition, Scope};
use geko_core::{GekoError, Result};
use serde::Deserialize;

pub const ID: &str = "overture";

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OvertureConfig {
    /// Base URL of the bucket of releases.
    pub base: String,
    /// `latest`, or a release such as `2026-09-23.1`.
    pub release: String,
    /// Places Overture is less sure exist are left out (0 to 1).
    pub min_confidence: f64,
}

impl Default for OvertureConfig {
    fn default() -> Self {
        Self { base: "https://overturemaps-us-west-2.s3.amazonaws.com".into(), release: "latest".into(), min_confidence: 0.8 }
    }
}

/// A box in degrees.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BBox {
    pub xmin: f64,
    pub ymin: f64,
    pub xmax: f64,
    pub ymax: f64,
}

impl BBox {
    pub fn meets(&self, other: &BBox) -> bool {
        self.xmin <= other.xmax && other.xmin <= self.xmax && self.ymin <= other.ymax && other.ymin <= self.ymax
    }

    pub fn contains(&self, lon: f64, lat: f64) -> bool {
        (self.xmin..=self.xmax).contains(&lon) && (self.ymin..=self.ymax).contains(&lat)
    }
}

/// Boxes of the countries, by ISO code (`data/country_bbox.tsv`).
fn country_boxes() -> &'static HashMap<String, BBox> {
    static BOXES: OnceLock<HashMap<String, BBox>> = OnceLock::new();
    BOXES.get_or_init(|| {
        include_str!("../data/country_bbox.tsv")
            .lines()
            .filter(|l| !l.starts_with('#') && !l.starts_with("country"))
            .filter_map(|l| {
                let f: Vec<&str> = l.split('\t').collect();
                let n = |i: usize| f.get(i)?.parse::<f64>().ok();
                Some((f.first()?.to_string(), BBox { xmin: n(1)?, ymin: n(2)?, xmax: n(3)?, ymax: n(4)? }))
            })
            .collect()
    })
}

/// OSM tags of the Overture categories (`data/categories.tsv`).
fn categories() -> &'static HashMap<String, String> {
    static MAP: OnceLock<HashMap<String, String>> = OnceLock::new();
    MAP.get_or_init(|| {
        include_str!("../data/categories.tsv")
            .lines()
            .filter(|l| !l.starts_with('#'))
            .filter_map(|l| l.split_once('\t').map(|(k, v)| (k.to_owned(), v.to_owned())))
            .collect()
    })
}

fn country_box(country: CountryCode) -> Result<BBox> {
    country_boxes().get(country.as_str()).copied().ok_or_else(|| GekoError::Config(format!("no Overture box for country {country}")))
}

pub struct OvertureSource {
    config: OvertureConfig,
    /// The regions of the countries of the scope, from their packs: the boxes of the partitions.
    regions: BTreeMap<CountryCode, Regions>,
}

impl OvertureSource {
    pub fn new(config: OvertureConfig, regions: BTreeMap<CountryCode, Regions>) -> Self {
        Self { config, regions }
    }

    fn base(&self) -> &str {
        self.config.base.trim_end_matches('/')
    }

    fn release(&self, dl: &dyn Downloader, raw: &RawDir) -> Result<String> {
        std::fs::create_dir_all(&raw.path)?;
        if self.config.release == "latest" { remote::latest_release(dl, self.base(), &raw.path) } else { Ok(self.config.release.clone()) }
    }

    /// The release staged last, as recorded by `fetch`.
    fn fetched_release(raw: &RawDir) -> Result<String> {
        std::fs::read_to_string(raw.path.join("release")).map(|s| s.trim().to_owned()).map_err(|_| {
            GekoError::Data("Overture places have not been downloaded; run `geko fetch` first".into())
        })
    }

    /// The box of each partition of the scope: its own, from its pack, or else its country's.
    fn boxes(&self, scope: &Scope) -> Result<Vec<BBox>> {
        let mut boxes = Vec::new();
        for p in scope.partitions() {
            let own = p.region.as_ref().and_then(|r| self.regions.get(&p.country)?.get(r)?.bbox);
            let b = match own {
                Some(b) => BBox { xmin: b.min.lon(), ymin: b.min.lat(), xmax: b.max.lon(), ymax: b.max.lat() },
                None => country_box(p.country)?,
            };
            if !boxes.contains(&b) {
                boxes.push(b);
            }
        }
        Ok(boxes)
    }
}

impl SourceProvider for OvertureSource {
    fn id(&self) -> SourceId {
        SourceId(ID.into())
    }

    fn license(&self) -> License {
        License {
            name: "CDLA-Permissive-2.0".into(),
            attribution: "© Overture Maps Foundation, places CDLA-Permissive-2.0".into(),
            url: Some("https://docs.overturemaps.org/attribution/".into()),
        }
    }

    fn coverage(&self) -> Coverage {
        Coverage::Global
    }

    fn plan(&self, scope: &Scope, dl: &dyn Downloader, raw: &RawDir) -> Result<Vec<DatasetRequest>> {
        let release = self.release(dl, raw)?;
        let dir = raw.path.join(&release);
        std::fs::create_dir_all(&dir)?;
        let files = remote::place_files(dl, self.base(), &release, &dir)?;
        let boxes = self.boxes(scope)?;
        Ok(files
            .into_iter()
            .map(|f| DatasetRequest {
                source: self.id(),
                name: format!("places {release} {} (the row groups that meet {} box{})", f.name, boxes.len(), if boxes.len() == 1 { "" } else { "es" }),
                url: f.url,
                estimated_size: None,
            })
            .collect())
    }

    fn fetch(&self, scope: &Scope, dl: &dyn Downloader, raw: &RawDir) -> Result<FetchReport> {
        let release = self.release(dl, raw)?;
        let dir = raw.path.join(&release);
        std::fs::create_dir_all(&dir)?;
        let boxes = self.boxes(scope)?;
        let mut report = FetchReport::default();
        let mut identity = format!("release {release}");
        for file in remote::place_files(dl, self.base(), &release, &dir)? {
            let local = dir.join(&file.name);
            let bytes = remote::fetch_file(dl, &file, &local, &boxes)?;
            let groups: Vec<String> = remote::read_fetched(&local).row_groups.iter().map(usize::to_string).collect();
            identity.push_str(&format!("; {} {} [{}]", file.name, file.size, groups.join(",")));
            let label = format!("places {release} {}", file.name);
            if bytes > 0 { report.downloaded.push(label) } else { report.not_modified.push(label) }
        }
        report.identities.push((raw.path.join("release"), format!("release {release}")));
        report.identities.push((dir.clone(), identity));
        // Written only when it changes: the identity of the downloads, and every stage that
        // reads them, would change with the time of the file.
        let pointer = raw.path.join("release");
        if std::fs::read_to_string(&pointer).ok().as_deref() != Some(release.as_str()) {
            std::fs::write(&pointer, &release)?;
        }
        // Releases are monthly: the parts of older ones are never read again.
        for entry in std::fs::read_dir(&raw.path)?.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.path().is_dir() && name != release && name.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                std::fs::remove_dir_all(entry.path())?;
                tracing::info!(release = %name, "older Overture release removed");
            }
        }
        Ok(report)
    }

    fn stage(&self, partition: &Partition, raw: &RawDir, out: &StagingDir) -> Result<SourceStats> {
        let release = Self::fetched_release(raw)?;
        let dir = raw.path.join(&release);
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "parquet"))
            .collect();
        files.sort();
        for entry in std::fs::read_dir(&dir)? {
            let name = entry?.file_name().to_string_lossy().into_owned();
            if let Some(parquet) = name.strip_suffix(".fetched.json")
                && !dir.join(parquet).exists()
            {
                return Err(GekoError::Data(format!(
                    "Overture {release} {parquet} was removed after publishing ([data] keep_downloads = false); a build with a connection downloads it again"
                )));
            }
        }
        let filter = stage::Filter {
            country: partition.country.as_str(),
            area: country_box(partition.country)?,
            min_confidence: self.config.min_confidence,
            categories: categories(),
        };
        let records = stage::stage(&files, &filter, &out.path)?;
        Ok(SourceStats { records })
    }

    fn stage_by_country(&self) -> bool {
        true
    }

    fn stage_inputs(&self, _: &Partition, raw: &RawDir) -> Result<Vec<PathBuf>> {
        let release = Self::fetched_release(raw)?;
        Ok(vec![raw.path.join("release"), raw.path.join(release)])
    }

    /// The sparse copies of the places files; their row groups stay recorded beside them.
    fn removable(&self, raw: &RawDir) -> Result<Vec<PathBuf>> {
        let Ok(release) = Self::fetched_release(raw) else { return Ok(Vec::new()) };
        let mut files = Vec::new();
        for entry in std::fs::read_dir(raw.path.join(release))? {
            let path = entry?.path();
            if path.extension().is_some_and(|x| x == "parquet") {
                files.push(path);
            }
        }
        Ok(files)
    }
}

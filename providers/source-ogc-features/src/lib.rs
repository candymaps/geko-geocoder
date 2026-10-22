//! Addresses from an OGC API Features service, as a country pack declares them in
//! `sources/<source>.toml` with `kind = "ogc-api-features"` (PROJECT.md §2.4, §7.6). The service,
//! the property a unit is walked by and the columns of the staging are data of the pack: this
//! provider knows no country.
//!
//! The units of download are the partitions of the pack (its regions that contain no other), each
//! with the prefix of the values it walks, an attribute of the region: the provinces of Spain
//! with their `postcode_prefix`.

mod api;
mod stage;

use std::path::PathBuf;

use geko_core::country::CountryCode;
use geko_core::model::{License, SourceId};
use geko_core::ports::{DatasetRequest, Downloader, FetchReport, RawDir, SourceProvider, SourceStats, StagingDir};
use geko_core::regions::Regions;
use geko_core::scope::{Coverage, Partition, Scope};
use geko_core::{GekoError, Result};
use serde::Deserialize;

pub const KIND: &str = "ogc-api-features";

/// The columns every address source writes (`schemas/staging/addresses.md`), besides `source_id`
/// and the coordinates.
const ADDRESS_COLUMNS: [&str; 5] = ["street", "number", "number_extension", "postcode", "place"];

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OgcFeaturesConfig {
    /// `ogc-api-features`.
    pub kind: String,
    /// How people know it ("CartoCiudad"), for the progress of a build; its id when missing.
    #[serde(default)]
    pub name: Option<String>,
    pub license: LicenseConfig,
    /// Base URL of the service.
    pub base: String,
    pub collection: String,
    #[serde(default = "page_size")]
    pub page_size: u64,
    /// Concurrent requests to the service. Kept low out of courtesy to a public service.
    #[serde(default = "parallel_requests")]
    pub parallel_requests: usize,
    /// The catalog record of the dataset (ISO 19139). When the data are due for a check
    /// (`[data] max_age_days`), a complete download is kept, without contacting the service, if
    /// it was made after the latest date of the record.
    #[serde(default)]
    pub catalog_url: Option<String>,
    /// A download older than this is made again whatever the catalog says.
    #[serde(default = "max_unchanged_days")]
    pub max_unchanged_days: u64,
    pub walk: Walk,
    pub staging: Staging,
}

fn page_size() -> u64 {
    10_000
}
fn parallel_requests() -> usize {
    4
}
/// What identifies the complete download of a unit, with or without its pages.
fn identity(marker: &api::CompleteMarker) -> String {
    format!("complete {} {} {}", marker.fetched_at_unix, marker.addresses, marker.pages)
}

fn max_unchanged_days() -> u64 {
    365
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LicenseConfig {
    pub name: String,
    pub attribution: String,
    #[serde(default)]
    pub url: Option<String>,
}

/// How a unit is fetched: every value `<prefix><suffix>` of `property`, where the prefix is the
/// attribute `prefix` of the unit and the suffixes are the numbers `from..=to` with `digits`
/// digits.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Walk {
    pub property: String,
    pub prefix: String,
    pub from: u32,
    pub to: u32,
    pub digits: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Staging {
    /// The `geko.schema` of the table.
    pub schema: String,
    /// Columns after `source_id` (the id of the feature), in order.
    pub columns: Vec<Column>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Column {
    pub name: String,
    /// The property of the feature it comes from.
    pub property: String,
    /// Values that mean none (the number of an address without one).
    #[serde(default)]
    pub null_values: Vec<String>,
}

/// A unit of download: a partition of the pack and the prefix of the values it walks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    pub prefix: String,
    pub name: String,
}

pub struct OgcFeaturesSource {
    id: String,
    country: CountryCode,
    config: OgcFeaturesConfig,
    regions: Regions,
}

impl OgcFeaturesSource {
    /// The source `id` of the pack of `country`, whose units are the partitions of `regions`.
    pub fn new(id: &str, country: CountryCode, config: OgcFeaturesConfig, regions: Regions) -> Result<Self> {
        let invalid = |msg: String| Err(GekoError::Config(format!("source {id} of the pack of {country}: {msg}")));
        if config.kind != KIND {
            return invalid(format!("kind is `{}`, not `{KIND}`", config.kind));
        }
        if let Some(missing) = ADDRESS_COLUMNS.iter().find(|c| !config.staging.columns.iter().any(|col| col.name == **c)) {
            return invalid(format!("the staging has no `{missing}` column (schemas/staging/addresses.md)"));
        }
        if config.walk.from > config.walk.to || config.walk.digits == 0 {
            return invalid("walk: `from` must not be above `to`, and `digits` must be at least 1".into());
        }
        Ok(Self { id: id.to_owned(), country, config, regions })
    }

    /// The units of the partition: the partitions of the pack inside it.
    fn units(&self, partition: &Partition) -> Result<Vec<Unit>> {
        let partitions = match &partition.region {
            Some(region) => self.regions.partitions_in(region)?,
            None => self.regions.partitions().collect(),
        };
        let key = &self.config.walk.prefix;
        partitions
            .into_iter()
            .map(|p| {
                let prefix = self.regions.attribute(&p.code, key).ok_or_else(|| {
                    GekoError::Config(format!("source {}: the pack of {} gives {} no {key}", self.id, self.country, p.code))
                })?;
                Ok(Unit { prefix: prefix.to_owned(), name: p.name.clone() })
            })
            .collect()
    }

    fn scope_units(&self, scope: &Scope) -> Result<Vec<Unit>> {
        let mut out: Vec<Unit> = Vec::new();
        for p in scope.partitions().into_iter().filter(|p| p.country == self.country) {
            for unit in self.units(&p)? {
                if !out.iter().any(|o| o.prefix == unit.prefix) {
                    out.push(unit);
                }
            }
        }
        Ok(out)
    }

    fn suffixes(&self) -> Vec<String> {
        let w = &self.config.walk;
        (w.from..=w.to).map(|n| format!("{n:0width$}", width = w.digits)).collect()
    }

    fn items(&self) -> api::Items<'_> {
        api::Items {
            base: &self.config.base,
            collection: &self.config.collection,
            property: &self.config.walk.property,
            page_size: self.config.page_size,
        }
    }

    /// The latest date of the dataset in its catalog record; None without one, or if it cannot be
    /// read, and then a stale download is made again.
    fn catalog_date(&self, dl: &dyn Downloader, raw: &RawDir) -> Option<String> {
        let url = self.config.catalog_url.clone()?;
        let path = raw.path.join("_catalog.xml");
        if let Err(e) = dl.fetch(&geko_core::ports::DownloadRequest { url, checksum: None }, &path) {
            tracing::warn!(source = self.id, "cannot read the catalog record: {e}");
            return None;
        }
        api::latest_date(&std::fs::read_to_string(&path).ok()?)
    }
}

impl SourceProvider for OgcFeaturesSource {
    fn id(&self) -> SourceId {
        SourceId(self.id.clone())
    }

    fn name(&self) -> String {
        self.config.name.clone().unwrap_or_else(|| self.id.clone())
    }

    fn license(&self) -> License {
        let l = &self.config.license;
        License { name: l.name.clone(), attribution: l.attribution.clone(), url: l.url.clone() }
    }

    fn coverage(&self) -> Coverage {
        Coverage::Countries(vec![self.country])
    }

    fn plan(&self, scope: &Scope, _: &dyn Downloader, raw: &RawDir) -> Result<Vec<DatasetRequest>> {
        let suffixes = self.suffixes();
        let (first, last) = (suffixes.first().cloned().unwrap_or_default(), suffixes.last().cloned().unwrap_or_default());
        Ok(self
            .scope_units(scope)?
            .into_iter()
            .map(|u| {
                let cached = api::complete_marker(&api::unit_dir(&raw.path, &u))
                    .map(|m| format!(", cached: {} addresses", m.addresses))
                    .unwrap_or_default();
                DatasetRequest {
                    source: self.id(),
                    name: format!(
                        "{} {} ({} {p}{first}-{p}{last}{cached})",
                        self.config.collection,
                        u.name,
                        self.config.walk.property,
                        p = u.prefix
                    ),
                    url: format!("{}/collections/{}/items", self.config.base.trim_end_matches('/'), self.config.collection),
                    estimated_size: None,
                }
            })
            .collect())
    }

    fn fetch(&self, scope: &Scope, dl: &dyn Downloader, raw: &RawDir) -> Result<FetchReport> {
        let mut report = FetchReport::default();
        let fetch = api::UnitFetch {
            source: &self.id,
            items: self.items(),
            suffixes: self.suffixes(),
            dl,
            parallel: self.config.parallel_requests,
        };
        let mut catalog_date: Option<Option<String>> = None;
        for unit in self.scope_units(scope)? {
            // Counted off however it ends: up to date, downloaded or failed.
            let _counted = Counted;
            let dir = api::unit_dir(&raw.path, &unit);
            let label = format!("{}-{}", self.config.collection, unit.prefix);
            if let Some(marker) = api::complete_marker(&dir) {
                // Its pages were removed after publishing, and a staging needs them again.
                let removed = marker.pages > 0 && api::page_files(&dir)?.is_empty();
                let restore = removed && !dl.keeps_removed();
                // Not due for a check: the complete download is kept without asking the service.
                if !restore && !dl.checks_for_updates() {
                    report.not_modified.push(label);
                    report.identities.push((dir, identity(&marker)));
                    continue;
                }
                let date = (!restore).then(|| catalog_date.get_or_insert_with(|| self.catalog_date(dl, raw)).clone());
                if date.flatten().is_some_and(|d| api::still_latest(&marker, &d, self.config.max_unchanged_days)) {
                    report.not_modified.push(label);
                    report.identities.push((dir, identity(&marker)));
                    continue;
                }
            }
            let marker = fetch.run(&unit, &dir)?;
            report.downloaded.push(label);
            report.identities.push((dir, identity(&marker)));
        }
        Ok(report)
    }

    fn stage(&self, partition: &Partition, raw: &RawDir, out: &StagingDir) -> Result<SourceStats> {
        let mut files: Vec<PathBuf> = Vec::new();
        for unit in self.units(partition)? {
            let dir = api::unit_dir(&raw.path, &unit);
            let Some(marker) = api::complete_marker(&dir) else {
                return Err(GekoError::Data(format!(
                    "{} {} has not been downloaded completely; run `geko fetch` first",
                    self.id, unit.name
                )));
            };
            let pages = api::page_files(&dir)?;
            if pages.is_empty() && marker.pages > 0 {
                return Err(GekoError::Data(format!(
                    "the pages of {} {} were removed after publishing ([data] keep_downloads = false); a build with a connection downloads them again",
                    self.id, unit.name
                )));
            }
            files.extend(pages);
        }
        let staging = &self.config.staging;
        let records = stage::write_addresses(&out.path, &staging.schema, &staging.columns, files.iter().map(|f| api::read_page(f)))?;
        Ok(SourceStats { records })
    }

    fn stage_inputs(&self, partition: &Partition, raw: &RawDir) -> Result<Vec<PathBuf>> {
        Ok(self.units(partition)?.iter().map(|u| api::unit_dir(&raw.path, u)).collect())
    }

    /// The pages of every complete download; its marker stays.
    fn removable(&self, raw: &RawDir) -> Result<Vec<PathBuf>> {
        let mut pages = Vec::new();
        for unit in self.units(&Partition { country: self.country, region: None })? {
            let dir = api::unit_dir(&raw.path, &unit);
            if api::complete_marker(&dir).is_some() {
                pages.extend(api::page_files(&dir)?);
            }
        }
        Ok(pages)
    }

    fn unused_raw(&self, scope: &Scope, raw: &RawDir) -> Result<Vec<PathBuf>> {
        let used = self.scope_units(scope)?;
        let all = self.units(&Partition { country: self.country, region: None })?;
        Ok(all
            .iter()
            .filter(|u| !used.contains(u))
            .map(|u| api::unit_dir(&raw.path, u))
            .filter(|dir| dir.exists())
            .collect())
    }
}

/// Counts off one of the units `plan` listed in the progress of the build when it goes.
struct Counted;

impl Drop for Counted {
    fn drop(&mut self) {
        geko_core::progress::advance(1);
    }
}

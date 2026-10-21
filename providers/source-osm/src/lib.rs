//! OpenStreetMap source provider (PROJECT.md §6.1, §7.4, §8.1).

mod geofabrik;
mod read;
mod write;

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use geko_core::model::{License, SourceId};
use geko_core::module::ModuleName;
use geko_core::ports::{
    Checksum, DatasetRequest, DownloadRequest, Downloader, FetchReport, RawDir, SourceProvider, SourceStats, StagingDir,
};
use geko_core::scope::{Coverage, Partition, Scope};
use geko_core::{GekoError, Result};
use rustc_hash::FxHashSet;
use serde::Deserialize;

use crate::geofabrik::{Catalog, Extract};
pub use crate::geofabrik::{Countries, Country, CountryExtracts};
use geko_geom::PreparedArea;
use crate::read::{Admin, Extracted, OsmType};

pub const ID: &str = "osm";

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OsmConfig {
    /// `auto` for Geofabrik, or the base URL of a mirror with the same layout.
    pub mirror: String,
}

impl Default for OsmConfig {
    fn default() -> Self {
        Self { mirror: "auto".into() }
    }
}

pub struct OsmSource {
    base: String,
    countries: Countries,
}

impl OsmSource {
    /// `countries`: what the packs of the countries of the scope tell this source.
    pub fn new(config: OsmConfig, countries: Countries) -> Self {
        let base = if config.mirror == "auto" { geofabrik::DEFAULT_BASE.to_string() } else { config.mirror };
        Self { base, countries }
    }

    fn index_path(raw: &RawDir) -> PathBuf {
        raw.path.join(geofabrik::INDEX_FILE)
    }

    fn catalog(&self, dl: &dyn Downloader, raw: &RawDir) -> Result<Catalog> {
        let path = Self::index_path(raw);
        fs::create_dir_all(&raw.path)?;
        let url = format!("{}/{}", self.base.trim_end_matches('/'), geofabrik::INDEX_FILE);
        dl.fetch(&DownloadRequest { url, checksum: None }, &path)?;
        Catalog::parse(&fs::read(&path)?)
    }

    fn extracts<'c>(&self, catalog: &'c Catalog, scope: &Scope) -> Result<Vec<&'c Extract>> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for p in scope.partitions() {
            for e in catalog.for_partition(&p, &self.countries)? {
                if seen.insert(e.id.clone()) {
                    out.push(e);
                }
            }
        }
        Ok(out)
    }

    fn pbf_url(&self, extract: &Extract) -> String {
        geofabrik::with_base(&extract.urls.pbf, &self.base)
    }
}

impl SourceProvider for OsmSource {
    fn id(&self) -> SourceId {
        SourceId(ID.into())
    }

    fn license(&self) -> License {
        License {
            name: "ODbL 1.0".into(),
            attribution: "© OpenStreetMap contributors".into(),
            url: Some("https://www.openstreetmap.org/copyright".into()),
        }
    }

    fn coverage(&self) -> Coverage {
        Coverage::Global
    }

    fn used_by(&self) -> &'static [ModuleName] {
        &[ModuleName::Geocoder, ModuleName::Tiles]
    }

    fn plan(&self, scope: &Scope, dl: &dyn Downloader, raw: &RawDir) -> Result<Vec<DatasetRequest>> {
        let catalog = self.catalog(dl, raw)?;
        Ok(self
            .extracts(&catalog, scope)?
            .into_iter()
            .map(|e| DatasetRequest {
                source: self.id(),
                name: geofabrik::file_name(e),
                url: self.pbf_url(e),
                estimated_size: None,
            })
            .collect())
    }

    fn fetch(&self, scope: &Scope, dl: &dyn Downloader, raw: &RawDir) -> Result<FetchReport> {
        let catalog = self.catalog(dl, raw)?;
        let mut report = FetchReport::default();
        for e in self.extracts(&catalog, scope)? {
            let file = geofabrik::file_name(e);
            let url = self.pbf_url(e);
            let md5_path = raw.path.join(format!("{file}.md5"));
            dl.fetch(&DownloadRequest { url: format!("{url}.md5"), checksum: None }, &md5_path)?;
            let md5 = fs::read_to_string(&md5_path)?
                .split_whitespace()
                .next()
                .map(str::to_owned)
                .ok_or_else(|| GekoError::Data(format!("empty checksum file {}", md5_path.display())))?;
            let path = raw.path.join(&file);
            let outcome = dl
                .fetch(&DownloadRequest { url, checksum: Some(Checksum::Md5(md5)) }, &path)
                .map_err(|e| GekoError::Data(format!("{e} (if Geofabrik published a new extract meanwhile, run again)")))?;
            report.identities.push((path, format!("sha256 {}", outcome.sha256)));
            if outcome.downloaded { report.downloaded.push(file) } else { report.not_modified.push(file) }
        }
        Ok(report)
    }

    fn stage(&self, partition: &Partition, raw: &RawDir, out: &StagingDir) -> Result<SourceStats> {
        let index = Self::index_path(raw);
        let catalog = Catalog::parse(&fs::read(&index).map_err(|e| {
            GekoError::Data(format!("{}: {e}; run `geko fetch` first", index.display()))
        })?)?;
        let mut merged = Extracted::default();
        for e in catalog.for_partition(partition, &self.countries)? {
            let path = raw.path.join(geofabrik::file_name(e));
            if !path.exists() {
                return Err(GekoError::Data(format!("{} is missing; run `geko fetch` first", path.display())));
            }
            tracing::info!(file = %path.display(), "reading OSM extract");
            merge(&mut merged, read::read(&path)?);
        }
        let clipped = clip(merged, partition)?;
        let records = write::write_all(&out.path, &clipped)?;
        Ok(SourceStats { records })
    }

    /// The extracts of the partition only: the index changes every day even when they do not.
    fn stage_inputs(&self, partition: &Partition, raw: &RawDir) -> Result<Vec<PathBuf>> {
        let index = Self::index_path(raw);
        let Ok(bytes) = fs::read(&index) else { return Ok(vec![index]) };
        Ok(Catalog::parse(&bytes)?.for_partition(partition, &self.countries)?.into_iter().map(|e| raw.path.join(geofabrik::file_name(e))).collect())
    }

    fn removable(&self, raw: &RawDir) -> Result<Vec<PathBuf>> {
        let mut extracts = Vec::new();
        for entry in fs::read_dir(&raw.path)? {
            let path = entry?.path();
            if path.to_string_lossy().ends_with(".osm.pbf") {
                extracts.push(path);
            }
        }
        Ok(extracts)
    }

    fn unused_raw(&self, scope: &Scope, raw: &RawDir) -> Result<Vec<PathBuf>> {
        let Ok(bytes) = fs::read(Self::index_path(raw)) else { return Ok(Vec::new()) };
        let catalog = Catalog::parse(&bytes)?;
        let used: BTreeSet<String> = self.extracts(&catalog, scope)?.into_iter().map(geofabrik::file_name).collect();
        let mut unused = Vec::new();
        for entry in fs::read_dir(&raw.path)? {
            let name = entry?.file_name().to_string_lossy().into_owned();
            if name.ends_with(".osm.pbf") && !used.contains(&name) {
                unused.push(raw.path.join(name));
            }
        }
        Ok(unused)
    }
}

/// Features repeated across overlapping extracts are kept once; for boundaries, the copy
/// with the most complete geometry wins.
fn merge(into: &mut Extracted, from: Extracted) {
    fn dedup<T>(into: &mut Vec<T>, from: Vec<T>, key: impl Fn(&T) -> (OsmType, i64)) {
        let seen: FxHashSet<_> = into.iter().map(&key).collect();
        into.extend(from.into_iter().filter(|x| !seen.contains(&key(x))));
    }
    dedup(&mut into.streets, from.streets, |x| (x.common.osm_type, x.common.osm_id));
    dedup(&mut into.addresses, from.addresses, |x| (x.common.osm_type, x.common.osm_id));
    dedup(&mut into.pois, from.pois, |x| (x.common.osm_type, x.common.osm_id));
    dedup(&mut into.places, from.places, |x| (x.common.osm_type, x.common.osm_id));
    for admin in from.admins {
        let size = |a: &Admin| a.rings.iter().map(Vec::len).sum::<usize>();
        match into.admins.iter_mut().find(|a| a.common.osm_id == admin.common.osm_id) {
            Some(existing) if size(existing) < size(&admin) => *existing = admin,
            Some(_) => {}
            None => into.admins.push(admin),
        }
    }
}

/// Keeps the features of the partition: those whose point lies in the region (or country)
/// boundary, plus the boundaries that contain the region, needed for the admin hierarchy.
fn clip(data: Extracted, partition: &Partition) -> Result<Extracted> {
    let area_admin = match &partition.region {
        Some(region) => Some(
            data.admins
                .iter()
                .find(|a| a.iso3166_2.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(region.as_str())))
                .ok_or_else(|| GekoError::Data(format!("no OSM boundary tagged ISO3166-2={region} in the extract")))?,
        ),
        None => {
            let found = data.admins.iter().find(|a| {
                a.admin_level == 2
                    && a.iso3166_1.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(partition.country.as_str()))
            });
            if found.is_none() {
                tracing::warn!(country = %partition.country, "no country boundary found; keeping the whole extract");
            }
            found
        }
    };
    let Some(area_admin) = area_admin else { return Ok(data) };
    let area = PreparedArea::new(&area_admin.rings);
    let (area_id, area_level) = (area_admin.common.osm_id, area_admin.admin_level);
    let area_bbox = area.bbox();

    let inside = |p| area.contains(p);
    let Extracted { streets, addresses, pois, places, admins } = data;
    Ok(Extracted {
        streets: streets.into_iter().filter(|x| inside(x.common.point)).collect(),
        addresses: addresses.into_iter().filter(|x| inside(x.common.point)).collect(),
        pois: pois.into_iter().filter(|x| inside(x.common.point)).collect(),
        places: places.into_iter().filter(|x| inside(x.common.point)).collect(),
        admins: admins
            .into_iter()
            .filter(|a| {
                a.common.osm_id == area_id
                    || if a.admin_level < area_level {
                        geko_geom::BBox::of(a.rings.iter().flatten().copied()).intersects(&area_bbox)
                    } else {
                        a.admin_level > area_level && inside(a.common.point)
                    }
            })
            .collect(),
    })
}

//! What the geocoder does for `geko plan`, `fetch`, `build` and `purge`: download its sources,
//! stage, fuse and index every country of the scope, and hand the host the parts of the bundle.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use geko_build::fetch::{Identities, fetch_all, plan_datasets, stage_all};
use geko_build::incremental::{self, Key};
use geko_build::selection::{SelectedSource, SkipReason, SkippedSource, select_sources};
use geko_core::bundle::LicenseNotice;
use geko_core::ports::Downloader;
use geko_core::scope::Scope;
use geko_core::segment::{Meta, Segment};
use geko_core::{GekoError, Result};
use geko_fetch::{HttpDownloader, HttpDownloaderConfig, RefreshPolicy};
use geko_module_sdk::module::{BundlePart, Built, DataPolicy, Refresh};
use geko_module_sdk::stages::StageLog;

use crate::config::Config;
use crate::sources::{self, Factory};

/// What every step reads: the configuration of the geocoder, and what the host tells it.
pub struct Setup {
    pub config: Config,
    /// The scope as configured, before it is split into partitions.
    pub scope: Scope,
    pub data_dir: PathBuf,
    pub data: DataPolicy,
}

pub struct Prepared {
    pub scope: Scope,
    pub selected: Vec<SelectedSource>,
    pub skipped: Vec<SkippedSource>,
    /// Code version of each enabled source, by name.
    pub codes: BTreeMap<String, String>,
}

pub fn prepare(setup: &Setup, registry: &BTreeMap<&'static str, Factory>) -> Result<Prepared> {
    let scope = setup.scope.clone();
    if scope.is_empty() {
        return Err(GekoError::Config("the scope is empty: add at least one [[scope]] entry".into()));
    }
    let packs = sources::packs(&scope)?;
    if registry.is_empty() {
        return Err(GekoError::Config("this geocoder was built without any source (its features source-*, on by default): it could build nothing".into()));
    }
    let scope = sources::partitioned(&scope, &packs)?;
    let sources::Instantiated { enabled, mut skipped, codes } = sources::instantiate(&setup.config, registry, &packs)?;
    let selection = select_sources(&scope, enabled);
    skipped.extend(selection.skipped);
    Ok(Prepared { scope, selected: selection.selected, skipped, codes })
}

/// The downloader of a run. With `refresh = "if-changed"`, it asks the sources for changes only
/// when the last check is older than `[data] max_age_days`, or when `refresh_now`
/// (`geko build --refresh`); otherwise it keeps every download and fetches only what is missing.
pub fn downloader(setup: &Setup, refresh_now: bool) -> HttpDownloader {
    HttpDownloader::new(HttpDownloaderConfig {
        policy: match setup.data.refresh {
            Refresh::Never => RefreshPolicy::Never,
            Refresh::IfChanged if refresh_now || check_due(setup) => RefreshPolicy::IfChanged,
            Refresh::IfChanged => RefreshPolicy::Never,
            Refresh::Always => RefreshPolicy::Always,
        },
        offline: setup.data.offline,
        ..Default::default()
    })
}

/// When every source was last checked for changes, all at once (seconds since the epoch).
fn checked_path(setup: &Setup) -> std::path::PathBuf {
    setup.data_dir.join("raw").join(".checked")
}

fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Seconds since the sources were last checked; `None` if they never were.
fn since_check(setup: &Setup) -> Option<u64> {
    let checked = std::fs::read_to_string(checked_path(setup)).ok()?.trim().parse::<u64>().ok()?;
    Some(now_unix().saturating_sub(checked))
}

/// Whether the sources are due for a check (`[data] max_age_days`).
pub fn check_due(setup: &Setup) -> bool {
    since_check(setup).is_none_or(|s| s >= setup.data.max_age_days.saturating_mul(86_400))
}

/// How long until the sources are due for a check (`[data] max_age_days`); zero if they are.
pub fn until_check(setup: &Setup) -> std::time::Duration {
    let max = setup.data.max_age_days.saturating_mul(86_400);
    std::time::Duration::from_secs(since_check(setup).map_or(0, |s| max.saturating_sub(s)))
}

/// The shortest time between two builds of `geko up`: after a failed one the sources that failed
/// are due at once, and `max_age_days = 0` makes them always due.
const MIN_WAIT: std::time::Duration = std::time::Duration::from_secs(3600);

/// How long until `geko up` builds again: when the sources are due for a check, and never sooner
/// than `MIN_WAIT`. Offline, or with `refresh = "never"`, the sources never change: never.
pub fn next_build(setup: &Setup) -> Option<std::time::Duration> {
    if setup.data.offline || setup.data.refresh == Refresh::Never {
        return None;
    }
    Some(until_check(setup).max(MIN_WAIT))
}

/// Notes that every source was checked, after a fetch in which all of them were.
fn mark_checked(setup: &Setup) -> Result<()> {
    let path = checked_path(setup);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, now_unix().to_string())?;
    Ok(())
}

/// The downloader that brings back what a staging to repeat reads, if it was removed after
/// publishing: it contacts no source for anything else.
fn restorer(setup: &Setup) -> HttpDownloader {
    HttpDownloader::new(HttpDownloaderConfig {
        policy: RefreshPolicy::Never,
        offline: setup.data.offline,
        restore_removed: true,
        ..Default::default()
    })
}

/// Left in the downloads of a source whose files were removed after publishing.
const REMOVED_MARKER: &str = ".removed";

/// After a bundle is published, removes the downloads of every source (`[data] keep_downloads`),
/// leaving their records. Never while offline: nothing could bring them back.
fn remove_downloads(setup: &Setup, prepared: &Prepared) -> Result<String> {
    if setup.data.keep_downloads || setup.data.offline {
        return Ok(String::new());
    }
    let mut out = String::new();
    for s in &prepared.selected {
        let raw = geko_build::fetch::raw_dir(&setup.data_dir, &s.provider.id());
        let (mut files, mut bytes) = (0, 0);
        for path in s.provider.removable(&raw)? {
            let Ok(meta) = std::fs::metadata(&path) else { continue };
            std::fs::remove_file(&path)?;
            files += 1;
            bytes += disk_size(&meta);
        }
        if files > 0 {
            std::fs::write(raw.path.join(REMOVED_MARKER), "")?;
            writeln!(out, "removed the downloads of {:<12} {files} files, {}", s.provider.id().0, human_size(bytes)).unwrap();
        }
    }
    Ok(out)
}

/// The space a file takes on disk: a sparse copy (Overture's) takes far less than its length.
fn disk_size(meta: &std::fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.blocks() * 512
    }
    #[cfg(not(unix))]
    {
        meta.len()
    }
}

/// The partitions of each source whose downloads were removed and whose staging was made by
/// other code (or never): a build has to download again what they read.
fn downloads_again(setup: &Setup, prepared: &Prepared) -> Vec<(String, Vec<String>)> {
    let mut again = Vec::new();
    for s in &prepared.selected {
        let id = s.provider.id();
        if !geko_build::fetch::raw_dir(&setup.data_dir, &id).path.join(REMOVED_MARKER).exists() {
            continue;
        }
        let code = prepared.codes.get(&id.0).cloned().unwrap_or_default();
        let stale: Vec<String> = geko_build::fetch::stage_partitions(s, &prepared.scope)
            .into_iter()
            .filter(|p| {
                let dir = geko_build::fetch::staging_dir(&setup.data_dir, &id, p).path;
                incremental::stamp(&dir).is_none_or(|stamp| stamp.info["code"].as_str() != Some(code.as_str()))
            })
            .map(|p| p.region.map_or_else(|| p.country.to_string(), |r| r.to_string()))
            .collect();
        if !stale.is_empty() {
            again.push((id.0, stale));
        }
    }
    again
}

/// What `geko plan` says about the check of the sources in this run.
fn check_line(setup: &Setup, dl: &dyn Downloader) -> String {
    let days = |s: u64| s / 86_400;
    if setup.data.offline {
        return "offline: nothing is downloaded or checked; downloads are never removed".into();
    }
    match setup.data.refresh {
        Refresh::Never => "refresh = never: only what is missing is downloaded".into(),
        Refresh::Always => "refresh = always: every source is downloaded again".into(),
        Refresh::IfChanged if dl.checks_for_updates() => match since_check(setup) {
            Some(s) => format!("sources checked {} days ago (max_age_days = {}): checking them now", days(s), setup.data.max_age_days),
            None => "sources never checked: checking them now".into(),
        },
        Refresh::IfChanged => {
            let s = since_check(setup).unwrap_or_default();
            format!(
                "sources checked {} days ago: next check in {} days (max_age_days = {}; geko build --refresh checks now)",
                days(s),
                setup.data.max_age_days.saturating_sub(days(s)),
                setup.data.max_age_days
            )
        }
    }
}

pub fn plan(setup: &Setup, prepared: &Prepared, dl: &dyn Downloader) -> String {
    let mut out = String::new();
    writeln!(out, "Scope:").unwrap();
    for p in prepared.scope.partitions() {
        match &p.region {
            Some(r) => writeln!(out, "  {} / {r}", p.country).unwrap(),
            None => writeln!(out, "  {} (whole country)", p.country).unwrap(),
        }
    }
    writeln!(
        out,
        "Data: {} (refresh: {}, max_age_days: {}, keep_downloads: {}, offline: {})",
        setup.data_dir.display(),
        setup.data.refresh.as_str(),
        setup.data.max_age_days,
        setup.data.keep_downloads,
        setup.data.offline
    )
    .unwrap();
    writeln!(out, "Check: {}", check_line(setup, dl)).unwrap();
    writeln!(out, "Country packs:").unwrap();
    for country in prepared.scope.countries() {
        match geko_country_pack::get(country) {
            Ok(pack) => writeln!(out, "  {country}  version {}  {}", pack.text.version, pack.dir.display()).unwrap(),
            Err(e) => writeln!(out, "  {country}  {e}").unwrap(),
        }
    }

    writeln!(out, "Selected sources:").unwrap();
    if prepared.selected.is_empty() {
        writeln!(out, "  none").unwrap();
    }
    let datasets = plan_datasets(&prepared.selected, &prepared.scope, dl, &setup.data_dir);
    for (s, (_, planned)) in prepared.selected.iter().zip(&datasets) {
        let countries: Vec<_> = s.countries.iter().map(|c| c.to_string()).collect();
        let license = s.provider.license();
        writeln!(out, "  {:<16} countries: {:<12} license: {}", s.provider.id().0, countries.join(","), license.name)
            .unwrap();
        match planned {
            Ok(list) => {
                for d in list {
                    let size = d.estimated_size.map(human_size).unwrap_or_else(|| "size unknown".into());
                    writeln!(out, "    {:<40} {:>10}  {}", d.name, size, d.url).unwrap();
                }
            }
            Err(e) => writeln!(out, "    cannot plan: {e}").unwrap(),
        }
    }
    if !prepared.skipped.is_empty() {
        writeln!(out, "Skipped sources:").unwrap();
        for s in &prepared.skipped {
            let why = match s.reason {
                SkipReason::Disabled => "disabled",
                SkipReason::NoCountryCovered => "covers no country of the scope",
            };
            writeln!(out, "  {:<16} {why}", s.id.0).unwrap();
        }
    }
    for (source, partitions) in downloads_again(setup, prepared) {
        writeln!(
            out,
            "Downloads again: {source} for {} (its downloads were removed after the last build, and its staging changed)",
            partitions.join(", ")
        )
        .unwrap();
    }
    if !prepared.selected.is_empty() && !setup.data.accept_licenses {
        writeln!(out, "Downloads are blocked until [data] accept_licenses = true (review the licenses above).").unwrap();
    }
    out
}

/// Downloads the packs of the countries of the scope that are missing or changed in the registry.
pub fn ensure_packs(setup: &Setup, packs: &Path) -> Result<String> {
    use geko_country_pack::registry::{self, Outcome};
    let mut out = String::new();
    for country in setup.scope.countries() {
        let outcome = registry::ensure(packs, country, &setup.config.packs.registry, setup.data.offline)?;
        let what = match outcome {
            Outcome::Downloaded => "downloaded".to_owned(),
            Outcome::UpToDate => "up to date".to_owned(),
            Outcome::Local(why) => format!("local copy ({why})"),
        };
        let version = geko_country_pack::get(country)?.text.version;
        writeln!(out, "pack {:<12} version {version}, {what}", country.lowercase()).unwrap();
    }
    Ok(out)
}

pub fn purge(setup: &Setup, prepared: &Prepared) -> Result<String> {
    let removed = geko_build::purge::purge(&setup.data_dir, &prepared.scope, &prepared.selected)?;
    let mut out = String::new();
    for path in &removed {
        writeln!(out, "purged {}", path.strip_prefix(&setup.data_dir).unwrap_or(path).display()).unwrap();
    }
    Ok(out)
}

/// Downloading needs the licenses of the selected sources accepted (`[data] accept_licenses`).
pub fn licenses_accepted(setup: &Setup, prepared: &Prepared) -> Result<()> {
    if prepared.selected.is_empty() || setup.data.accept_licenses {
        return Ok(());
    }
    let licenses: Vec<_> =
        prepared.selected.iter().map(|s| format!("{} ({})", s.provider.id().0, s.provider.license().name)).collect();
    Err(GekoError::Config(format!(
        "downloading requires accepting the source licenses: {}. Set [data] accept_licenses = true",
        licenses.join(", ")
    )))
}

pub fn fetch(setup: &Setup, prepared: &Prepared, dl: &dyn Downloader) -> Result<String> {
    fetched(setup, prepared, dl).map(|(out, _)| out)
}

/// Fetches every source; returns the report and the identities of the downloads.
fn fetched(setup: &Setup, prepared: &Prepared, dl: &dyn Downloader) -> Result<(String, Identities)> {
    licenses_accepted(setup, prepared)?;
    let results = fetch_all(&prepared.selected, &prepared.scope, dl, &setup.data_dir, setup.data.parallel_downloads);
    let identities = geko_build::fetch::identities(&results);
    let mut out = String::new();
    let mut failed = Vec::new();
    for (id, result) in results {
        match result {
            Ok(report) => writeln!(
                out,
                "{:<16} downloaded: {}, up to date: {}",
                id.0,
                report.downloaded.len(),
                report.not_modified.len()
            )
            .unwrap(),
            Err(e) => {
                writeln!(out, "{:<16} FAILED: {e}", id.0).unwrap();
                failed.push(id.0);
            }
        }
    }
    if failed.is_empty() {
        if dl.checks_for_updates() {
            mark_checked(setup)?;
        }
        Ok((out, identities))
    } else {
        Err(GekoError::Data(format!("{out}failed sources: {}", failed.join(", "))))
    }
}

/// Fetch, stage every partition, fuse and index each country, and leave a segment per country for
/// the bundle. Every stage whose code and inputs are unchanged since the last build is reused
/// (PROJECT.md §8.3), unless `force`.
pub fn build(setup: &Setup, prepared: &Prepared, dl: &dyn Downloader, force: bool) -> Result<Built> {
    let mut log = StageLog::new();
    let mut out = String::new();
    if setup.data.offline && !setup.data.keep_downloads {
        writeln!(out, "offline: the downloads are kept, although [data] keep_downloads = false").unwrap();
    }
    out.push_str(&stage(setup, prepared, dl, force)?);
    log.done("fetch and stage", false);
    if prepared.selected.is_empty() {
        return Ok(Built { report: out, stages: log.into_records(), ..Default::default() });
    }
    let (mut meta, mut parts) = (Meta::default(), Vec::new());
    let (geocode_format, reverse_format) = crate::sources::index_formats();
    for country in prepared.scope.countries() {
        let dir = format!("segments/{}", country.lowercase());
        let segment = Segment { country: country.to_string(), dir: dir.clone(), geocode: geocode_format.clone(), reverse: reverse_format.clone() };
        meta.segments.push(segment);
        parts.extend(PARTS.map(|part| BundlePart { from: segment_dir(&setup.data_dir, country).join(part), to: format!("{dir}/{part}") }));
        let normalizer = sources::normalizer(country)?;
        let fusion_code = format!("{} {}", sources::FUSION_CODE, sources::normalizer_code(country)?);
        let partitions: Vec<_> = prepared.scope.partitions().into_iter().filter(|p| p.country == country).collect();
        let enabled: Vec<String> = prepared.selected.iter().map(|s| s.provider.id().0).collect();
        let admin_levels = sources::admin_levels(country)?;
        let thresholds = geko_build::fusion::metrics::Thresholds::default();
        let inputs = geko_build::fusion::Inputs {
            sources: &enabled,
            normalizer: normalizer.as_ref(),
            admin_levels: &admin_levels,
            thresholds: &thresholds,
        };
        let (relocated, relocated_reused) = geko_build::fusion::relocate::relocate_incremental(
            &setup.data_dir,
            country,
            &partitions,
            &enabled,
            sources::FUSION_CODE,
            force,
        )?;
        writeln!(
            out,
            "relocated {country}: {} records into the partition that contains them, {} outside every partition, in {} ms{}",
            relocated.total(),
            relocated.unplaced,
            relocated.elapsed_ms,
            up_to_date(relocated_reused)
        )
        .unwrap();
        log.done(format!("relocate {country}"), relocated_reused);
        let started = std::time::Instant::now();
        let mut reports = Vec::new();
        let mut reused = 0;
        // Every partition is fused even if another fails, so one build shows them all.
        let mut failed = Vec::new();
        for partition in &partitions {
            let fused = geko_build::fusion::fuse_partition_incremental(&setup.data_dir, partition, &inputs, &fusion_code, force);
            let (report, was_reused) = match fused {
                Ok(fused) => fused,
                Err(e) => {
                    tracing::error!(partition = %geko_build::fusion::merge::partition_name(partition), "{e}");
                    failed.push(e.to_string());
                    continue;
                }
            };
            tracing::info!(
                partition = %report.partition,
                addresses = report.entities["addresses"],
                elapsed_ms = report.elapsed_ms,
                reused = was_reused,
                "partition fused"
            );
            reused += usize::from(was_reused);
            reports.push(report);
        }
        if !failed.is_empty() {
            return Err(GekoError::Data(format!(
                "{out}fusion failed in {} of {} partitions of {country}:\n  {}",
                failed.len(),
                partitions.len(),
                failed.join("\n  ")
            )));
        }
        let worst = |rate: fn(&geko_build::fusion::FusionReport) -> f64| reports.iter().map(rate).fold(0.0, f64::max);
        let (address_dups, street_dups) = (worst(|r| r.metrics.address_duplicate_rate), worst(|r| r.metrics.street_duplicate_rate));
        let (report, merge_reused) =
            geko_build::fusion::merge::merge_incremental(&setup.data_dir, country, &partitions, reports, sources::FUSION_CODE, force)?;
        let e = &report.entities;
        writeln!(
            out,
            "fused {country}: {} partitions ({reused} up to date), {} streets, {} addresses, {} places, {} POIs, {} postcodes in {} ms{}; worst duplicates: addresses {:.3}%, streets {:.3}%",
            partitions.len(),
            e["streets"],
            e["addresses"],
            e["places"],
            e["pois"],
            e["postcodes"],
            started.elapsed().as_millis(),
            if merge_reused { ", merge up to date" } else { "" },
            address_dups * 100.0,
            street_dups * 100.0,
        )
        .unwrap();
        log.done(format!("fuse {country}"), merge_reused && reused == partitions.len());

        let canonical = geko_build::fusion::merge::country_dir(&setup.data_dir, country);
        let canonical_digest = incremental::output_of(&canonical, geko_build::fusion::merge::NOT_CANONICAL)?;
        let started = std::time::Instant::now();
        let key = Key::new("geocode")
            .add("code", &format!("{} {}", sources::geocode_code(), sources::normalizer_code(country)?))
            .add("canonical", &canonical_digest)
            .finish();
        let (info, reused) = segment_stage(&segment_dir(&setup.data_dir, country).join("geocode"), key, force, |dir| {
            let index = crate::sources::geocode_builder()?.build(
                &geko_core::ports::CanonicalPartition { country, dir: canonical.clone() },
                normalizer.as_ref(),
                dir,
            )?;
            Ok(serde_json::json!({ "entities": index.entities }))
        })?;
        writeln!(
            out,
            "indexed {country}: {} entities ({} v{}) in {} ms{}",
            info["entities"],
            geocode_format.provider,
            geocode_format.version,
            started.elapsed().as_millis(),
            up_to_date(reused)
        )
        .unwrap();
        log.done(format!("index {country}"), reused);
        let started = std::time::Instant::now();
        let key = Key::new("reverse").add("code", sources::reverse_code()).add("canonical", &canonical_digest).finish();
        let (info, reused) = segment_stage(&segment_dir(&setup.data_dir, country).join("reverse"), key, force, |dir| {
            Ok(serde_json::json!({ "points": crate::sources::build_reverse(&canonical, country, dir)? }))
        })?;
        writeln!(out, "reverse {country}: {} points in {} ms{}", info["points"], started.elapsed().as_millis(), up_to_date(reused)).unwrap();
        log.done(format!("reverse {country}"), reused);
        let pack = geko_country_pack::get(country)?;
        pack.copy_to(&segment_dir(&setup.data_dir, country).join("pack"))?;
        writeln!(out, "pack {country}: version {}", pack.text.version).unwrap();
    }
    let licenses = prepared
        .selected
        .iter()
        .map(|s| {
            let l = s.provider.license();
            LicenseNotice { source: s.provider.id().0, name: l.name, attribution: l.attribution, url: l.url }
        })
        .collect();
    let packs: serde_json::Map<String, serde_json::Value> =
        prepared.scope.countries().map(|c| Ok((c.to_string(), geko_country_pack::get(c)?.text.version.into()))).collect::<Result<_>>()?;
    Ok(Built {
        report: out,
        parts,
        meta: meta.to_value(),
        licenses,
        stages: log.into_records(),
        report_extra: serde_json::Map::from_iter([("packs".to_owned(), packs.into())]),
    })
}

/// After the host published a bundle: removes the downloads that are no longer needed.
pub fn published(setup: &Setup, prepared: &Prepared) -> Result<String> {
    remove_downloads(setup, prepared)
}

fn up_to_date(reused: bool) -> &'static str {
    if reused { " (up to date)" } else { "" }
}

/// Builds a segment part into a fresh `dir` with `build`, unless the stage keyed by `key` already
/// wrote it. Returns what `build` reported and whether it was reused.
fn segment_stage(
    dir: &std::path::Path,
    key: String,
    force: bool,
    build: impl FnOnce(&std::path::Path) -> Result<serde_json::Value>,
) -> Result<(serde_json::Value, bool)> {
    if !force && let Some(stamp) = incremental::up_to_date(dir, &key) {
        return Ok((stamp.info, true));
    }
    incremental::invalidate(dir)?;
    let info = build(&fresh_dir(dir)?)?;
    Ok((incremental::seal(dir, key, &[], info)?.info, false))
}

/// The parts of a segment: the indexes, and the country pack they were built with, which
/// `serve` reads from the bundle (PROJECT.md §2.4).
const PARTS: [&str; 3] = ["geocode", "reverse", "pack"];

/// Working directory where a country segment is built before being published in a bundle.
pub fn segment_dir(data_dir: &Path, country: geko_core::country::CountryCode) -> PathBuf {
    data_dir.join("segments").join(country.lowercase())
}

/// An empty output directory, so files a previous build wrote and this one no longer does are not
/// published. Published bundles hard-link their files, so they keep them.
fn fresh_dir(dir: &std::path::Path) -> Result<std::path::PathBuf> {
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    std::fs::create_dir_all(dir)?;
    Ok(dir.to_path_buf())
}

fn stage(setup: &Setup, prepared: &Prepared, dl: &dyn Downloader, force: bool) -> Result<String> {
    let (mut out, identities) = fetched(setup, prepared, dl)?;
    let mut failed = Vec::new();
    let code = |id: &geko_core::model::SourceId| prepared.codes.get(&id.0).cloned().unwrap_or_default();
    let restore = restorer(setup);
    let staged = stage_all(&prepared.selected, &prepared.scope, &setup.data_dir, &code, force, &identities, &restore);
    for (id, partition, result) in staged {
        let label = match &partition.region {
            Some(r) => format!("{}/{r}", id.0),
            None => format!("{}/{}", id.0, partition.country),
        };
        match result {
            Ok(staged) => writeln!(out, "staged {label:<20} {} records{}", staged.records, up_to_date(staged.reused)).unwrap(),
            Err(e) => {
                writeln!(out, "staged {label:<20} FAILED: {e}").unwrap();
                failed.push(label);
            }
        }
    }
    if failed.is_empty() {
        Ok(out)
    } else {
        Err(GekoError::Data(format!("{out}failed partitions: {}", failed.join(", "))))
    }
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{value:.1} {}", UNITS[unit]) }
}


#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use geko_core::country::{CountryCode, RegionCode};
    use geko_core::model::{License, SourceId};
    use geko_core::ports::{
        DatasetRequest, DownloadOutcome, DownloadRequest, FetchReport, RawDir, SourceProvider, SourceStats, StagingDir,
    };
    use geko_core::scope::{Coverage, Partition};

    struct NoNetwork;

    impl Downloader for NoNetwork {
        fn fetch(&self, _: &DownloadRequest, _: &Path) -> Result<DownloadOutcome> {
            Err(GekoError::Data("no network in tests".into()))
        }
    }

    struct Fake;

    impl SourceProvider for Fake {
        fn id(&self) -> SourceId {
            SourceId("fake".into())
        }
        fn license(&self) -> License {
            License { name: "Fake License".into(), attribution: "test".into(), url: None }
        }
        fn coverage(&self) -> Coverage {
            Coverage::Global
        }
        fn plan(&self, _: &Scope, _: &dyn Downloader, _: &RawDir) -> Result<Vec<DatasetRequest>> {
            Ok(vec![DatasetRequest {
                source: self.id(),
                name: "fake.bin".into(),
                url: "https://example.org/fake.bin".into(),
                estimated_size: Some(3 * 1024 * 1024),
            }])
        }
        fn fetch(&self, _: &Scope, _: &dyn Downloader, _: &RawDir) -> Result<FetchReport> {
            Ok(FetchReport { downloaded: vec!["fake.bin".into()], ..FetchReport::default() })
        }
        fn stage(&self, _: &Partition, _: &RawDir, _: &StagingDir) -> Result<SourceStats> {
            Ok(SourceStats::default())
        }
    }

    fn registry() -> BTreeMap<&'static str, Factory> {
        let factory = Factory { create: |_, _| Ok(Arc::new(Fake)), code: "test", required: false };
        BTreeMap::from([("fake", factory)])
    }

    fn setup(data: DataPolicy) -> Setup {
        geko_country_pack::init(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../countries"));
        // The source that the pack of Spain declares is not what these tests are about.
        let config = Config::parse("[sources.fake]\n\n[sources.cartociudad]\nenabled = false\n", "").unwrap();
        let mut scope = Scope::default();
        scope.add(CountryCode::new("ES").unwrap(), vec![RegionCode::new("ES-B").unwrap()]).unwrap();
        Setup { config, scope, data_dir: std::env::temp_dir().join("geko-plan-tests"), data }
    }

    #[test]
    fn empty_scope_is_an_error() {
        let mut empty = setup(DataPolicy::default());
        empty.scope = Scope::default();
        let err = prepare(&empty, &BTreeMap::new()).err().unwrap();
        assert!(err.to_string().contains("scope is empty"), "{err}");
    }

    #[test]
    fn plan_lists_sources_datasets_and_license_warning() {
        let setup = setup(DataPolicy::default());
        let prepared = prepare(&setup, &registry()).unwrap();
        let report = plan(&setup, &prepared, &NoNetwork);
        assert!(report.contains("ES / ES-B"), "{report}");
        assert!(report.contains("fake") && report.contains("Fake License"), "{report}");
        assert!(report.contains("fake.bin") && report.contains("3.0 MB"), "{report}");
        assert!(report.contains("accept_licenses = true"), "{report}");
    }

    #[test]
    fn fetch_requires_accepting_licenses() {
        let setup_ = setup(DataPolicy::default());
        let prepared = prepare(&setup_, &registry()).unwrap();
        let err = fetch(&setup_, &prepared, &NoNetwork).unwrap_err();
        assert!(err.to_string().contains("fake (Fake License)"), "{err}");

    #[test]
    fn a_geocoder_without_sources_is_an_error() {
        let err = prepare(&setup(DataPolicy::default()), &BTreeMap::new()).err().unwrap();
        assert!(err.to_string().contains("without any source"), "{err}");
    }

        let accepted = setup(DataPolicy { accept_licenses: true, ..DataPolicy::default() });
        let prepared = prepare(&accepted, &registry()).unwrap();
        let out = fetch(&accepted, &prepared, &NoNetwork).unwrap();
        assert!(out.contains("downloaded: 1"), "{out}");
    }

    #[test]
    fn human_sizes() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(271_619_251), "259.0 MB");
    }

    #[test]
    fn sources_are_checked_once_every_max_age_days() {
        let mut setup = setup(DataPolicy { max_age_days: 30, ..DataPolicy::default() });
        setup.data_dir = std::env::temp_dir().join(format!("geko-check-due-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&setup.data_dir);
        assert!(check_due(&setup), "never checked");
        assert!(downloader(&setup, false).checks_for_updates());
        mark_checked(&setup).unwrap();
        assert!(!check_due(&setup));
        let wait = until_check(&setup).as_secs();
        assert!((30 * 86_400 - 5..=30 * 86_400).contains(&wait), "{wait}");
        assert_eq!(next_build(&setup), Some(until_check(&setup).max(MIN_WAIT)));
        assert!(!downloader(&setup, false).checks_for_updates(), "kept until the time is up");
        assert!(downloader(&setup, true).checks_for_updates(), "geko build --refresh");
        setup.data.max_age_days = 0;
        assert!(check_due(&setup));
        assert_eq!(until_check(&setup).as_secs(), 0);
        assert_eq!(next_build(&setup), Some(MIN_WAIT), "never sooner than an hour");
        setup.data.refresh = Refresh::Never;
        assert!(!downloader(&setup, true).checks_for_updates());
        assert_eq!(next_build(&setup), None, "the sources never change");
        std::fs::remove_dir_all(&setup.data_dir).unwrap();
    }
}

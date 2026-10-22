//! Architecture ports (PROJECT.md §4). Only domain types cross these boundaries.

use std::path::{Path, PathBuf};

use crate::Result;
use crate::country::CountryCode;
use crate::geo::Coord;
use crate::model::{AdminChain, HouseNumber, License, LocalId, PlaceRef, PublicId, SourceId, StreetParts};
use crate::query::{Hit, KindFilter, ParsedQuery, QueryContext};
use crate::scope::{Coverage, Partition, Scope};

pub use geko_module_sdk::download::{Checksum, DownloadOutcome, DownloadRequest, Downloader};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatasetRequest {
    pub source: SourceId,
    pub name: String,
    pub url: String,
    pub estimated_size: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct RawDir {
    pub path: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct FetchReport {
    pub downloaded: Vec<String>,
    pub not_modified: Vec<String>,
    /// What identifies each input of the staging (`SourceProvider::stage_inputs`) that the
    /// source can tell without its files: a hash, a release, a marker of a complete download. A
    /// staging is keyed by these rather than by the files, so that it is reused after its
    /// downloads were removed. An input not listed is keyed by the size and time of its files.
    pub identities: Vec<(PathBuf, String)>,
}

#[derive(Debug, Clone)]
pub struct StagingDir {
    pub path: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct SourceStats {
    pub records: u64,
}

/// Construction must be cheap and do no I/O: providers are instantiated to decide
/// whether they take part in a run.
pub trait SourceProvider: Send + Sync {
    fn id(&self) -> SourceId;
    /// How people know it ("OSM", "CartoCiudad"), for the progress of a build; its id by default.
    fn name(&self) -> String {
        self.id().0
    }
    fn license(&self) -> License;
    fn coverage(&self) -> Coverage;
    /// What `fetch` would download for `scope`. May download small catalog metadata
    /// (e.g. an extract index) into `raw`, never the datasets themselves.
    fn plan(&self, scope: &Scope, dl: &dyn Downloader, raw: &RawDir) -> Result<Vec<DatasetRequest>>;
    fn fetch(&self, scope: &Scope, dl: &dyn Downloader, raw: &RawDir) -> Result<FetchReport>;
    fn stage(&self, partition: &Partition, raw: &RawDir, out: &StagingDir) -> Result<SourceStats>;
    /// Whether the source is staged once per country, in its `_all` partition, rather than once
    /// per region: a source not published by region (a world file sorted by place) cannot tell
    /// a region's records from its neighbour's. Relocation then gives every record to the region
    /// that contains it (PROJECT.md §6.5.8).
    fn stage_by_country(&self) -> bool {
        false
    }
    /// Files and directories under `raw` that `stage` reads for `partition`, so that a build
    /// restages a partition only when they change (PROJECT.md §8.3). By default, all of `raw`.
    fn stage_inputs(&self, partition: &Partition, raw: &RawDir) -> Result<Vec<PathBuf>> {
        let _ = partition;
        Ok(vec![raw.path.clone()])
    }
    /// Files and directories under `raw` that no partition of `scope` reads, so that
    /// `geko build --purge` removes them (with the downloader's files beside them). By default,
    /// none: a source that cannot tell them apart keeps everything.
    fn unused_raw(&self, scope: &Scope, raw: &RawDir) -> Result<Vec<PathBuf>> {
        let _ = (scope, raw);
        Ok(Vec::new())
    }
    /// The downloads under `raw` that may be removed once a bundle is published
    /// (`[data] keep_downloads = false`): files whose identity `fetch` reports, so that the
    /// staging is reused without them, and that `fetch` brings back when the downloader does not
    /// keep removed downloads. The records beside them stay. By default, none.
    fn removable(&self, raw: &RawDir) -> Result<Vec<PathBuf>> {
        let _ = raw;
        Ok(Vec::new())
    }
}

/// Word pairs, each in its own language (`TextNormalizer::equivalences`).
pub type WordPairs = [(String, String)];

pub trait TextNormalizer: Send + Sync {
    fn country(&self) -> CountryCode;
    /// Light normalization applied to both indexed names and queries.
    fn normalize(&self, text: &str, out: &mut String);
    /// Splits normalized text into index tokens.
    fn tokenize(&self, normalized: &str, out: &mut Vec<String>);
    /// Index-time expansion: every normalized variant under which a name must be findable.
    fn variants(&self, name: &str, out: &mut Vec<String>);
    /// Splits a street name into its type code and normalized core, for comparing names
    /// across sources and languages (PROJECT.md §6.5.1).
    fn street_parts(&self, name: &str) -> StreetParts;
    /// Splits a street name typed in a query: like `street_parts`, but a country may recognise
    /// its street type despite a typo ("crarer" for "carrer"), so that the type is not searched
    /// as a word of the name. Indexing and fusion never use it.
    fn query_street_parts(&self, name: &str) -> StreetParts {
        self.street_parts(name)
    }
    /// Parses a house number as written in the country; `None` for "no number" (s/n…).
    fn parse_housenumber(&self, raw: &str) -> Option<HouseNumber>;
    /// Whether two street type codes may name the same street. Beyond equal or unknown types,
    /// the country knows which types its sources use for one another.
    fn street_types_compatible(&self, a: Option<&str>, b: Option<&str>) -> bool;
    /// Whether a folded word only links others ("de", "la"): a name is found without it.
    fn is_connector(&self, _folded: &str) -> bool {
        false
    }
    /// Languages of the country (ISO 639-1), whose names (`name:<lang>`) teach which words are
    /// the same in each language. Empty: every language.
    fn languages(&self) -> &[String] {
        &[]
    }
    /// Word pairs known to be the same in two languages of the country, and pairs that must
    /// never be taken as such even if the names suggest it.
    fn equivalences(&self) -> (&WordPairs, &WordPairs) {
        (&[], &[])
    }
}

pub trait AddressParser: Send + Sync {
    fn country(&self) -> CountryCode;
    fn parse(&self, text: &str) -> Result<ParsedQuery>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexFormat {
    pub provider: String,
    pub version: u32,
}

#[derive(Debug, Clone)]
pub struct IndexManifest {
    pub format: IndexFormat,
    pub country: CountryCode,
    pub entities: u64,
}

#[derive(Debug, Clone)]
pub struct CanonicalPartition {
    pub country: CountryCode,
    pub dir: PathBuf,
}

pub trait GeocodeIndexBuilder: Send {
    fn format(&self) -> IndexFormat;
    fn build(&self, data: &CanonicalPartition, normalizer: &dyn TextNormalizer, dir: &Path) -> Result<IndexManifest>;
}

/// Reader of one country segment.
pub trait GeocodeEngine: Send + Sync {
    fn country(&self) -> CountryCode;
    fn search(&self, q: &ParsedQuery, ctx: &QueryContext, out: &mut Vec<Hit>) -> Result<()>;
    fn autocomplete(&self, q: &ParsedQuery, ctx: &QueryContext, out: &mut Vec<Hit>) -> Result<()>;
    fn place(&self, id: LocalId) -> Option<PlaceRef<'_>>;
    fn resolve(&self, id: &PublicId) -> Option<LocalId>;
}

pub trait ReverseIndex: Send + Sync {
    fn country(&self) -> CountryCode;
    fn nearest(&self, p: Coord, filter: KindFilter, k: usize, out: &mut Vec<Hit>) -> Result<()>;
    fn admin_at(&self, p: Coord) -> Option<AdminChain<'_>>;
}

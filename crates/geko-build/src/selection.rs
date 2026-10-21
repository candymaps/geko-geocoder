//! Which configured sources take part in a run (PROJECT.md §7.1):
//! active modules → enabled providers those modules need → countries of the scope they cover.

use std::sync::Arc;

use geko_core::country::CountryCode;
use geko_core::model::SourceId;
use geko_core::module::ModuleName;
use geko_core::ports::SourceProvider;
use geko_core::scope::Scope;

pub struct SelectedSource {
    pub provider: Arc<dyn SourceProvider>,
    pub countries: Vec<CountryCode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// Reported by the caller: disabled sources are never instantiated.
    Disabled,
    NoActiveModuleNeedsIt,
    NoCountryCovered,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedSource {
    pub id: SourceId,
    pub reason: SkipReason,
}

#[derive(Default)]
pub struct SourceSelection {
    pub selected: Vec<SelectedSource>,
    pub skipped: Vec<SkippedSource>,
}

/// `enabled` holds the providers of enabled sources only.
pub fn select_sources(active: &[ModuleName], scope: &Scope, enabled: Vec<Arc<dyn SourceProvider>>) -> SourceSelection {
    let mut selection = SourceSelection::default();
    for provider in enabled {
        let skip = |reason| SkippedSource { id: provider.id(), reason };
        if !provider.used_by().iter().any(|m| active.contains(m)) {
            selection.skipped.push(skip(SkipReason::NoActiveModuleNeedsIt));
            continue;
        }
        let countries = scope.covered_by(&provider.coverage());
        if countries.is_empty() {
            selection.skipped.push(skip(SkipReason::NoCountryCovered));
            continue;
        }
        selection.selected.push(SelectedSource { provider, countries });
    }
    selection
}

#[cfg(test)]
mod tests {
    use super::*;
    use geko_core::Result;
    use geko_core::model::License;
    use geko_core::ports::{DatasetRequest, Downloader, FetchReport, RawDir, SourceStats, StagingDir};
    use geko_core::scope::{Coverage, Partition};

    struct Fake {
        id: &'static str,
        coverage: Coverage,
        used_by: &'static [ModuleName],
    }

    impl SourceProvider for Fake {
        fn id(&self) -> SourceId {
            SourceId(self.id.into())
        }
        fn license(&self) -> License {
            License { name: "test".into(), attribution: "test".into(), url: None }
        }
        fn coverage(&self) -> Coverage {
            self.coverage.clone()
        }
        fn used_by(&self) -> &'static [ModuleName] {
            self.used_by
        }
        fn plan(&self, _: &Scope, _: &dyn Downloader, _: &RawDir) -> Result<Vec<DatasetRequest>> {
            Ok(vec![])
        }
        fn fetch(&self, _: &Scope, _: &dyn Downloader, _: &RawDir) -> Result<FetchReport> {
            Ok(FetchReport::default())
        }
        fn stage(&self, _: &Partition, _: &RawDir, _: &StagingDir) -> Result<SourceStats> {
            Ok(SourceStats::default())
        }
    }

    fn cc(s: &str) -> CountryCode {
        CountryCode::new(s).unwrap()
    }

    fn candidates() -> Vec<Arc<dyn SourceProvider>> {
        let osm = Fake { id: "osm", coverage: Coverage::Global, used_by: &[ModuleName::Geocoder, ModuleName::Tiles] };
        let cartociudad =
            Fake { id: "cartociudad", coverage: Coverage::Countries(vec![cc("ES")]), used_by: &[ModuleName::Geocoder] };
        let ban = Fake { id: "ban", coverage: Coverage::Countries(vec![cc("FR")]), used_by: &[ModuleName::Geocoder] };
        vec![Arc::new(osm), Arc::new(cartociudad), Arc::new(ban)]
    }

    fn scope(countries: &[&str]) -> Scope {
        let mut s = Scope::default();
        for c in countries {
            s.add(cc(c), vec![]).unwrap();
        }
        s
    }

    fn selected_ids(sel: &SourceSelection) -> Vec<String> {
        sel.selected.iter().map(|s| s.provider.id().0).collect()
    }

    fn reason(sel: &SourceSelection, id: &str) -> SkipReason {
        sel.skipped.iter().find(|s| s.id.0 == id).unwrap().reason.clone()
    }

    #[test]
    fn geocoder_in_spain_uses_global_and_spanish_sources() {
        let sel = select_sources(&[ModuleName::Geocoder], &scope(&["ES"]), candidates());
        assert_eq!(selected_ids(&sel), ["osm", "cartociudad"]);
        assert_eq!(reason(&sel, "ban"), SkipReason::NoCountryCovered);
    }

    #[test]
    fn tiles_only_downloads_nothing_for_the_geocoder() {
        let sel = select_sources(&[ModuleName::Tiles], &scope(&["ES", "FR"]), candidates());
        assert_eq!(selected_ids(&sel), ["osm"]);
        assert_eq!(reason(&sel, "cartociudad"), SkipReason::NoActiveModuleNeedsIt);
        assert_eq!(reason(&sel, "ban"), SkipReason::NoActiveModuleNeedsIt);
    }

    #[test]
    fn global_source_covers_every_country_of_the_scope() {
        let sel = select_sources(&[ModuleName::Geocoder], &scope(&["ES", "FR", "PT"]), candidates());
        let osm = sel.selected.iter().find(|s| s.provider.id().0 == "osm").unwrap();
        assert_eq!(osm.countries, [cc("ES"), cc("FR"), cc("PT")]);
        let ban = sel.selected.iter().find(|s| s.provider.id().0 == "ban").unwrap();
        assert_eq!(ban.countries, [cc("FR")]);
    }
}

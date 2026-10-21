use crate::country::CountryCode;
use crate::geo::{BBox, Coord};
use crate::model::{LocalId, MatchType, PlaceKind};

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedQuery {
    pub raw: String,
    /// Sorted by descending probability.
    pub interpretations: Vec<Interpretation>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Interpretation {
    pub probability: f32,
    /// Country whose parser produced this interpretation.
    pub country: CountryCode,
    pub kind: InterpretationKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InterpretationKind {
    Address(AddressComponents),
    /// A named place, optionally within another (e.g. "Sants" within "Barcelona").
    Place { text: String, within: Option<String> },
    Postcode(String),
    Coordinates(Coord),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AddressComponents {
    pub street: Option<String>,
    pub housenumber: Option<String>,
    pub postcode: Option<String>,
    pub locality: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RankingWeights {
    pub text: f32,
    pub importance: f32,
    pub distance: f32,
    pub kind_affinity: f32,
}

impl Default for RankingWeights {
    fn default() -> Self {
        Self { text: 1.0, importance: 0.3, distance: 0.5, kind_affinity: 0.4 }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueryContext {
    pub focus: Option<Coord>,
    pub boundary: Option<BBox>,
    /// Empty means every loaded country.
    pub countries: Vec<CountryCode>,
    pub lang: Option<String>,
    pub size: usize,
    pub weights: RankingWeights,
    pub debug: bool,
    /// Score of the k-th result already found elsewhere (other segments or interpretations):
    /// an engine may stop as soon as it cannot beat it.
    pub min_score: f32,
    /// Time budget (PROJECT.md §9.5): past it, an engine skips its remaining secondary readings
    /// and fallback searches and returns what it has. The first reading always completes.
    pub deadline: Option<std::time::Instant>,
}

impl Default for QueryContext {
    fn default() -> Self {
        Self {
            focus: None,
            boundary: None,
            countries: Vec::new(),
            lang: None,
            size: 10,
            weights: RankingWeights::default(),
            debug: false,
            min_score: f32::NEG_INFINITY,
            deadline: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KindFilter(u16);

impl KindFilter {
    pub const ALL: KindFilter = KindFilter(u16::MAX);

    pub fn only(kinds: &[PlaceKind]) -> Self {
        KindFilter(kinds.iter().fold(0, |acc, k| acc | (1 << *k as u16)))
    }

    pub fn accepts(self, kind: PlaceKind) -> bool {
        self.0 & (1 << kind as u16) != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hit {
    pub country: CountryCode,
    pub id: LocalId,
    pub score: f32,
    pub match_type: MatchType,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_filter() {
        let f = KindFilter::only(&[PlaceKind::Street, PlaceKind::Address]);
        assert!(f.accepts(PlaceKind::Street));
        assert!(f.accepts(PlaceKind::Address));
        assert!(!f.accepts(PlaceKind::Poi));
        assert!(PlaceKind::ALL.iter().all(|k| KindFilter::ALL.accepts(*k)));
    }
}

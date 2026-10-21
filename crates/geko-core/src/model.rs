use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use crate::country::CountryCode;
use crate::geo::Coord;
use crate::{GekoError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PlaceKind {
    Country,
    Region,
    Province,
    Municipality,
    Locality,
    Neighbourhood,
    Toponym,
    Postcode,
    Street,
    Address,
    Poi,
    KilometricPoint,
    Parcel,
}

impl PlaceKind {
    pub const ALL: [PlaceKind; 13] = [
        PlaceKind::Country,
        PlaceKind::Region,
        PlaceKind::Province,
        PlaceKind::Municipality,
        PlaceKind::Locality,
        PlaceKind::Neighbourhood,
        PlaceKind::Toponym,
        PlaceKind::Postcode,
        PlaceKind::Street,
        PlaceKind::Address,
        PlaceKind::Poi,
        PlaceKind::KilometricPoint,
        PlaceKind::Parcel,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            PlaceKind::Country => "country",
            PlaceKind::Region => "region",
            PlaceKind::Province => "province",
            PlaceKind::Municipality => "municipality",
            PlaceKind::Locality => "locality",
            PlaceKind::Neighbourhood => "neighbourhood",
            PlaceKind::Toponym => "toponym",
            PlaceKind::Postcode => "postcode",
            PlaceKind::Street => "street",
            PlaceKind::Address => "address",
            PlaceKind::Poi => "poi",
            PlaceKind::KilometricPoint => "kilometric_point",
            PlaceKind::Parcel => "parcel",
        }
    }
}

impl fmt::Display for PlaceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for PlaceKind {
    type Err = GekoError;

    fn from_str(s: &str) -> Result<Self> {
        PlaceKind::ALL
            .into_iter()
            .find(|k| k.as_str() == s)
            .ok_or_else(|| GekoError::InvalidInput(format!("unknown place kind: {s}")))
    }
}

/// Stable, deterministic identifier exposed through the API: `geko:<kind>:<country>-<key>`.
/// The country selects the segment that holds the entity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PublicId(String);

impl PublicId {
    const PREFIX: &'static str = "geko:";

    pub fn new(kind: PlaceKind, country: CountryCode, key: &str) -> Result<Self> {
        if key.is_empty() || key.chars().any(char::is_whitespace) {
            return Err(GekoError::InvalidInput(format!("invalid public id key: {key:?}")));
        }
        Ok(Self(format!("{}{}:{}-{}", Self::PREFIX, kind, country.lowercase(), key)))
    }

    pub fn parse(s: &str) -> Result<Self> {
        let invalid = || GekoError::InvalidInput(format!("invalid public id: {s:?}"));
        let rest = s.strip_prefix(Self::PREFIX).ok_or_else(invalid)?;
        let (kind, rest) = rest.split_once(':').ok_or_else(invalid)?;
        let (country, key) = rest.split_once('-').ok_or_else(invalid)?;
        if country.bytes().any(|b| b.is_ascii_uppercase()) {
            return Err(invalid());
        }
        Self::new(
            kind.parse().map_err(|_| invalid())?,
            CountryCode::new(country).map_err(|_| invalid())?,
            key,
        )
    }

    fn parts(&self) -> (&str, &str, &str) {
        let rest = &self.0[Self::PREFIX.len()..];
        let (kind, rest) = rest.split_once(':').expect("validated on construction");
        let (country, key) = rest.split_once('-').expect("validated on construction");
        (kind, country, key)
    }

    pub fn kind(&self) -> PlaceKind {
        self.parts().0.parse().expect("validated on construction")
    }

    pub fn country(&self) -> CountryCode {
        CountryCode::new(self.parts().1).expect("validated on construction")
    }

    pub fn key(&self) -> &str {
        self.parts().2
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PublicId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Dense per-segment identifier, assigned in descending order of importance:
/// iterating ids in ascending order visits the most important entities first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LocalId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MatchType {
    Exact,
    Interpolated,
    Approximate,
    StreetOnly,
    LocalityOnly,
    Coordinates,
    Cadastral,
    Kilometric,
}

impl MatchType {
    pub fn as_str(self) -> &'static str {
        match self {
            MatchType::Exact => "exact",
            MatchType::Interpolated => "interpolated",
            MatchType::Approximate => "approximate",
            MatchType::StreetOnly => "street_only",
            MatchType::LocalityOnly => "locality_only",
            MatchType::Coordinates => "coordinates",
            MatchType::Cadastral => "cadastral",
            MatchType::Kilometric => "kilometric",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SourceId(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct License {
    pub name: String,
    pub attribution: String,
    pub url: Option<String>,
}

/// Borrowed view of a place stored in a loaded segment.
#[derive(Debug, Clone)]
pub struct PlaceRef<'a> {
    pub country: CountryCode,
    pub id: LocalId,
    /// Borrowed from the index, or built on demand by indexes that derive it.
    pub public_id: Cow<'a, str>,
    pub kind: PlaceKind,
    pub name: Cow<'a, str>,
    pub point: Coord,
    /// GeoJSON `properties` members, without braces.
    pub json: Cow<'a, [u8]>,
}

/// A street name split for comparison (PROJECT.md §6.5.1): the street type as a code common
/// to every language of the country, and the remaining name normalized.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StreetParts {
    pub type_code: Option<&'static str>,
    pub core: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HouseNumber {
    pub number: u32,
    /// Normalized suffix: a letter or `BIS`, `TER`…
    pub suffix: Option<String>,
    /// Last number of a range such as `187-189`.
    pub range_end: Option<u32>,
}

impl HouseNumber {
    pub fn label(&self) -> String {
        let mut s = self.number.to_string();
        if let Some(end) = self.range_end {
            s.push_str(&format!("-{end}"));
        }
        if let Some(suffix) = &self.suffix {
            s.push_str(suffix);
        }
        s
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AdminLevelRef<'a> {
    pub kind: PlaceKind,
    pub code: &'a str,
    pub name: &'a str,
}

/// Administrative chain of a point, from the country down to the finest known level.
#[derive(Debug, Clone)]
pub struct AdminChain<'a> {
    pub country: CountryCode,
    pub levels: Vec<AdminLevelRef<'a>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn es() -> CountryCode {
        CountryCode::new("ES").unwrap()
    }

    #[test]
    fn public_id_round_trip() {
        let id = PublicId::new(PlaceKind::Street, es(), "08019-carrer-de-mallorca").unwrap();
        assert_eq!(id.as_str(), "geko:street:es-08019-carrer-de-mallorca");
        assert_eq!(PublicId::parse(id.as_str()).unwrap(), id);
        assert_eq!(id.kind(), PlaceKind::Street);
        assert_eq!(id.country(), es());
        assert_eq!(id.key(), "08019-carrer-de-mallorca");
    }

    #[test]
    fn public_id_key_may_contain_separators() {
        let id = PublicId::parse("geko:address:es-08019-carrer-de-mallorca:401:b").unwrap();
        assert_eq!(id.kind(), PlaceKind::Address);
        assert_eq!(id.key(), "08019-carrer-de-mallorca:401:b");
    }

    #[test]
    fn public_id_rejects_invalid_input() {
        for bad in [
            "osm:street:es-1",
            "geko:castle:es-1",
            "geko:street:es-",
            "geko:street:es",
            "geko:street:ES-1",
            "geko:street:esp-1",
        ] {
            assert!(PublicId::parse(bad).is_err(), "{bad}");
        }
        assert!(PublicId::new(PlaceKind::Street, es(), "with space").is_err());
    }

    #[test]
    fn place_kind_round_trip() {
        for kind in PlaceKind::ALL {
            assert_eq!(kind.as_str().parse::<PlaceKind>().unwrap(), kind);
        }
    }
}

/// The OSM `admin_level` of each kind of administrative unit in a country: the levels mean
/// different things in each country, so its pack declares them (PROJECT.md §2.4).
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminLevels {
    pub country: Vec<u8>,
    pub region: Vec<u8>,
    pub province: Vec<u8>,
    pub municipality: Vec<u8>,
    pub neighbourhood: Vec<u8>,
}

impl Default for AdminLevels {
    /// The most common use of the levels, for a country without a pack.
    fn default() -> Self {
        Self { country: vec![2], region: vec![4], province: vec![6], municipality: vec![8], neighbourhood: vec![9, 10] }
    }
}

impl AdminLevels {
    pub fn kind(&self, level: u8) -> Option<PlaceKind> {
        [
            (&self.country, PlaceKind::Country),
            (&self.region, PlaceKind::Region),
            (&self.province, PlaceKind::Province),
            (&self.municipality, PlaceKind::Municipality),
            (&self.neighbourhood, PlaceKind::Neighbourhood),
        ]
        .into_iter()
        .find(|(levels, _)| levels.contains(&level))
        .map(|(_, kind)| kind)
    }
}

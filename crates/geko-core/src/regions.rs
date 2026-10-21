//! The regions of a country, from its pack (PROJECT.md §2.4): the partitions of a build and the
//! larger regions that group them.

use std::collections::BTreeMap;

use crate::country::{CountryCode, RegionCode};
use crate::geo::BBox;
use crate::{GekoError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    pub code: RegionCode,
    pub name: String,
    /// The region that contains this one, if the pack lists it.
    pub parent: Option<RegionCode>,
    /// Facts of the region that some source needs (`postcode_prefix`…), inherited by the
    /// regions it contains.
    pub attributes: BTreeMap<String, String>,
    /// A box that holds the whole region, if the pack gives one: a source that downloads by area
    /// fetches only what the partitions of the scope need, instead of the whole country.
    pub bbox: Option<BBox>,
}

/// The regions of one country. The partitions are the regions that contain no other.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Regions {
    regions: Vec<Region>,
}

impl Regions {
    pub fn new(country: CountryCode, regions: Vec<Region>) -> Result<Self> {
        let invalid = |msg: String| Err(GekoError::Config(format!("regions of {country}: {msg}")));
        for (i, r) in regions.iter().enumerate() {
            if r.code.country() != country {
                return invalid(format!("{} is not a region of {country}", r.code));
            }
            if regions[..i].iter().any(|o| o.code == r.code) {
                return invalid(format!("{} appears more than once", r.code));
            }
            if let Some(parent) = &r.parent
                && !regions.iter().any(|o| &o.code == parent)
            {
                return invalid(format!("the parent {parent} of {} is not listed", r.code));
            }
        }
        let table = Self { regions };
        for r in &table.regions {
            if table.ancestors(&r.code).count() > table.regions.len() {
                return invalid(format!("{} is its own ancestor", r.code));
            }
        }
        Ok(table)
    }

    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    pub fn get(&self, code: &RegionCode) -> Option<&Region> {
        self.regions.iter().find(|r| &r.code == code)
    }

    /// The regions that contain no other, in the order of the pack.
    pub fn partitions(&self) -> impl Iterator<Item = &Region> {
        self.regions.iter().filter(|r| !self.regions.iter().any(|o| o.parent.as_ref() == Some(&r.code)))
    }

    /// The partitions inside `code`: the region itself if it contains no other.
    pub fn partitions_in(&self, code: &RegionCode) -> Result<Vec<&Region>> {
        if self.get(code).is_none() {
            return Err(GekoError::Config(format!("{code} is not a region of the pack of {}", code.country())));
        }
        Ok(self.partitions().filter(|p| self.ancestors(&p.code).any(|a| &a.code == code)).collect())
    }

    /// The region and the regions that contain it, from the smallest.
    pub fn ancestors<'a>(&'a self, code: &RegionCode) -> impl Iterator<Item = &'a Region> + 'a {
        let mut next = self.get(code);
        let mut steps = 0;
        std::iter::from_fn(move || {
            let region = next?;
            steps += 1;
            // A cycle is rejected by `new`; the bound only keeps this from looping before that.
            next = if steps > self.regions.len() { None } else { region.parent.as_ref().and_then(|p| self.get(p)) };
            Some(region)
        })
    }

    /// An attribute of the region or, if it has none, of the nearest region that contains it.
    pub fn attribute(&self, code: &RegionCode, key: &str) -> Option<&str> {
        self.ancestors(code).find_map(|r| r.attributes.get(key)).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rc(s: &str) -> RegionCode {
        RegionCode::new(s).unwrap()
    }

    fn region(code: &str, parent: Option<&str>, prefix: Option<&str>) -> Region {
        Region {
            code: rc(code),
            name: code.into(),
            parent: parent.map(rc),
            attributes: prefix.map(|p| BTreeMap::from([("postcode_prefix".to_owned(), p.to_owned())])).unwrap_or_default(),
            bbox: None,
        }
    }

    fn spain() -> Regions {
        let es = CountryCode::new("ES").unwrap();
        Regions::new(
            es,
            vec![
                region("ES-CT", None, None),
                region("ES-B", Some("ES-CT"), Some("08")),
                region("ES-GI", Some("ES-CT"), Some("17")),
                region("ES-MD", None, None),
                region("ES-M", Some("ES-MD"), Some("28")),
                region("ES-CE", None, Some("51")),
            ],
        )
        .unwrap()
    }

    #[test]
    fn partitions_are_the_regions_that_contain_no_other() {
        let codes = |v: Vec<&Region>| v.into_iter().map(|r| r.code.as_str().to_owned()).collect::<Vec<_>>();
        let t = spain();
        assert_eq!(codes(t.partitions().collect()), ["ES-B", "ES-GI", "ES-M", "ES-CE"]);
        assert_eq!(codes(t.partitions_in(&rc("ES-CT")).unwrap()), ["ES-B", "ES-GI"]);
        assert_eq!(codes(t.partitions_in(&rc("ES-B")).unwrap()), ["ES-B"]);
        assert_eq!(codes(t.partitions_in(&rc("ES-CE")).unwrap()), ["ES-CE"]);
        assert!(t.partitions_in(&rc("ES-XX")).is_err());
    }

    #[test]
    fn attributes_are_inherited() {
        let mut t = spain();
        t.regions[0].attributes.insert("osm".into(), "cataluna".into());
        assert_eq!(t.attribute(&rc("ES-GI"), "osm"), Some("cataluna"));
        assert_eq!(t.attribute(&rc("ES-GI"), "postcode_prefix"), Some("17"));
        assert_eq!(t.attribute(&rc("ES-M"), "osm"), None);
    }

    #[test]
    fn rejects_inconsistent_tables() {
        let es = CountryCode::new("ES").unwrap();
        assert!(Regions::new(es, vec![region("FR-75", None, None)]).is_err());
        assert!(Regions::new(es, vec![region("ES-B", Some("ES-CT"), None)]).is_err());
        assert!(Regions::new(es, vec![region("ES-B", None, None), region("ES-B", None, None)]).is_err());
        assert!(Regions::new(es, vec![region("ES-B", Some("ES-GI"), None), region("ES-GI", Some("ES-B"), None)]).is_err());
    }
}

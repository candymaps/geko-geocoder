//! Administrative assignment by coordinates (PROJECT.md §6.5.4): never by the municipality
//! name a source carries.

use geko_core::model::{AdminLevels, PlaceKind};
use geko_geom::{BBox, PreparedArea, Pt};
use rstar::RTree;
use rstar::primitives::{GeomWithData, Rectangle};

use super::records::AdminRec;

struct Unit {
    admin: usize,
    kind: PlaceKind,
    level: u8,
    area: PreparedArea,
}

pub struct AdminIndex {
    levels: AdminLevels,
    units: Vec<Unit>,
    tree: RTree<GeomWithData<Rectangle<[f64; 2]>, usize>>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Chain {
    pub region: Option<usize>,
    pub province: Option<usize>,
    pub municipality: Option<usize>,
    pub neighbourhood: Option<usize>,
}

impl AdminIndex {
    pub fn new(admins: &[AdminRec], levels: &AdminLevels) -> Self {
        let units: Vec<Unit> = admins
            .iter()
            .enumerate()
            .filter_map(|(i, a)| {
                let kind = levels.kind(a.level)?;
                (kind != PlaceKind::Country && !a.rings.is_empty()).then(|| Unit {
                    admin: i,
                    kind,
                    level: a.level,
                    area: PreparedArea::new(&a.rings),
                })
            })
            .collect();
        let rects = units
            .iter()
            .enumerate()
            .map(|(i, u)| {
                let BBox { min, max } = u.area.bbox();
                GeomWithData::new(Rectangle::from_corners([min.0 as f64, min.1 as f64], [max.0 as f64, max.1 as f64]), i)
            })
            .collect();
        Self { levels: levels.clone(), units, tree: RTree::bulk_load(rects) }
    }

    /// The kind of administrative unit at an OSM `admin_level` in this country.
    pub fn kind(&self, level: u8) -> Option<PlaceKind> {
        self.levels.kind(level)
    }

    pub fn chain(&self, p: Pt) -> Chain {
        let mut chain = Chain::default();
        let mut neighbourhood_level = 0;
        for hit in self.tree.locate_all_at_point(&[p.0 as f64, p.1 as f64]) {
            let unit = &self.units[hit.data];
            if !unit.area.contains(p) {
                continue;
            }
            match unit.kind {
                PlaceKind::Region => chain.region = Some(unit.admin),
                PlaceKind::Province => chain.province = Some(unit.admin),
                PlaceKind::Municipality => chain.municipality = Some(unit.admin),
                PlaceKind::Neighbourhood if unit.level > neighbourhood_level => {
                    neighbourhood_level = unit.level;
                    chain.neighbourhood = Some(unit.admin);
                }
                _ => {}
            }
        }
        chain
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x0: i32, y0: i32, size: i32) -> Vec<Pt> {
        vec![(x0, y0), (x0 + size, y0), (x0 + size, y0 + size), (x0, y0 + size), (x0, y0)]
    }

    fn admin(id: i64, level: u8, ring: Vec<Pt>) -> AdminRec {
        AdminRec {
            osm_id: id,
            level,
            name: None,
            names: Default::default(),
            iso3166_2: None,
            official_ref: None,
            population: None,
            wikidata: None,
            point: ring[0],
            rings: vec![ring],
        }
    }

    #[test]
    fn assigns_the_containing_units() {
        let admins = vec![
            admin(1, 6, square(0, 0, 1000)),
            admin(2, 8, square(0, 0, 500)),
            admin(3, 8, square(500, 0, 500)),
            admin(4, 9, square(0, 0, 250)),
            admin(5, 10, square(0, 0, 100)),
        ];
        let index = AdminIndex::new(&admins, &AdminLevels::default());
        assert_eq!(
            index.chain((50, 50)),
            Chain { region: None, province: Some(0), municipality: Some(1), neighbourhood: Some(4) }
        );
        assert_eq!(index.chain((200, 200)).neighbourhood, Some(3));
        assert_eq!(index.chain((700, 100)).municipality, Some(2));
        assert_eq!(index.chain((2000, 2000)), Chain::default());
    }
}

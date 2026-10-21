//! Fusion quality metrics (PROJECT.md §13.6).

use std::collections::BTreeMap;

use geko_geom::{Pt, distance_m, point_line_m};
use serde::Serialize;

use super::Fused;
use super::ids::Ids;
use geko_core::ports::TextNormalizer;

use super::streets::StreetEntity;

const DUPLICATE_ADDRESS_M: f64 = 50.0;
const DUPLICATE_STREET_M: f64 = 25.0;
const MAX_EXAMPLES: usize = 20;

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct Thresholds {
    pub address_duplicate_rate: f64,
    pub street_duplicate_rate: f64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self { address_duplicate_rate: 0.005, street_duplicate_rate: 0.005 }
    }
}

#[derive(Debug, Clone, Default, Serialize, serde::Deserialize)]
pub struct Metrics {
    pub duplicate_addresses: usize,
    pub address_duplicate_rate: f64,
    pub duplicate_address_examples: Vec<(String, String)>,
    pub duplicate_streets: usize,
    pub street_duplicate_rate: f64,
    pub duplicate_street_examples: Vec<(String, String)>,
    /// Streets that share municipality, locality and name with another: fragments that fusion
    /// failed to join, or homonyms that no source tells apart.
    pub homonymous_streets: usize,
    pub conflicts_by_attribute: BTreeMap<String, usize>,
}

impl Metrics {
    pub fn failures(&self, t: &Thresholds) -> Vec<String> {
        let mut out = Vec::new();
        if self.address_duplicate_rate > t.address_duplicate_rate {
            out.push(format!(
                "address duplicate rate {:.3}% exceeds {:.3}%",
                self.address_duplicate_rate * 100.0,
                t.address_duplicate_rate * 100.0
            ));
        }
        if self.street_duplicate_rate > t.street_duplicate_rate {
            out.push(format!(
                "street duplicate rate {:.3}% exceeds {:.3}%",
                self.street_duplicate_rate * 100.0,
                t.street_duplicate_rate * 100.0
            ));
        }
        out
    }
}

fn street_near(a: &StreetEntity, b: &StreetEntity, max_m: f64) -> bool {
    let samples = |s: &StreetEntity| -> Vec<Pt> {
        let mut pts: Vec<Pt> = s.lines.iter().flat_map(|l| l.iter().step_by((l.len() / 16).max(1)).copied()).collect();
        pts.extend(s.points.iter().step_by((s.points.len() / 16).max(1)).copied());
        pts
    };
    let dist = |p: Pt, s: &StreetEntity| {
        let l = s.lines.iter().map(|l| point_line_m(p, l)).fold(f64::INFINITY, f64::min);
        s.points.iter().map(|q| distance_m(p, *q)).fold(l, f64::min)
    };
    samples(a).into_iter().any(|p| dist(p, b) < max_m) || samples(b).into_iter().any(|p| dist(p, a) < max_m)
}

pub fn compute(f: &Fused, ids: &Ids, normalizer: &dyn TextNormalizer) -> Metrics {
    let mut m = Metrics::default();

    let mut groups: BTreeMap<(usize, u32, Option<&str>), Vec<usize>> = BTreeMap::new();
    for (i, a) in f.addresses.iter().enumerate() {
        if let Some(n) = &a.number {
            groups.entry((a.street, n.number, n.suffix.as_deref())).or_default().push(i);
        }
    }
    for members in groups.values().filter(|g| g.len() > 1) {
        for x in 0..members.len() {
            for y in x + 1..members.len() {
                let (a, b) = (members[x], members[y]);
                if distance_m(f.addresses[a].point, f.addresses[b].point) < DUPLICATE_ADDRESS_M {
                    m.duplicate_addresses += 1;
                    if m.duplicate_address_examples.len() < MAX_EXAMPLES {
                        m.duplicate_address_examples.push((ids.addresses[a].clone(), ids.addresses[b].clone()));
                    }
                }
            }
        }
    }
    m.address_duplicate_rate = m.duplicate_addresses as f64 / f.addresses.len().max(1) as f64;

    let mut by_name: BTreeMap<(Option<usize>, &str), Vec<usize>> = BTreeMap::new();
    for (i, s) in f.streets.iter().enumerate() {
        by_name.entry((s.municipality, s.core.as_str())).or_default().push(i);
    }
    for members in by_name.values().filter(|g| g.len() > 1) {
        for x in 0..members.len() {
            for y in x + 1..members.len() {
                let (a, b) = (&f.streets[members[x]], &f.streets[members[y]]);
                // Between two geometry streets the type is explicit ("Carrer de Sants" next to
                // "Plaça de Sants" is not a duplicate); a street created from addresses may carry
                // a source's default type, so the country's compatibility applies.
                let same_type = if !a.lines.is_empty() && !b.lines.is_empty() {
                    a.type_code == b.type_code || a.type_code.is_none() || b.type_code.is_none()
                } else {
                    normalizer.street_types_compatible(a.type_code, b.type_code)
                };
                if same_type && street_near(a, b, DUPLICATE_STREET_M) {
                    m.duplicate_streets += 1;
                    if m.duplicate_street_examples.len() < MAX_EXAMPLES {
                        m.duplicate_street_examples.push((ids.streets[members[x]].clone(), ids.streets[members[y]].clone()));
                    }
                }
            }
        }
    }
    m.street_duplicate_rate = m.duplicate_streets as f64 / f.streets.len().max(1) as f64;

    let mut labels: BTreeMap<(Option<usize>, Option<&str>, String), usize> = BTreeMap::new();
    for s in &f.streets {
        *labels.entry((s.municipality, s.locality.as_deref(), s.display_name.to_lowercase())).or_default() += 1;
    }
    m.homonymous_streets = labels.values().filter(|&&c| c > 1).sum();

    for c in &f.conflicts {
        *m.conflicts_by_attribute.entry(c.attribute.to_string()).or_default() += 1;
    }
    m
}

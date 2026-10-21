//! Minimal geometry shared by staging and fusion: ring assembly, point-in-polygon, WKB and
//! approximate distances. Coordinates are `(lon_e7, lat_e7)`.

use rustc_hash::FxHashMap;

pub type Pt = (i32, i32);

/// Joins way segments end to end into closed rings. Segments that cannot be closed are dropped.
pub fn assemble_rings(mut segments: Vec<Vec<Pt>>) -> Vec<Vec<Pt>> {
    segments.retain(|s| s.len() >= 2);
    let mut rings = Vec::new();
    let mut open = Vec::new();
    for s in segments {
        if s.len() >= 4 && s.first() == s.last() {
            rings.push(s);
        } else {
            open.push(s);
        }
    }

    let mut by_endpoint: FxHashMap<Pt, Vec<usize>> = FxHashMap::default();
    for (i, s) in open.iter().enumerate() {
        by_endpoint.entry(s[0]).or_default().push(i);
        by_endpoint.entry(*s.last().unwrap()).or_default().push(i);
    }
    let mut used = vec![false; open.len()];
    for start in 0..open.len() {
        if used[start] {
            continue;
        }
        used[start] = true;
        let mut ring = open[start].clone();
        while ring.first() != ring.last() {
            let end = *ring.last().unwrap();
            let next = by_endpoint.get(&end).and_then(|c| c.iter().copied().find(|&i| !used[i]));
            let Some(i) = next else { break };
            used[i] = true;
            let seg = &open[i];
            if seg[0] == end {
                ring.extend_from_slice(&seg[1..]);
            } else {
                ring.extend(seg.iter().rev().skip(1));
            }
        }
        if ring.len() >= 4 && ring.first() == ring.last() {
            rings.push(ring);
        }
    }
    rings
}

/// Even-odd point-in-ring test.
pub fn ring_contains(ring: &[Pt], p: Pt) -> bool {
    let (px, py) = (p.0 as i64, p.1 as i64);
    let mut inside = false;
    for w in ring.windows(2) {
        let ((x1, y1), (x2, y2)) = ((w[0].0 as i64, w[0].1 as i64), (w[1].0 as i64, w[1].1 as i64));
        if (y1 > py) != (y2 > py) {
            let cross = (x2 - x1) as i128 * (py - y1) as i128;
            let x_at = x1 as i128 + cross / (y2 - y1) as i128;
            if (px as i128) < x_at {
                inside = !inside;
            }
        }
    }
    inside
}

#[derive(Debug, Clone)]
pub struct Polygon {
    pub outer: Vec<Pt>,
    pub inners: Vec<Vec<Pt>>,
}

/// Groups rings into polygons: each inner ring goes to the smallest outer ring that contains it.
pub fn build_polygons(outers: Vec<Vec<Pt>>, inners: Vec<Vec<Pt>>) -> Vec<Polygon> {
    let mut polygons: Vec<Polygon> = outers.into_iter().map(|outer| Polygon { outer, inners: Vec::new() }).collect();
    for inner in inners {
        let owner = polygons
            .iter_mut()
            .filter(|p| ring_contains(&p.outer, inner[0]))
            .min_by_key(|p| bbox_area(&p.outer));
        if let Some(p) = owner {
            p.inners.push(inner);
        }
    }
    polygons
}

fn bbox_area(ring: &[Pt]) -> i128 {
    let b = BBox::of(ring.iter().copied());
    (b.max.0 as i128 - b.min.0 as i128) * (b.max.1 as i128 - b.min.1 as i128)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BBox {
    pub min: Pt,
    pub max: Pt,
}

impl BBox {
    pub fn of(points: impl IntoIterator<Item = Pt>) -> Self {
        points.into_iter().fold(
            BBox { min: (i32::MAX, i32::MAX), max: (i32::MIN, i32::MIN) },
            |b, p| BBox { min: (b.min.0.min(p.0), b.min.1.min(p.1)), max: (b.max.0.max(p.0), b.max.1.max(p.1)) },
        )
    }

    pub fn contains(&self, p: Pt) -> bool {
        (self.min.0..=self.max.0).contains(&p.0) && (self.min.1..=self.max.1).contains(&p.1)
    }

    pub fn intersects(&self, o: &BBox) -> bool {
        self.min.0 <= o.max.0 && o.min.0 <= self.max.0 && self.min.1 <= o.max.1 && o.min.1 <= self.max.1
    }
}

/// Point-in-polygon over many rings with edges bucketed by latitude band, so each query only
/// scans the edges of its band. Even-odd over all rings handles holes without ring roles.
pub struct PreparedArea {
    bbox: BBox,
    band_height: i64,
    bands: Vec<Vec<(Pt, Pt)>>,
}

impl PreparedArea {
    pub fn new(rings: &[Vec<Pt>]) -> Self {
        let bbox = BBox::of(rings.iter().flatten().copied());
        let edges: usize = rings.iter().map(|r| r.len()).sum();
        let band_count = (edges / 8).clamp(1, 4096);
        let height = (bbox.max.1 as i64 - bbox.min.1 as i64 + 1).max(1);
        let band_height = (height + band_count as i64 - 1) / band_count as i64;
        let mut bands = vec![Vec::new(); band_count];
        for ring in rings {
            for w in ring.windows(2) {
                let (lo, hi) = (w[0].1.min(w[1].1) as i64, w[0].1.max(w[1].1) as i64);
                let first = ((lo - bbox.min.1 as i64) / band_height) as usize;
                let last = (((hi - bbox.min.1 as i64) / band_height) as usize).min(band_count - 1);
                for band in &mut bands[first..=last] {
                    band.push((w[0], w[1]));
                }
            }
        }
        Self { bbox, band_height, bands }
    }

    pub fn bbox(&self) -> BBox {
        self.bbox
    }

    pub fn contains(&self, p: Pt) -> bool {
        if !self.bbox.contains(p) {
            return false;
        }
        let band = ((p.1 as i64 - self.bbox.min.1 as i64) / self.band_height) as usize;
        let mut inside = false;
        for &(a, b) in &self.bands[band.min(self.bands.len() - 1)] {
            if ring_contains(&[a, b], p) {
                inside = !inside;
            }
        }
        inside
    }
}

pub fn centroid(points: &[Pt]) -> Option<Pt> {
    let pts = match points {
        [] => return None,
        [first, .., last] if first == last => &points[..points.len() - 1],
        _ => points,
    };
    let n = pts.len() as i64;
    let (sx, sy) = pts.iter().fold((0i64, 0i64), |(sx, sy), p| (sx + p.0 as i64, sy + p.1 as i64));
    Some(((sx / n) as i32, (sy / n) as i32))
}

const METERS_PER_DEGREE_LAT: f64 = 110_574.0;
const METERS_PER_DEGREE_LON_AT_EQUATOR: f64 = 111_320.0;

/// Equirectangular approximation, accurate to well under 1 % at the distances used in fusion
/// (up to a few kilometres).
pub fn distance_m(a: Pt, b: Pt) -> f64 {
    let lat = ((a.1 as f64 + b.1 as f64) / 2.0 * 1e-7).to_radians();
    let dx = (b.0 - a.0) as f64 * 1e-7 * METERS_PER_DEGREE_LON_AT_EQUATOR * lat.cos();
    let dy = (b.1 - a.1) as f64 * 1e-7 * METERS_PER_DEGREE_LAT;
    (dx * dx + dy * dy).sqrt()
}

/// Distance from `p` to the segment `a`–`b`, with the same approximation as `distance_m`.
pub fn point_segment_m(p: Pt, a: Pt, b: Pt) -> f64 {
    let lat = (p.1 as f64 * 1e-7).to_radians();
    let kx = 1e-7 * METERS_PER_DEGREE_LON_AT_EQUATOR * lat.cos();
    let ky = 1e-7 * METERS_PER_DEGREE_LAT;
    let (px, py) = ((p.0 - a.0) as f64 * kx, (p.1 - a.1) as f64 * ky);
    let (bx, by) = ((b.0 - a.0) as f64 * kx, (b.1 - a.1) as f64 * ky);
    let len2 = bx * bx + by * by;
    let t = if len2 == 0.0 { 0.0 } else { ((px * bx + py * by) / len2).clamp(0.0, 1.0) };
    ((px - t * bx).powi(2) + (py - t * by).powi(2)).sqrt()
}

/// Distance from `p` to a polyline.
pub fn point_line_m(p: Pt, line: &[Pt]) -> f64 {
    match line {
        [] => f64::INFINITY,
        [only] => distance_m(p, *only),
        _ => line.windows(2).map(|w| point_segment_m(p, w[0], w[1])).fold(f64::INFINITY, f64::min),
    }
}

pub mod wkb {
    use super::{Polygon, Pt};

    const SCALE: f64 = 1e7;

    /// Parsed WKB geometry: lines for (multi)linestrings, rings for (multi)polygons.
    #[derive(Debug, Clone, PartialEq)]
    pub enum Geometry {
        Point(Pt),
        Lines(Vec<Vec<Pt>>),
        Polygons(Vec<Vec<Vec<Pt>>>),
    }

    struct Reader<'a> {
        bytes: &'a [u8],
        pos: usize,
        little: bool,
    }

    impl Reader<'_> {
        fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
            let slice = self.bytes.get(self.pos..self.pos + N)?;
            self.pos += N;
            slice.try_into().ok()
        }

        fn u32(&mut self) -> Option<u32> {
            let b = self.take::<4>()?;
            Some(if self.little { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) })
        }

        fn f64(&mut self) -> Option<f64> {
            let b = self.take::<8>()?;
            Some(if self.little { f64::from_le_bytes(b) } else { f64::from_be_bytes(b) })
        }

        fn header(&mut self) -> Option<u32> {
            self.little = self.take::<1>()?[0] == 1;
            self.u32()
        }

        fn point(&mut self) -> Option<Pt> {
            let (x, y) = (self.f64()?, self.f64()?);
            Some(((x * SCALE).round() as i32, (y * SCALE).round() as i32))
        }

        fn points(&mut self) -> Option<Vec<Pt>> {
            let n = self.u32()? as usize;
            (0..n).map(|_| self.point()).collect()
        }

        fn polygon(&mut self) -> Option<Vec<Vec<Pt>>> {
            let n = self.u32()? as usize;
            (0..n).map(|_| self.points()).collect()
        }

        fn geometry(&mut self) -> Option<Geometry> {
            match self.header()? {
                1 => Some(Geometry::Point(self.point()?)),
                2 => Some(Geometry::Lines(vec![self.points()?])),
                3 => Some(Geometry::Polygons(vec![self.polygon()?])),
                5 => {
                    let n = self.u32()?;
                    let mut lines = Vec::new();
                    for _ in 0..n {
                        self.header()?;
                        lines.push(self.points()?);
                    }
                    Some(Geometry::Lines(lines))
                }
                6 => {
                    let n = self.u32()?;
                    let mut polygons = Vec::new();
                    for _ in 0..n {
                        self.header()?;
                        polygons.push(self.polygon()?);
                    }
                    Some(Geometry::Polygons(polygons))
                }
                _ => None,
            }
        }
    }

    /// Parses 2D WKB (Point, LineString, Polygon, MultiLineString, MultiPolygon).
    pub fn parse(bytes: &[u8]) -> Option<Geometry> {
        Reader { bytes, pos: 0, little: true }.geometry()
    }

    fn header(out: &mut Vec<u8>, kind: u32) {
        out.push(1);
        out.extend_from_slice(&kind.to_le_bytes());
    }

    fn coords(out: &mut Vec<u8>, pts: &[Pt]) {
        out.extend_from_slice(&(pts.len() as u32).to_le_bytes());
        for p in pts {
            out.extend_from_slice(&(p.0 as f64 / SCALE).to_le_bytes());
            out.extend_from_slice(&(p.1 as f64 / SCALE).to_le_bytes());
        }
    }

    pub fn line_string(pts: &[Pt]) -> Vec<u8> {
        let mut out = Vec::with_capacity(9 + pts.len() * 16);
        header(&mut out, 2);
        coords(&mut out, pts);
        out
    }

    pub fn multi_line_string(lines: &[Vec<Pt>]) -> Vec<u8> {
        let mut out = Vec::new();
        header(&mut out, 5);
        out.extend_from_slice(&(lines.len() as u32).to_le_bytes());
        for line in lines {
            header(&mut out, 2);
            coords(&mut out, line);
        }
        out
    }

    pub fn multi_polygon(polygons: &[Polygon]) -> Vec<u8> {
        let mut out = Vec::new();
        header(&mut out, 6);
        out.extend_from_slice(&(polygons.len() as u32).to_le_bytes());
        for p in polygons {
            header(&mut out, 3);
            out.extend_from_slice(&(1 + p.inners.len() as u32).to_le_bytes());
            coords(&mut out, &p.outer);
            for inner in &p.inners {
                coords(&mut out, inner);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x0: i32, y0: i32, size: i32) -> Vec<Pt> {
        vec![(x0, y0), (x0 + size, y0), (x0 + size, y0 + size), (x0, y0 + size), (x0, y0)]
    }

    #[test]
    fn assembles_rings_from_unordered_and_reversed_segments() {
        let segments = vec![
            vec![(10, 0), (10, 10)],
            vec![(0, 0), (10, 0)],
            vec![(0, 10), (10, 10)],
            vec![(0, 10), (0, 0)],
            vec![(50, 50), (60, 60)],
        ];
        let rings = assemble_rings(segments);
        assert_eq!(rings.len(), 1);
        let ring = &rings[0];
        assert_eq!(ring.first(), ring.last());
        assert_eq!(ring.len(), 5);
        assert!(ring_contains(ring, (5, 5)));
        assert!(!ring_contains(ring, (15, 5)));
    }

    #[test]
    fn closed_ways_are_rings_on_their_own() {
        assert_eq!(assemble_rings(vec![square(0, 0, 10)]).len(), 1);
    }

    #[test]
    fn prepared_area_honours_holes_and_multiple_parts() {
        let rings = vec![square(0, 0, 100), square(40, 40, 20), square(200, 0, 10)];
        let area = PreparedArea::new(&rings);
        assert!(area.contains((10, 10)));
        assert!(!area.contains((50, 50)), "inside the hole");
        assert!(area.contains((205, 5)), "second part");
        assert!(!area.contains((150, 50)));
        assert!(!area.contains((-1, 50)));
    }

    #[test]
    fn prepared_area_matches_brute_force() {
        let mut ring: Vec<Pt> = (0..360)
            .step_by(7)
            .map(|deg| {
                let r = if deg % 2 == 0 { 1_000_000.0 } else { 600_000.0 };
                let a = (deg as f64).to_radians();
                ((r * a.cos()) as i32, (r * a.sin()) as i32)
            })
            .collect();
        ring.push(ring[0]);
        let area = PreparedArea::new(std::slice::from_ref(&ring));
        for x in (-1_100_000..1_100_000).step_by(37_013) {
            for y in (-1_100_000..1_100_000).step_by(41_011) {
                assert_eq!(area.contains((x, y)), ring_contains(&ring, (x, y)), "({x}, {y})");
            }
        }
    }

    #[test]
    fn inner_rings_go_to_their_outer() {
        let polygons = build_polygons(vec![square(0, 0, 100), square(200, 0, 100)], vec![square(210, 10, 10)]);
        assert!(polygons[0].inners.is_empty());
        assert_eq!(polygons[1].inners.len(), 1);
    }

    #[test]
    fn centroid_ignores_closing_point() {
        assert_eq!(centroid(&square(0, 0, 10)), Some((5, 5)));
        assert_eq!(centroid(&[]), None);
    }

    #[test]
    fn wkb_round_trip() {
        let line = vec![(21_686_000, 413_874_000), (21_700_000, 413_880_000)];
        assert_eq!(wkb::parse(&wkb::line_string(&line)), Some(wkb::Geometry::Lines(vec![line])));
        let polygons = build_polygons(vec![square(0, 0, 100)], vec![square(10, 10, 10)]);
        let parsed = wkb::parse(&wkb::multi_polygon(&polygons)).unwrap();
        assert_eq!(parsed, wkb::Geometry::Polygons(vec![vec![square(0, 0, 100), square(10, 10, 10)]]));
        assert_eq!(wkb::parse(&[1, 2]), None);
        let lines = vec![vec![(0, 0), (10, 10)], vec![(20, 20), (30, 30), (40, 40)]];
        assert_eq!(wkb::parse(&wkb::multi_line_string(&lines)), Some(wkb::Geometry::Lines(lines)));
    }

    #[test]
    fn distances_in_metres() {
        let a = (21_686_000, 413_874_000);
        let east = (21_686_000 + 1_197, 413_874_000);
        assert!((distance_m(a, east) - 10.0).abs() < 0.1, "{}", distance_m(a, east));
        let north = (21_686_000, 413_874_000 + 904);
        assert!((distance_m(a, north) - 10.0).abs() < 0.1, "{}", distance_m(a, north));
        let seg = [(21_686_000, 413_874_000), (21_686_000 + 11_970, 413_874_000)];
        let p = (21_686_000 + 5_985, 413_874_000 + 904);
        assert!((point_line_m(p, &seg) - 10.0).abs() < 0.1);
        assert!((point_line_m((21_686_000 - 1_197, 413_874_000), &seg) - 10.0).abs() < 0.1);
    }

    #[test]
    fn wkb_line_string_layout() {
        let bytes = wkb::line_string(&[(10_000_000, 20_000_000), (30_000_000, 40_000_000)]);
        assert_eq!(bytes.len(), 1 + 4 + 4 + 2 * 16);
        assert_eq!(bytes[0], 1);
        assert_eq!(u32::from_le_bytes(bytes[1..5].try_into().unwrap()), 2);
        assert_eq!(f64::from_le_bytes(bytes[9..17].try_into().unwrap()), 1.0);
        assert_eq!(f64::from_le_bytes(bytes[17..25].try_into().unwrap()), 2.0);
    }
}

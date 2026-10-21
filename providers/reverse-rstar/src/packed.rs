//! Static R-tree packed along a Hilbert curve, read in place from a mapped file: opening it
//! costs nothing and its pages are shared with the OS cache, unlike an R-tree rebuilt in RAM.
//!
//! Level 0 are the points themselves, sorted by Hilbert key; each node of level `l + 1` is the
//! bounding box of `NODE` consecutive entries of level `l`, up to a single root.

use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

use bytemuck::{Pod, Zeroable};

pub const NODE: usize = 16;

/// Bounding box in 1e-7 degrees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Pod, Zeroable)]
#[repr(C)]
pub struct Box4 {
    pub min_lon: i32,
    pub min_lat: i32,
    pub max_lon: i32,
    pub max_lat: i32,
}

impl Box4 {
    const EMPTY: Self = Self { min_lon: i32::MAX, min_lat: i32::MAX, max_lon: i32::MIN, max_lat: i32::MIN };

    fn extend(&mut self, b: &Box4) {
        self.min_lon = self.min_lon.min(b.min_lon);
        self.min_lat = self.min_lat.min(b.min_lat);
        self.max_lon = self.max_lon.max(b.max_lon);
        self.max_lat = self.max_lat.max(b.max_lat);
    }

    fn point(lon: i32, lat: i32) -> Self {
        Self { min_lon: lon, min_lat: lat, max_lon: lon, max_lat: lat }
    }
}

/// Position on a Hilbert curve of order 16 (Warren, "Hacker's Delight", via flatbush).
pub fn hilbert(x: u32, y: u32) -> u32 {
    let mut a = x ^ y;
    let mut b = 0xFFFF ^ a;
    let mut c = 0xFFFF ^ (x | y);
    let mut d = x & (y ^ 0xFFFF);

    let mut aa = a | (b >> 1);
    let mut bb = (a >> 1) ^ a;
    let mut cc = ((c >> 1) ^ (b & (d >> 1))) ^ c;
    let mut dd = ((a & (c >> 1)) ^ (d >> 1)) ^ d;

    a = aa;
    b = bb;
    c = cc;
    d = dd;
    aa = (a & (a >> 2)) ^ (b & (b >> 2));
    bb = (a & (b >> 2)) ^ (b & ((a ^ b) >> 2));
    cc ^= (a & (c >> 2)) ^ (b & (d >> 2));
    dd ^= (b & (c >> 2)) ^ ((a ^ b) & (d >> 2));

    a = aa;
    b = bb;
    c = cc;
    d = dd;
    aa = (a & (a >> 4)) ^ (b & (b >> 4));
    bb = (a & (b >> 4)) ^ (b & ((a ^ b) >> 4));
    cc ^= (a & (c >> 4)) ^ (b & (d >> 4));
    dd ^= (b & (c >> 4)) ^ ((a ^ b) & (d >> 4));

    a = aa;
    b = bb;
    c = cc;
    d = dd;
    cc ^= (a & (c >> 8)) ^ (b & (d >> 8));
    dd ^= (b & (c >> 8)) ^ ((a ^ b) & (d >> 8));

    a = cc ^ (cc >> 1);
    b = dd ^ (dd >> 1);

    let mut i0 = x ^ y;
    let mut i1 = b | (0xFFFF ^ (i0 | a));

    i0 = (i0 | (i0 << 8)) & 0x00FF_00FF;
    i0 = (i0 | (i0 << 4)) & 0x0F0F_0F0F;
    i0 = (i0 | (i0 << 2)) & 0x3333_3333;
    i0 = (i0 | (i0 << 1)) & 0x5555_5555;

    i1 = (i1 | (i1 << 8)) & 0x00FF_00FF;
    i1 = (i1 | (i1 << 4)) & 0x0F0F_0F0F;
    i1 = (i1 | (i1 << 2)) & 0x3333_3333;
    i1 = (i1 | (i1 << 1)) & 0x5555_5555;

    (i1 << 1) | i0
}

/// Order in which to store `coords` (lon, lat) so that neighbours on the curve are neighbours
/// on the map.
pub fn hilbert_order(coords: &[[i32; 2]]) -> Vec<u32> {
    let mut bbox = Box4::EMPTY;
    for &[lon, lat] in coords {
        bbox.extend(&Box4::point(lon, lat));
    }
    let span = |min: i32, max: i32, v: i32| -> u32 {
        let w = (max as i64 - min as i64).max(1);
        ((v as i64 - min as i64) * 0xFFFF / w) as u32
    };
    let mut keyed: Vec<(u32, u32)> = coords
        .iter()
        .enumerate()
        .map(|(i, &[lon, lat])| (hilbert(span(bbox.min_lon, bbox.max_lon, lon), span(bbox.min_lat, bbox.max_lat, lat)), i as u32))
        .collect();
    keyed.sort_unstable();
    keyed.into_iter().map(|(_, i)| i).collect()
}

/// Boxes of every level above the points, bottom-up, and where each level starts.
pub fn build_levels(coords: &[[i32; 2]]) -> (Vec<Box4>, Vec<u64>) {
    let mut boxes = Vec::new();
    let mut starts = Vec::new();
    let mut below: Vec<Box4> = coords.iter().map(|&[lon, lat]| Box4::point(lon, lat)).collect();
    while below.len() > 1 {
        let level: Vec<Box4> = below
            .chunks(NODE)
            .map(|c| {
                let mut b = Box4::EMPTY;
                c.iter().for_each(|x| b.extend(x));
                b
            })
            .collect();
        starts.push(boxes.len() as u64);
        boxes.extend_from_slice(&level);
        below = level;
    }
    (boxes, starts)
}

/// A packed tree over borrowed (mapped) slices.
pub struct Packed<'a> {
    pub coords: &'a [[i32; 2]],
    pub boxes: &'a [Box4],
    pub starts: &'a [u64],
    /// cos(mean latitude): longitude differences are scaled so that distances are proportional
    /// to metres.
    pub lon_scale: f64,
}

impl Packed<'_> {
    fn level_len(&self, level: usize) -> usize {
        if level == 0 {
            return self.coords.len();
        }
        let start = self.starts[level - 1] as usize;
        let end = self.starts.get(level).map_or(self.boxes.len(), |&s| s as usize);
        end - start
    }

    fn box_at(&self, level: usize, i: usize) -> Box4 {
        if level == 0 {
            let [lon, lat] = self.coords[i];
            Box4::point(lon, lat)
        } else {
            self.boxes[self.starts[level - 1] as usize + i]
        }
    }

    fn dist2(&self, b: &Box4, lon: i32, lat: i32) -> f64 {
        let axis = |v: i32, min: i32, max: i32| -> f64 {
            if v < min {
                (min as i64 - v as i64) as f64
            } else if v > max {
                (v as i64 - max as i64) as f64
            } else {
                0.0
            }
        };
        let dx = axis(lon, b.min_lon, b.max_lon) * self.lon_scale;
        let dy = axis(lat, b.min_lat, b.max_lat);
        dx * dx + dy * dy
    }

    /// Indices of points by increasing distance, until `visit` returns false.
    pub fn nearest(&self, lon: i32, lat: i32, mut visit: impl FnMut(usize) -> bool) {
        if self.coords.is_empty() {
            return;
        }
        // Squared distances are non-negative, so their bit patterns sort like the floats; the
        // payload packs the level (high half) and the index within it.
        let entry = |d: f64, level: usize, i: usize| Reverse((d.to_bits(), ((level as u64) << 32) | i as u64));
        HEAP.with_borrow_mut(|heap| {
            heap.clear();
            let top = self.starts.len();
            for i in 0..self.level_len(top) {
                heap.push(entry(self.dist2(&self.box_at(top, i), lon, lat), top, i));
            }
            while let Some(Reverse((_, payload))) = heap.pop() {
                let (level, i) = ((payload >> 32) as usize, (payload & 0xFFFF_FFFF) as usize);
                if level == 0 {
                    if !visit(i) {
                        return;
                    }
                    continue;
                }
                let below = level - 1;
                let end = ((i + 1) * NODE).min(self.level_len(below));
                if below == 0 {
                    for (child, &[x, y]) in self.coords[i * NODE..end].iter().enumerate() {
                        heap.push(entry(self.dist2(&Box4::point(x, y), lon, lat), 0, i * NODE + child));
                    }
                } else {
                    let start = self.starts[below - 1] as usize;
                    for (child, b) in self.boxes[start + i * NODE..start + end].iter().enumerate() {
                        heap.push(entry(self.dist2(b, lon, lat), below, i * NODE + child));
                    }
                }
            }
        });
    }
}

thread_local! {
    /// Reused across queries: a reverse lookup should not allocate.
    static HEAP: RefCell<BinaryHeap<Reverse<(u64, u64)>>> = const { RefCell::new(BinaryHeap::new()) };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(coords: &[[i32; 2]]) -> (Vec<[i32; 2]>, Vec<Box4>, Vec<u64>) {
        let order = hilbert_order(coords);
        let sorted: Vec<[i32; 2]> = order.iter().map(|&i| coords[i as usize]).collect();
        let (boxes, starts) = build_levels(&sorted);
        (sorted, boxes, starts)
    }

    #[test]
    fn nearest_matches_brute_force() {
        let mut seed = 7u64;
        let mut next = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 33) % 2_000_000) as i32
        };
        let coords: Vec<[i32; 2]> = (0..5_000).map(|_| [next(), next()]).collect();
        let (sorted, boxes, starts) = tree(&coords);
        let packed = Packed { coords: &sorted, boxes: &boxes, starts: &starts, lon_scale: 0.77 };
        for _ in 0..50 {
            let (lon, lat) = (next(), next());
            let mut got = Vec::new();
            packed.nearest(lon, lat, |i| {
                got.push(i);
                got.len() < 10
            });
            let d = |&[x, y]: &[i32; 2]| ((x - lon) as f64 * 0.77).powi(2) + ((y - lat) as f64).powi(2);
            let mut expected: Vec<f64> = sorted.iter().map(d).collect();
            expected.sort_by(f64::total_cmp);
            let got: Vec<f64> = got.iter().map(|&i| d(&sorted[i])).collect();
            assert_eq!(got, expected[..10]);
        }
    }

    #[test]
    fn single_point_and_empty() {
        let (sorted, boxes, starts) = tree(&[[5, 5]]);
        let packed = Packed { coords: &sorted, boxes: &boxes, starts: &starts, lon_scale: 1.0 };
        let mut seen = Vec::new();
        packed.nearest(0, 0, |i| {
            seen.push(i);
            true
        });
        assert_eq!(seen, [0]);
        let empty = Packed { coords: &[], boxes: &[], starts: &[], lon_scale: 1.0 };
        empty.nearest(0, 0, |_| panic!("no points"));
    }
}

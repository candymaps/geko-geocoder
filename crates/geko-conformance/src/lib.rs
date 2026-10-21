//! Conformance suites, one per port (PROJECT.md §2.1 rule 5, §13.5). Every implementation of a
//! port runs the suite of its port from its own tests:
//!
//! ```ignore
//! #[test]
//! fn conformance() {
//!     let fixture = geko_conformance::fixture::canonical(&country_pack_es::normalizer());
//!     geko_conformance::geocode::check(&FstIndexBuilder, &open, &country_pack_es::normalizer(), &fixture);
//! }
//! ```

pub mod download;
pub mod fixture;
pub mod geocode;
pub mod reverse;
pub mod source;
pub mod text;

use geko_core::geo::Coord;

/// Approximate distance in metres, enough for the tolerances of the suites.
pub(crate) fn distance_m(a: Coord, b: Coord) -> f64 {
    let lat = (a.lat_e7() as f64 + b.lat_e7() as f64) / 2e7;
    let dy = (a.lat_e7() - b.lat_e7()) as f64 / 1e7 * 111_320.0;
    let dx = (a.lon_e7() - b.lon_e7()) as f64 / 1e7 * 111_320.0 * lat.to_radians().cos();
    (dx * dx + dy * dy).sqrt()
}

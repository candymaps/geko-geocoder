//! `cargo run --release -p reverse-rstar --example bench -- <canonical dir> <urls file>`: builds
//! the reverse artifacts into a temporary directory and times `nearest` and `admin_at` for the
//! points of a file of reverse URLs (`lat=..&lon=..`), without HTTP.

use std::time::Instant;

use geko_core::geo::Coord;
use geko_core::ports::ReverseIndex;
use geko_core::query::KindFilter;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = tempfile::tempdir().unwrap();
    let country = geko_core::country::CountryCode::new("ES").unwrap();
    reverse_rstar::build(std::path::Path::new(&args[1]), country, dir.path()).expect("build");
    let index = reverse_rstar::RstarReverse::open(dir.path()).expect("open");
    let field = |line: &str, key: &str| -> f64 {
        let at = line.find(&format!("{key}=")).expect("field") + key.len() + 1;
        line[at..].split('&').next().unwrap().parse().unwrap()
    };
    let points: Vec<Coord> = std::fs::read_to_string(&args[2])
        .unwrap()
        .lines()
        .map(|l| Coord::from_degrees(field(l, "lat"), field(l, "lon")).unwrap())
        .collect();
    let mut hits = Vec::new();
    for (name, filter) in [("all", KindFilter::ALL), ("address", KindFilter::only(&[geko_core::model::PlaceKind::Address]))] {
        let rounds = 200;
        let started = Instant::now();
        for _ in 0..rounds {
            for p in &points {
                index.nearest(*p, filter, 5, &mut hits).unwrap();
                std::hint::black_box(&hits);
            }
        }
        let per = started.elapsed().as_nanos() as f64 / (rounds * points.len()) as f64;
        let started = Instant::now();
        for _ in 0..rounds {
            for p in &points {
                std::hint::black_box(index.admin_at(*p));
            }
        }
        let admin = started.elapsed().as_nanos() as f64 / (rounds * points.len()) as f64;
        println!("{name}: nearest {per:.0} ns/query, admin_at {admin:.0} ns/query");
    }
}

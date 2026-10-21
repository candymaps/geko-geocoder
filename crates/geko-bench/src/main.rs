//! The hot paths of the engine (PROJECT.md §13.3) over the reference queries of a bundle, for
//! `ci/test.sh bench`: each path runs in its own `bench_*` function, so that callgrind counts
//! the instructions of that function alone (`--toggle-collect=*bench_search*`). Instructions,
//! unlike time, are the same from one run to the next and from one machine to another, so they
//! can be compared on CI.
//!
//!     geko-bench <bundle> <cases.jsonl> <parse|search|autocomplete|reverse>

use std::hint::black_box;
use std::path::Path;

use anyhow::{Context, Result, bail};
use geko_core::bundle::BundleManifest;
use geko_core::geo::Coord;
use geko_core::ports::{AddressParser, GeocodeEngine, ReverseIndex, TextNormalizer};
use geko_core::query::{KindFilter, QueryContext};

#[derive(serde::Deserialize)]
struct Case {
    query: String,
    #[serde(default)]
    reverse: Option<String>,
    lat: f64,
    lon: f64,
}

/// The text rules of a country, from its pack in this repository.
fn normalizer(country: &str) -> Result<Box<dyn TextNormalizer>> {
    Ok(Box::new(pack(country)?.normalizer()))
}

fn parser(country: &str) -> Result<Box<dyn AddressParser>> {
    Ok(Box::new(pack(country)?.parser()))
}

fn pack(country: &str) -> Result<&'static geko_country_pack::CountryPack> {
    if geko_core::country::CountryCode::new(country).is_err() {
        bail!("`{country}` is not a country code");
    }
    Ok(geko_country_pack::repo(country))
}

#[inline(never)]
fn bench_parse(parser: &dyn AddressParser, texts: &[&str]) -> usize {
    texts.iter().map(|t| parser.parse(black_box(t)).map_or(0, |q| q.interpretations.len())).sum()
}

#[inline(never)]
fn bench_search(parser: &dyn AddressParser, engine: &dyn GeocodeEngine, texts: &[&str], prefix: bool) -> usize {
    let ctx = QueryContext::default();
    let mut out = Vec::new();
    let mut found = 0;
    for text in texts {
        let Ok(q) = parser.parse(black_box(text)) else { continue };
        out.clear();
        let _ = if prefix { engine.autocomplete(&q, &ctx, &mut out) } else { engine.search(&q, &ctx, &mut out) };
        found += out.len();
    }
    found
}

#[inline(never)]
fn bench_autocomplete(parser: &dyn AddressParser, engine: &dyn GeocodeEngine, texts: &[&str]) -> usize {
    bench_search(parser, engine, texts, true)
}

#[inline(never)]
fn bench_reverse(reverse: &dyn ReverseIndex, points: &[Coord]) -> usize {
    let mut out = Vec::new();
    let mut found = 0;
    for p in points {
        out.clear();
        let _ = reverse.nearest(black_box(*p), KindFilter::ALL, 10, &mut out);
        found += out.len() + usize::from(reverse.admin_at(*p).is_some());
    }
    found
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let [_, bundle, cases, bench] = &args[..] else { bail!("usage: geko-bench <bundle> <cases.jsonl> <parse|search|autocomplete|reverse>") };
    let bundle = Path::new(bundle);
    let manifest = BundleManifest::read(bundle)?;
    let geocoder = manifest.modules.iter().find(|m| m.name.as_str() == "geocoder").context("the bundle has no part of the geocoder")?;
    let meta = geko_core::segment::Meta::of(geocoder)?;
    let segment = meta.segments.first().context("the bundle has no segment")?;
    let dir = bundle.join(&segment.dir);
    let cases: Vec<Case> = std::fs::read_to_string(cases)?
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with("//"))
        .map(serde_json::from_str)
        .collect::<std::result::Result<_, _>>()?;
    let texts: Vec<&str> = cases.iter().filter(|c| c.reverse.is_none()).map(|c| c.query.as_str()).collect();
    let parser = parser(&segment.country)?;
    let found = match bench.as_str() {
        "parse" => bench_parse(parser.as_ref(), &texts),
        "search" | "autocomplete" => {
            let engine = geocode_fst::FstEngine::open(&dir.join("geocode"), normalizer(&segment.country)?, false)?;
            if bench == "search" {
                bench_search(parser.as_ref(), &engine, &texts, false)
            } else {
                bench_autocomplete(parser.as_ref(), &engine, &texts)
            }
        }
        "reverse" => {
            let reverse = reverse_rstar::RstarReverse::open(&dir.join("reverse"))?;
            let points: Vec<Coord> = cases.iter().filter_map(|c| Coord::from_degrees(c.lat, c.lon).ok()).collect();
            bench_reverse(&reverse, &points)
        }
        _ => bail!("unknown bench {bench}"),
    };
    println!("{bench}: {} cases, {found} results", cases.len());
    Ok(())
}

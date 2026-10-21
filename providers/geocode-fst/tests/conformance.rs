use geocode_fst::{FstEngine, FstIndexBuilder};
use geko_core::ports::GeocodeEngine;

#[test]
fn conformance() {
    let fixture = geko_conformance::fixture::canonical(&country_pack_es::normalizer());
    for preload in [false, true] {
        let open = |dir: &std::path::Path| -> geko_core::Result<Box<dyn GeocodeEngine>> {
            Ok(Box::new(FstEngine::open(dir, Box::new(country_pack_es::normalizer()), preload)?))
        };
        geko_conformance::geocode::check(&FstIndexBuilder, &open, &country_pack_es::normalizer(), &fixture);
    }
}

use geko_core::ports::ReverseIndex;
use reverse_rstar::RstarReverse;

#[test]
fn conformance() {
    let fixture = geko_conformance::fixture::canonical(&country_pack_es::normalizer());
    let open = |dir: &std::path::Path| -> geko_core::Result<Box<dyn ReverseIndex>> { Ok(Box::new(RstarReverse::open(dir)?)) };
    geko_conformance::reverse::check(&reverse_rstar::build, &open, &fixture);
}

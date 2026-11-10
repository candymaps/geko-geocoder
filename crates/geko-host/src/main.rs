//! geko with the geocoder linked in (`geko_app::main`), for the checks of this repository: the
//! golden, the instruction counts and all of Spain build and serve with it, as the host would
//! with the module it downloads, without a published module or a process in between.

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> anyhow::Result<()> {
    geko_app::main(vec![geko_app::Linked {
        name: "geocoder",
        open: geko_module_geocoder::open,
    }])
}

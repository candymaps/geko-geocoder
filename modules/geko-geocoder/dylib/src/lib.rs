//! The geocoder as a library that geko loads (docs/MODULES_PLAN.md, `DylibModule`).

use geko_module_sdk::Result;
use geko_module_sdk::module::{Context, Module};

// The library has an allocator of its own, apart from the host's: each side frees what it allocated.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn open(context: Context) -> Result<Box<dyn Module>> {
    // The library has its own copy of `tracing`, which the host's subscriber does not see.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_writer(std::io::stderr)
        .try_init();
    geko_module_geocoder::open(context)
}

geko_module_sdk::export_module!(open);

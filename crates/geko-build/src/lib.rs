pub mod fetch;
pub mod fusion;
pub mod purge;
pub mod selection;

/// The stamps of incremental generation (PROJECT.md §8.3), which every module builds with.
pub use geko_module_sdk::incremental;

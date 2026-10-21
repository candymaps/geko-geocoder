//! Own search engine (PROJECT.md §9): one segment per country with word, full-name and prefix
//! FSTs over posting lists of local ids in importance order, address blocks per street, and
//! hot and cold files kept apart.

mod builder;
mod canonical;
mod engine;
mod equivalences;
mod format;
mod store;

pub use builder::FstIndexBuilder;
pub use engine::FstEngine;
pub use format::{PROVIDER, VERSION};

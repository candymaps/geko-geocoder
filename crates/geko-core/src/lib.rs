//! The domain of the geocoder: its types and its ports (PROJECT.md §4). What it shares with the
//! host (errors, country codes, scope, bundle manifest, downloads) comes from the SDK of the
//! modules and is re-exported here, so that the geocoder names everything from one place.

pub use geko_module_sdk::{bundle, country, error, scope};

pub mod geo;
pub mod model;
pub mod ports;
pub mod query;
pub mod regions;
pub mod segment;

pub use error::{GekoError, Result};

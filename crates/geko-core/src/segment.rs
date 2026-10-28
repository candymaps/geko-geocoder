//! What the geocoder keeps in its entry of the bundle manifest (`BundleModule::meta`): a segment
//! per country, with the formats its indexes were written in, which the providers check at load.

use serde::{Deserialize, Serialize};

use crate::bundle::BundleModule;
use crate::{GekoError, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderFormat {
    pub provider: String,
    pub version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    pub country: String,
    /// Relative to the bundle root, e.g. `segments/es`.
    pub dir: String,
    pub geocode: ProviderFormat,
    pub reverse: ProviderFormat,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    pub segments: Vec<Segment>,
    /// The sources this part of the bundle was built from, since the manifest also carries the
    /// licenses of the other modules. A bundle without it lists every license as its own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
}

impl Meta {
    pub fn of(module: &BundleModule) -> Result<Self> {
        serde_json::from_value(module.meta.clone()).map_err(|e| GekoError::Data(format!("the geocoder part of the bundle: {e}")))
    }

    pub fn to_value(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("serializable")
    }
}

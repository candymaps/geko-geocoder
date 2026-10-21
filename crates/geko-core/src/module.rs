use std::fmt;
use std::str::FromStr;

use crate::{GekoError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ModuleName {
    Geocoder,
    Tiles,
}

impl ModuleName {
    pub const ALL: [ModuleName; 2] = [ModuleName::Geocoder, ModuleName::Tiles];

    pub fn as_str(self) -> &'static str {
        match self {
            ModuleName::Geocoder => "geocoder",
            ModuleName::Tiles => "tiles",
        }
    }
}

impl fmt::Display for ModuleName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ModuleName {
    type Err = GekoError;

    fn from_str(s: &str) -> Result<Self> {
        ModuleName::ALL
            .into_iter()
            .find(|m| m.as_str() == s.trim())
            .ok_or_else(|| GekoError::Config(format!("unknown module: {s:?}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_name_parsing() {
        assert_eq!(" tiles ".parse::<ModuleName>().unwrap(), ModuleName::Tiles);
        assert!("routing".parse::<ModuleName>().is_err());
    }
}

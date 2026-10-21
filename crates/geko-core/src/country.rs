use std::fmt;
use std::str::FromStr;

use crate::{GekoError, Result};

/// ISO 3166-1 alpha-2 code, stored uppercase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CountryCode([u8; 2]);

impl CountryCode {
    pub fn new(code: &str) -> Result<Self> {
        match code.as_bytes() {
            [a, b] if a.is_ascii_alphabetic() && b.is_ascii_alphabetic() => {
                Ok(Self([a.to_ascii_uppercase(), b.to_ascii_uppercase()]))
            }
            _ => Err(GekoError::InvalidInput(format!("invalid ISO 3166-1 alpha-2 code: {code:?}"))),
        }
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).expect("ASCII by construction")
    }

    pub fn lowercase(&self) -> String {
        self.as_str().to_ascii_lowercase()
    }
}

impl fmt::Display for CountryCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for CountryCode {
    type Err = GekoError;

    fn from_str(s: &str) -> Result<Self> {
        Self::new(s)
    }
}

/// ISO 3166-2 subdivision code, e.g. `ES-B`, stored uppercase.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RegionCode(String);

impl RegionCode {
    pub fn new(code: &str) -> Result<Self> {
        let invalid = || GekoError::InvalidInput(format!("invalid ISO 3166-2 code: {code:?}"));
        let (country, sub) = code.split_once('-').ok_or_else(invalid)?;
        CountryCode::new(country).map_err(|_| invalid())?;
        if sub.is_empty() || sub.len() > 3 || !sub.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(invalid());
        }
        Ok(Self(code.to_ascii_uppercase()))
    }

    pub fn country(&self) -> CountryCode {
        CountryCode::new(&self.0[..2]).expect("validated on construction")
    }

    pub fn subdivision(&self) -> &str {
        &self.0[3..]
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RegionCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for RegionCode {
    type Err = GekoError;

    fn from_str(s: &str) -> Result<Self> {
        Self::new(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn country_code_is_normalized_to_uppercase() {
        assert_eq!(CountryCode::new("es").unwrap().as_str(), "ES");
        assert_eq!(CountryCode::new("Es").unwrap().lowercase(), "es");
    }

    #[test]
    fn country_code_rejects_invalid_input() {
        for bad in ["", "E", "ESP", "E1", "ñ"] {
            assert!(CountryCode::new(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn region_code_parsing() {
        let r = RegionCode::new("es-b").unwrap();
        assert_eq!(r.as_str(), "ES-B");
        assert_eq!(r.country().as_str(), "ES");
        assert_eq!(r.subdivision(), "B");
        assert_eq!(RegionCode::new("FR-75").unwrap().subdivision(), "75");
        for bad in ["ES", "ES-", "ES-ABCD", "ESP-B", "ES-B!"] {
            assert!(RegionCode::new(bad).is_err(), "{bad}");
        }
    }
}

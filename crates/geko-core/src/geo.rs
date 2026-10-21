use crate::{GekoError, Result};

const EARTH_RADIUS_M: f64 = 6_371_008.8;
const SCALE: f64 = 1e7;

/// WGS84 coordinate in fixed point (1e-7 degrees, ~1 cm): 8 bytes per point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Coord {
    lat_e7: i32,
    lon_e7: i32,
}

impl Coord {
    pub fn from_degrees(lat: f64, lon: f64) -> Result<Self> {
        if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
            return Err(GekoError::InvalidInput(format!(
                "coordinates out of range: lat={lat}, lon={lon}"
            )));
        }
        Ok(Self { lat_e7: (lat * SCALE).round() as i32, lon_e7: (lon * SCALE).round() as i32 })
    }

    pub fn from_e7(lat_e7: i32, lon_e7: i32) -> Result<Self> {
        Self::from_degrees(lat_e7 as f64 / SCALE, lon_e7 as f64 / SCALE)
    }

    pub fn lat_e7(self) -> i32 {
        self.lat_e7
    }

    pub fn lon_e7(self) -> i32 {
        self.lon_e7
    }

    pub fn lat(self) -> f64 {
        self.lat_e7 as f64 / SCALE
    }

    pub fn lon(self) -> f64 {
        self.lon_e7 as f64 / SCALE
    }

    pub fn distance_m(self, other: Coord) -> f64 {
        let (lat1, lat2) = (self.lat().to_radians(), other.lat().to_radians());
        let dlat = lat2 - lat1;
        let dlon = (other.lon() - self.lon()).to_radians();
        let a = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
        2.0 * EARTH_RADIUS_M * a.sqrt().asin()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BBox {
    pub min: Coord,
    pub max: Coord,
}

impl BBox {
    pub fn new(min: Coord, max: Coord) -> Result<Self> {
        if min.lat_e7 > max.lat_e7 || min.lon_e7 > max.lon_e7 {
            return Err(GekoError::InvalidInput("bbox minimum is greater than maximum".into()));
        }
        Ok(Self { min, max })
    }

    pub fn contains(&self, p: Coord) -> bool {
        (self.min.lat_e7..=self.max.lat_e7).contains(&p.lat_e7)
            && (self.min.lon_e7..=self.max.lon_e7).contains(&p.lon_e7)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(lat: f64, lon: f64) -> Coord {
        Coord::from_degrees(lat, lon).unwrap()
    }

    #[test]
    fn rejects_out_of_range_coordinates() {
        assert!(Coord::from_degrees(91.0, 0.0).is_err());
        assert!(Coord::from_degrees(0.0, -181.0).is_err());
        assert!(Coord::from_e7(i32::MAX, 0).is_err());
    }

    #[test]
    fn fixed_point_round_trip_within_a_centimetre() {
        let p = c(41.387_412_345, 2.168_612_345);
        assert!((p.lat() - 41.387_412_345).abs() < 1e-7);
        assert!((p.lon() - 2.168_612_345).abs() < 1e-7);
        assert_eq!(p.lat_e7(), 413_874_123);
        assert_eq!(c(-90.0, -180.0).lon_e7(), -1_800_000_000);
    }

    #[test]
    fn distance_between_madrid_and_barcelona() {
        let km = c(40.4168, -3.7038).distance_m(c(41.3874, 2.1686)) / 1000.0;
        assert!((km - 505.0).abs() < 5.0, "{km}");
    }

    #[test]
    fn bbox_contains_point() {
        let bbox = BBox::new(c(41.3, 1.9), c(41.5, 2.3)).unwrap();
        assert!(bbox.contains(c(41.3874, 2.1686)));
        assert!(!bbox.contains(c(40.4168, -3.7038)));
        assert!(BBox::new(c(41.5, 2.3), c(41.3, 1.9)).is_err());
    }
}

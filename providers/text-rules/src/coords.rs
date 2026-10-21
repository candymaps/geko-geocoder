//! Coordinates typed as a query: decimal degrees (with a decimal point or, as usual in Spain,
//! a decimal comma), degrees-minutes-seconds, UTM (zone + latitude band) and full Plus Codes.

use geko_core::geo::Coord;

pub fn parse(text: &str) -> Option<Coord> {
    let t = text.trim();
    if t.contains('+')
        && let Some(c) = plus_code(t)
    {
        return Some(c);
    }
    if t.contains(['°', 'º']) || t.contains('\'') {
        return dms(t);
    }
    utm(t).or_else(|| decimal(t))
}

const OLC_DIGITS: &[u8] = b"23456789CFGHJMPQRVWX";

/// A full Open Location Code, "8FVC9G8F+6X": the centre of its area. Ten digits are five pairs
/// of latitude and longitude digits in base 20; each digit after them splits the area into a
/// 5×4 grid. Short codes ("9G8F+6X Madrid") need a reference place and are not read.
fn plus_code(t: &str) -> Option<Coord> {
    let code = t.to_ascii_uppercase();
    let (head, tail) = code.split_once('+')?;
    if head.len() != 8 || tail.len() == 1 {
        return None;
    }
    let digits_of = |s: &str| s.bytes().map(|b| OLC_DIGITS.iter().position(|&d| d == b)).collect::<Option<Vec<usize>>>();
    let unpadded = head.trim_end_matches('0');
    if unpadded.len() % 2 == 1 || unpadded.len() < 2 || (unpadded.len() < 8 && !tail.is_empty()) {
        return None;
    }
    let mut digits = digits_of(unpadded)?;
    digits.extend(digits_of(tail)?);
    if digits[0] >= 9 || digits[1] >= 18 {
        return None;
    }
    let (mut lat, mut lon, mut size) = (-90.0, -180.0, 20.0 * 20.0);
    for pair in digits[..digits.len().min(10)].chunks(2) {
        size /= 20.0;
        lat += pair[0] as f64 * size;
        lon += pair[1] as f64 * size;
    }
    let (mut lat_size, mut lon_size) = (size, size);
    for &d in digits.iter().skip(10) {
        lat_size /= 5.0;
        lon_size /= 4.0;
        lat += (d / 4) as f64 * lat_size;
        lon += (d % 4) as f64 * lon_size;
    }
    valid(lat + lat_size / 2.0, lon + lon_size / 2.0)
}

fn valid(lat: f64, lon: f64) -> Option<Coord> {
    Coord::from_degrees(lat, lon).ok()
}

fn decimal_number(s: &str) -> Option<f64> {
    let s = s.trim();
    if !s.contains(['.', ',']) {
        return None;
    }
    s.replace(',', ".").parse().ok()
}

/// "41.3874, 2.1686", "41.3874 2.1686", "41.3874;2.1686" and "41,3874 2,1686".
fn decimal(t: &str) -> Option<Coord> {
    let splits: [Vec<&str>; 3] = [
        t.split(';').collect(),
        t.split(", ").collect(),
        t.split_whitespace().filter(|s| *s != ",").collect(),
    ];
    let mut candidates: Vec<Vec<&str>> = splits.into_iter().collect();
    if t.matches(',').count() == 1 && !t.contains(' ') {
        candidates.push(t.split(',').collect());
    }
    for parts in candidates {
        let parts: Vec<&str> = parts.into_iter().map(|p| p.trim().trim_end_matches(',')).filter(|p| !p.is_empty()).collect();
        if let [a, b] = parts[..]
            && let (Some(lat), Some(lon)) = (decimal_number(a), decimal_number(b))
        {
            return valid(lat, lon);
        }
    }
    None
}

/// "40°25'0.6\"N 3°42'13\"W", "40º 25' N, 3º 42' O", "N 40° 25.5'". `O` is Spanish for west.
fn dms(t: &str) -> Option<Coord> {
    #[derive(Default)]
    struct Group {
        deg: Option<f64>,
        min: Option<f64>,
        sec: Option<f64>,
        hemisphere: Option<char>,
    }

    impl Group {
        fn value(&self) -> Option<f64> {
            let (m, s) = (self.min.unwrap_or(0.0), self.sec.unwrap_or(0.0));
            (m < 60.0 && s < 60.0).then(|| self.deg.unwrap_or(0.0) + m / 60.0 + s / 3600.0)
        }
    }

    let chars: Vec<char> = t.chars().collect();
    let mut groups: Vec<Group> = Vec::new();
    let mut current = Group::default();
    let mut prefix: Option<char> = None;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_digit() {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            let number: f64 = chars[start..i].iter().collect::<String>().parse().ok()?;
            let mut j = i;
            while chars.get(j) == Some(&' ') {
                j += 1;
            }
            match chars.get(j) {
                Some('°' | 'º') => {
                    if current.deg.is_some() {
                        groups.push(std::mem::take(&mut current));
                    }
                    current.deg = Some(number);
                    i = j + 1;
                }
                Some('\'' | '′') => {
                    current.min = Some(number);
                    i = j + 1;
                }
                Some('"' | '″') => {
                    current.sec = Some(number);
                    i = j + 1;
                }
                _ if current.deg.is_none() => current.deg = Some(number),
                _ if current.min.is_none() => current.min = Some(number),
                _ => current.sec = Some(number),
            }
            if current.hemisphere.is_none() {
                current.hemisphere = prefix.take();
            }
            continue;
        }
        let isolated = |k: usize| k == 0 || !chars[k - 1].is_alphabetic();
        let next_isolated = chars.get(i + 1).is_none_or(|n| !n.is_alphabetic());
        match c.to_ascii_uppercase() {
            h @ ('N' | 'S' | 'E' | 'W' | 'O') if isolated(i) && next_isolated => {
                if current.deg.is_some() && current.hemisphere.is_none() {
                    current.hemisphere = Some(h);
                    groups.push(std::mem::take(&mut current));
                } else {
                    if current.deg.is_some() {
                        groups.push(std::mem::take(&mut current));
                    }
                    prefix = Some(h);
                }
            }
            ',' | ';' if current.deg.is_some() => groups.push(std::mem::take(&mut current)),
            _ => {}
        }
        i += 1;
    }
    if current.deg.is_some() {
        groups.push(current);
    }
    let [a, b] = &groups[..] else { return None };
    let signed = |g: &Group| g.value().map(|v| if matches!(g.hemisphere, Some('S' | 'W' | 'O')) { -v } else { v });
    let (lat, lon) = match (a.hemisphere, b.hemisphere) {
        (Some('E' | 'W' | 'O'), _) | (_, Some('N' | 'S')) => (signed(b)?, signed(a)?),
        _ => (signed(a)?, signed(b)?),
    };
    valid(lat, lon)
}

const A: f64 = 6_378_137.0;
const F: f64 = 1.0 / 298.257_223_563;
const K0: f64 = 0.9996;

/// "30T 440291 4474254" or "30 T 440291 4474254" (ETRS89/WGS84).
fn utm(t: &str) -> Option<Coord> {
    let tokens: Vec<&str> = t.split(|c: char| c.is_whitespace() || c == ',').filter(|s| !s.is_empty()).collect();
    let (zone_band, easting, northing) = match tokens[..] {
        [zb, e, n] => (zb.to_string(), e, n),
        [z, b, e, n] if b.len() == 1 => (format!("{z}{b}"), e, n),
        _ => return None,
    };
    if !zone_band.is_ascii() || zone_band.len() < 2 {
        return None;
    }
    let band = zone_band.chars().last()?.to_ascii_uppercase();
    let zone: u32 = zone_band[..zone_band.len() - 1].parse().ok()?;
    if !(1..=60).contains(&zone) || !('C'..='X').contains(&band) || band == 'I' || band == 'O' {
        return None;
    }
    let easting: f64 = easting.parse().ok()?;
    let northing: f64 = northing.parse().ok()?;
    if !(100_000.0..=900_000.0).contains(&easting) || !(0.0..=10_000_000.0).contains(&northing) {
        return None;
    }
    let (lat, lon) = utm_to_latlon(zone, band >= 'N', easting, northing);
    valid(lat, lon)
}

pub fn utm_to_latlon(zone: u32, north: bool, easting: f64, northing: f64) -> (f64, f64) {
    let e2 = F * (2.0 - F);
    let ep2 = e2 / (1.0 - e2);
    let x = easting - 500_000.0;
    let y = if north { northing } else { northing - 10_000_000.0 };
    let m = y / K0;
    let mu = m / (A * (1.0 - e2 / 4.0 - 3.0 * e2 * e2 / 64.0 - 5.0 * e2.powi(3) / 256.0));
    let e1 = (1.0 - (1.0 - e2).sqrt()) / (1.0 + (1.0 - e2).sqrt());
    let phi1 = mu
        + (3.0 * e1 / 2.0 - 27.0 * e1.powi(3) / 32.0) * (2.0 * mu).sin()
        + (21.0 * e1 * e1 / 16.0 - 55.0 * e1.powi(4) / 32.0) * (4.0 * mu).sin()
        + (151.0 * e1.powi(3) / 96.0) * (6.0 * mu).sin()
        + (1097.0 * e1.powi(4) / 512.0) * (8.0 * mu).sin();
    let (sin1, cos1, tan1) = (phi1.sin(), phi1.cos(), phi1.tan());
    let n1 = A / (1.0 - e2 * sin1 * sin1).sqrt();
    let t1 = tan1 * tan1;
    let c1 = ep2 * cos1 * cos1;
    let r1 = A * (1.0 - e2) / (1.0 - e2 * sin1 * sin1).powf(1.5);
    let d = x / (n1 * K0);
    let lat = phi1
        - (n1 * tan1 / r1)
            * (d * d / 2.0 - (5.0 + 3.0 * t1 + 10.0 * c1 - 4.0 * c1 * c1 - 9.0 * ep2) * d.powi(4) / 24.0
                + (61.0 + 90.0 * t1 + 298.0 * c1 + 45.0 * t1 * t1 - 252.0 * ep2 - 3.0 * c1 * c1) * d.powi(6) / 720.0);
    let lon0 = (zone as f64 * 6.0 - 183.0).to_radians();
    let lon = lon0
        + (d - (1.0 + 2.0 * t1 + c1) * d.powi(3) / 6.0
            + (5.0 - 2.0 * c1 + 28.0 * t1 - 3.0 * c1 * c1 + 8.0 * ep2 + 24.0 * t1 * t1) * d.powi(5) / 120.0)
            / cos1;
    (lat.to_degrees(), lon.to_degrees())
}

#[cfg(test)]
pub fn latlon_to_utm(lat: f64, lon: f64, zone: u32) -> (f64, f64) {
    let e2 = F * (2.0 - F);
    let ep2 = e2 / (1.0 - e2);
    let (phi, lam) = (lat.to_radians(), lon.to_radians());
    let lam0 = (zone as f64 * 6.0 - 183.0).to_radians();
    let n = A / (1.0 - e2 * phi.sin().powi(2)).sqrt();
    let t = phi.tan().powi(2);
    let c = ep2 * phi.cos().powi(2);
    let a = phi.cos() * (lam - lam0);
    let m = A
        * ((1.0 - e2 / 4.0 - 3.0 * e2 * e2 / 64.0 - 5.0 * e2.powi(3) / 256.0) * phi
            - (3.0 * e2 / 8.0 + 3.0 * e2 * e2 / 32.0 + 45.0 * e2.powi(3) / 1024.0) * (2.0 * phi).sin()
            + (15.0 * e2 * e2 / 256.0 + 45.0 * e2.powi(3) / 1024.0) * (4.0 * phi).sin()
            - (35.0 * e2.powi(3) / 3072.0) * (6.0 * phi).sin());
    let easting = K0 * n * (a + (1.0 - t + c) * a.powi(3) / 6.0 + (5.0 - 18.0 * t + t * t + 72.0 * c - 58.0 * ep2) * a.powi(5) / 120.0)
        + 500_000.0;
    let northing = K0
        * (m + n * phi.tan()
            * (a * a / 2.0
                + (5.0 - t + 9.0 * c + 4.0 * c * c) * a.powi(4) / 24.0
                + (61.0 - 58.0 * t + t * t + 600.0 * c - 330.0 * ep2) * a.powi(6) / 720.0));
    (easting, northing)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plus_codes() {
        let near = |t: &str, lat: f64, lon: f64| {
            let c = parse(t).unwrap_or_else(|| panic!("{t} not read"));
            assert!((c.lat() - lat).abs() < 1e-4 && (c.lon() - lon).abs() < 1e-4, "{t}: {} {}", c.lat(), c.lon());
        };
        // Worked out by hand: 8F VC 9G 8F 6X are the latitude and longitude digits of five pairs.
        near("8FVC9G8F+6X", 47.3655625, 8.5249375);
        near("8fvc9g8f+6x", 47.3655625, 8.5249375);
        near("8FVC9G8F+6XQ", 47.3655875, 8.5249844);
        near("8FVC0000+", 47.5, 8.5);
        near("849VCWC8+R9", 37.421988, -122.084113);
        for bad in ["9G8F+6X", "8FVC9G8F+6", "8FVC9GA0+", "8FVC00+", "XFVC9G8F+6X", "1 + 2"] {
            assert!(parse(bad).is_none(), "{bad}");
        }
    }

    fn close(c: Option<Coord>, lat: f64, lon: f64, tol: f64) {
        let c = c.unwrap_or_else(|| panic!("no coordinates, expected {lat},{lon}"));
        assert!((c.lat() - lat).abs() < tol && (c.lon() - lon).abs() < tol, "{},{} vs {lat},{lon}", c.lat(), c.lon());
    }

    #[test]
    fn decimal_degrees() {
        close(parse("41.3874, 2.1686"), 41.3874, 2.1686, 1e-9);
        close(parse("41.3874 2.1686"), 41.3874, 2.1686, 1e-9);
        close(parse("41.3874;2.1686"), 41.3874, 2.1686, 1e-9);
        close(parse("41.3874,2.1686"), 41.3874, 2.1686, 1e-9);
        close(parse("41,3874 2,1686"), 41.3874, 2.1686, 1e-9);
        close(parse("+41.3874, +2.1686"), 41.3874, 2.1686, 1e-9);
        close(parse("40.4168, -3.7038"), 40.4168, -3.7038, 1e-9);
        assert!(parse("calle mayor 23").is_none());
        assert!(parse("23 45").is_none(), "two integers are not coordinates");
        assert!(parse("95.1, 2.0").is_none());
    }

    #[test]
    fn degrees_minutes_seconds() {
        close(parse("40°25'0.6\"N 3°42'13.7\"W"), 40.41683, -3.70381, 1e-4);
        close(parse("40º 25' N, 3º 42' O"), 40.41667, -3.7, 1e-4);
        close(parse("N 41° 23.244' E 2° 10.116'"), 41.3874, 2.1686, 1e-4);
        close(parse("3°42'W 40°25'N"), 40.41667, -3.7, 1e-4);
    }

    #[test]
    fn utm_round_trip_and_known_point() {
        let (e, n) = latlon_to_utm(40.4168, -3.7038, 30);
        close(parse(&format!("30T {e:.1} {n:.1}")), 40.4168, -3.7038, 1e-6);
        close(parse(&format!("30 T {e:.0} {n:.0}")), 40.4168, -3.7038, 1e-4);
        assert!((e - 440_300.0).abs() < 200.0 && (n - 4_474_250.0).abs() < 200.0, "{e} {n}");
        let (e, n) = latlon_to_utm(28.1235, -15.4363, 28);
        close(parse(&format!("28R {e:.1} {n:.1}")), 28.1235, -15.4363, 1e-6);
        assert!(parse("30T 12 34").is_none());
        assert!(parse("99T 440000 4474000").is_none());
    }
}

use std::fmt;

/// A geographic coordinate represented as latitude and longitude in decimal degrees.
///
/// This is the Haystack `Coord` scalar kind. Latitude must be in the range
/// −90 to 90 and longitude in −180 to 180. Displayed in Zinc as `C(lat,lng)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coord {
    /// Latitude in decimal degrees (−90 … 90).
    pub lat: f64,
    /// Longitude in decimal degrees (−180 … 180).
    pub lng: f64,
}

impl Coord {
    /// Creates a new `Coord` from the given latitude and longitude.
    ///
    /// Both values are stored as-is; callers are responsible for ensuring
    /// that `lat` is within −90..=90 and `lng` is within −180..=180.
    pub fn new(lat: f64, lng: f64) -> Self {
        Self { lat, lng }
    }
}

impl fmt::Display for Coord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "C({},{})", self.lat, self.lng)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coord_display() {
        let c = Coord::new(37.5458266, -77.4491888);
        assert_eq!(c.to_string(), "C(37.5458266,-77.4491888)");
    }

    #[test]
    fn coord_equality() {
        assert_eq!(Coord::new(37.0, -77.0), Coord::new(37.0, -77.0));
        assert_ne!(Coord::new(37.0, -77.0), Coord::new(37.0, -78.0));
    }

    #[test]
    fn coord_is_copy() {
        let c = Coord::new(1.0, 2.0);
        let c2 = c;
        assert_eq!(c, c2);
    }
}

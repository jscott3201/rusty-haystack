use std::fmt;

/// Geographic coordinate (latitude/longitude in decimal degrees).
/// Zinc: `C(37.55,-77.45)`
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coord {
    pub lat: f64,
    pub lng: f64,
}

impl Coord {
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

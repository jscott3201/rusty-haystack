use chrono::{DateTime, FixedOffset};
use std::fmt;
use std::hash::{Hash, Hasher};

/// Timezone-aware datetime with Haystack city-based timezone name.
///
/// Stores chrono DateTime<FixedOffset> plus the Haystack tz name
/// (e.g., "New_York", "London", "UTC").
///
/// Zinc: `2024-01-01T08:12:05-05:00 New_York`
#[derive(Debug, Clone)]
pub struct HDateTime {
    pub dt: DateTime<FixedOffset>,
    pub tz_name: String,
}

impl HDateTime {
    pub fn new(dt: DateTime<FixedOffset>, tz_name: impl Into<String>) -> Self {
        Self {
            dt,
            tz_name: tz_name.into(),
        }
    }
}

impl PartialEq for HDateTime {
    fn eq(&self, other: &Self) -> bool {
        self.dt == other.dt && self.tz_name == other.tz_name
    }
}

impl Eq for HDateTime {}

impl PartialOrd for HDateTime {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.dt.partial_cmp(&other.dt)
    }
}

impl Hash for HDateTime {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.dt.hash(state);
        self.tz_name.hash(state);
    }
}

impl fmt::Display for HDateTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Format: 2024-01-01T08:12:05-05:00 New_York
        write!(
            f,
            "{} {}",
            self.dt.format("%Y-%m-%dT%H:%M:%S%:z"),
            self.tz_name
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn hdatetime_display() {
        let offset = FixedOffset::west_opt(5 * 3600).unwrap();
        let dt = offset.with_ymd_and_hms(2024, 1, 1, 8, 12, 5).unwrap();
        let hdt = HDateTime::new(dt, "New_York");
        let s = hdt.to_string();
        assert!(s.contains("2024-01-01T08:12:05"));
        assert!(s.contains("New_York"));
    }

    #[test]
    fn hdatetime_equality() {
        let offset = FixedOffset::west_opt(5 * 3600).unwrap();
        let dt = offset.with_ymd_and_hms(2024, 1, 1, 8, 0, 0).unwrap();
        let a = HDateTime::new(dt, "New_York");
        let b = HDateTime::new(dt, "New_York");
        assert_eq!(a, b);
    }

    #[test]
    fn hdatetime_different_tz_name() {
        let offset = FixedOffset::west_opt(5 * 3600).unwrap();
        let dt = offset.with_ymd_and_hms(2024, 1, 1, 8, 0, 0).unwrap();
        let a = HDateTime::new(dt, "New_York");
        let b = HDateTime::new(dt, "Chicago");
        assert_ne!(a, b);
    }

    #[test]
    fn hdatetime_utc() {
        let offset = FixedOffset::east_opt(0).unwrap();
        let dt = offset.with_ymd_and_hms(2024, 6, 15, 12, 0, 0).unwrap();
        let hdt = HDateTime::new(dt, "UTC");
        let s = hdt.to_string();
        assert!(s.contains("+00:00"));
        assert!(s.contains("UTC"));
    }
}

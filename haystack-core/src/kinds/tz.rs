use std::collections::HashMap;
use std::sync::LazyLock;

#[cfg(feature = "chrono-tz")]
use chrono::{FixedOffset, LocalResult, NaiveDateTime, Offset, TimeZone};

static TZ_MAP: LazyLock<HashMap<String, String>> = LazyLock::new(build_tz_map);

fn build_tz_map() -> HashMap<String, String> {
    let mut map = HashMap::new();

    // Load from generated data file
    let data = include_str!("../../data/tz_map.txt");
    for line in data.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((city, iana)) = line.split_once('=') {
            map.insert(city.trim().to_string(), iana.trim().to_string());
        }
    }

    // Special Haystack mappings (inserted after file data to override)
    map.insert("UTC".to_string(), "UTC".to_string());
    map.insert("GMT".to_string(), "Etc/GMT".to_string());
    map.insert("Rel".to_string(), "UTC".to_string());

    map
}

/// Resolve a Haystack timezone name to an IANA identifier.
///
/// Tries city name lookup first (e.g., "New_York" -> "America/New_York"),
/// then checks if the input is already a valid IANA path (contains `/`).
pub fn tz_for(name: &str) -> Option<&'static str> {
    // City name lookup (most common case)
    if let Some(iana) = TZ_MAP.get(name) {
        return Some(iana.as_str());
    }
    // Check if it's already a full IANA path that's in our values
    if name.contains('/') {
        for v in TZ_MAP.values() {
            if v == name {
                return Some(v.as_str());
            }
        }
    }
    None
}

/// Get the full timezone map.
pub fn tz_map() -> &'static HashMap<String, String> {
    &TZ_MAP
}

/// Resolve a naive local date and time to the UTC offset(s) valid in a timezone.
///
/// The outer [`Option`] is `None` when `name` is not a known Haystack or IANA
/// timezone name. The [`LocalResult`] preserves daylight-saving transitions:
/// [`LocalResult::None`] denotes a skipped local time and
/// [`LocalResult::Ambiguous`] contains both offsets for a repeated local time.
#[cfg(feature = "chrono-tz")]
pub fn resolve_local_offset(name: &str, local: NaiveDateTime) -> Option<LocalResult<FixedOffset>> {
    let timezone = tz_for(name)?.parse::<chrono_tz::Tz>().ok()?;
    Some(
        timezone
            .from_local_datetime(&local)
            .map(|datetime| datetime.offset().fix()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "chrono-tz")]
    use chrono::{NaiveDate, TimeDelta};

    #[test]
    fn tz_map_loaded() {
        let map = tz_map();
        assert!(
            map.len() > 100,
            "expected 100+ timezone mappings, got {}",
            map.len()
        );
    }

    #[test]
    fn tz_utc() {
        assert_eq!(tz_for("UTC"), Some("UTC"));
    }

    #[test]
    fn tz_gmt() {
        assert_eq!(tz_for("GMT"), Some("Etc/GMT"));
    }

    #[test]
    fn tz_rel() {
        assert_eq!(tz_for("Rel"), Some("UTC"));
    }

    #[test]
    fn tz_new_york() {
        let result = tz_for("New_York");
        assert!(result.is_some(), "New_York should resolve");
        assert_eq!(result.unwrap(), "America/New_York");
    }

    #[test]
    fn tz_london() {
        let result = tz_for("London");
        assert!(result.is_some(), "London should resolve");
        assert_eq!(result.unwrap(), "Europe/London");
    }

    #[test]
    fn tz_unknown() {
        assert_eq!(tz_for("Nonexistent_City"), None);
    }

    #[test]
    fn tz_full_iana_path() {
        let result = tz_for("America/New_York");
        assert!(result.is_some(), "Full IANA path should resolve");
    }

    #[cfg(feature = "chrono-tz")]
    fn local_datetime(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(year, month, day)
            .unwrap()
            .and_hms_opt(hour, minute, 0)
            .unwrap()
    }

    #[cfg(feature = "chrono-tz")]
    #[test]
    fn resolves_new_york_summer_and_winter_offsets() {
        let winter = resolve_local_offset("New_York", local_datetime(2024, 1, 1, 12, 0));
        let summer = resolve_local_offset("New_York", local_datetime(2024, 7, 1, 12, 0));

        assert_eq!(
            winter,
            Some(LocalResult::Single(
                FixedOffset::west_opt(5 * 3600).unwrap()
            ))
        );
        assert_eq!(
            summer,
            Some(LocalResult::Single(
                FixedOffset::west_opt(4 * 3600).unwrap()
            ))
        );
    }

    #[cfg(feature = "chrono-tz")]
    #[test]
    fn resolves_non_dst_zone_consistently() {
        let winter = resolve_local_offset("Phoenix", local_datetime(2024, 1, 1, 12, 0));
        let summer = resolve_local_offset("Phoenix", local_datetime(2024, 7, 1, 12, 0));
        let expected = Some(LocalResult::Single(
            FixedOffset::west_opt(7 * 3600).unwrap(),
        ));

        assert_eq!(winter, expected);
        assert_eq!(summer, expected);
    }

    #[cfg(feature = "chrono-tz")]
    #[test]
    fn unknown_timezone_has_no_resolution() {
        assert_eq!(
            resolve_local_offset("Nonexistent_City", local_datetime(2024, 1, 1, 12, 0)),
            None
        );
    }

    #[cfg(feature = "chrono-tz")]
    #[test]
    fn spring_forward_local_time_does_not_exist() {
        assert_eq!(
            resolve_local_offset("New_York", local_datetime(2024, 3, 10, 2, 30)),
            Some(LocalResult::None)
        );
    }

    #[cfg(feature = "chrono-tz")]
    #[test]
    fn fall_back_local_time_has_both_offsets() {
        let result = resolve_local_offset("New_York", local_datetime(2024, 11, 3, 1, 30)).unwrap();
        let LocalResult::Ambiguous(first, second) = result else {
            panic!("expected repeated local time to have two offsets");
        };

        let difference = (first.local_minus_utc() - second.local_minus_utc()).abs();
        assert_eq!(difference, TimeDelta::hours(1).num_seconds() as i32);
        assert!([first, second].contains(&FixedOffset::west_opt(4 * 3600).unwrap()));
        assert!([first, second].contains(&FixedOffset::west_opt(5 * 3600).unwrap()));
    }
}

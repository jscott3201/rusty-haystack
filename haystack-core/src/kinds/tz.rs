use std::collections::HashMap;
use std::sync::LazyLock;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}

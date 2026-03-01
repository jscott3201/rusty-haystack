//! In-memory time-series store for historical data (hisRead / hisWrite).

use std::collections::HashMap;

use chrono::{DateTime, FixedOffset};
use parking_lot::RwLock;

use haystack_core::kinds::Kind;

const MAX_ITEMS_PER_SERIES: usize = 1_000_000;

/// A single historical data point: a timestamp and a value.
#[derive(Debug, Clone)]
pub struct HisItem {
    pub ts: DateTime<FixedOffset>,
    pub val: Kind,
}

/// Thread-safe in-memory time-series store.
///
/// Maps entity IDs to sorted vectors of `HisItem`, ordered by timestamp.
pub struct HisStore {
    items: RwLock<HashMap<String, Vec<HisItem>>>,
}

impl Default for HisStore {
    fn default() -> Self {
        Self {
            items: RwLock::new(HashMap::new()),
        }
    }
}

impl HisStore {
    /// Create a new empty history store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Write history items for a given point ID.
    ///
    /// Items are merged into the existing series and the result is kept
    /// sorted by timestamp. Duplicate timestamps are replaced.
    pub fn write(&self, id: &str, new_items: Vec<HisItem>) {
        let mut map = self.items.write();
        let series = map.entry(id.to_string()).or_default();

        for item in new_items {
            // Check if there is already an entry with this exact timestamp.
            match series.binary_search_by(|probe| probe.ts.cmp(&item.ts)) {
                Ok(pos) => {
                    // Replace existing entry at same timestamp.
                    series[pos] = item;
                }
                Err(pos) => {
                    // Insert at the correct sorted position.
                    series.insert(pos, item);
                }
            }
        }

        // Enforce per-series size cap by dropping oldest entries.
        if series.len() > MAX_ITEMS_PER_SERIES {
            let excess = series.len() - MAX_ITEMS_PER_SERIES;
            series.drain(..excess);
        }
    }

    /// Read history items for a point, optionally bounded by start/end.
    ///
    /// Both bounds are inclusive. If `start` is `None`, reads from the
    /// beginning. If `end` is `None`, reads to the end.
    pub fn read(
        &self,
        id: &str,
        start: Option<DateTime<FixedOffset>>,
        end: Option<DateTime<FixedOffset>>,
    ) -> Vec<HisItem> {
        let map = self.items.read();
        let series = match map.get(id) {
            Some(s) => s,
            None => return Vec::new(),
        };

        series
            .iter()
            .filter(|item| {
                if let Some(ref s) = start && item.ts < *s {
                    return false;
                }
                if let Some(ref e) = end && item.ts > *e {
                    return false;
                }
                true
            })
            .cloned()
            .collect()
    }

    /// Return the count of history items stored for a given point.
    pub fn len(&self, id: &str) -> usize {
        let map = self.items.read();
        map.get(id).map_or(0, |s| s.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use haystack_core::kinds::Number;

    /// Helper: build a DateTime<FixedOffset> at UTC for the given date/hour.
    fn utc_dt(year: i32, month: u32, day: u32, hour: u32) -> DateTime<FixedOffset> {
        let offset = FixedOffset::east_opt(0).unwrap();
        offset
            .with_ymd_and_hms(year, month, day, hour, 0, 0)
            .unwrap()
    }

    fn num_item(ts: DateTime<FixedOffset>, v: f64) -> HisItem {
        HisItem {
            ts,
            val: Kind::Number(Number::unitless(v)),
        }
    }

    #[test]
    fn write_and_read_back() {
        let store = HisStore::new();
        let ts1 = utc_dt(2024, 6, 1, 10);
        let ts2 = utc_dt(2024, 6, 1, 11);
        store.write("p1", vec![num_item(ts1, 72.0), num_item(ts2, 73.5)]);

        let items = store.read("p1", None, None);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].ts, ts1);
        assert_eq!(items[1].ts, ts2);
    }

    #[test]
    fn range_query() {
        let store = HisStore::new();
        let ts1 = utc_dt(2024, 6, 1, 8);
        let ts2 = utc_dt(2024, 6, 1, 10);
        let ts3 = utc_dt(2024, 6, 1, 12);
        let ts4 = utc_dt(2024, 6, 1, 14);
        store.write(
            "p1",
            vec![
                num_item(ts1, 70.0),
                num_item(ts2, 72.0),
                num_item(ts3, 74.0),
                num_item(ts4, 76.0),
            ],
        );

        // Query for items between 10:00 and 12:00 inclusive.
        let items = store.read("p1", Some(ts2), Some(ts3));
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].ts, ts2);
        assert_eq!(items[1].ts, ts3);
    }

    #[test]
    fn empty_read_returns_empty() {
        let store = HisStore::new();

        // Read from a point that was never written.
        let items = store.read("nonexistent", None, None);
        assert!(items.is_empty());
        assert_eq!(store.len("nonexistent"), 0);
    }

    #[test]
    fn multiple_points_are_independent() {
        let store = HisStore::new();
        let ts1 = utc_dt(2024, 1, 1, 0);
        let ts2 = utc_dt(2024, 1, 2, 0);

        store.write("temp", vec![num_item(ts1, 68.0)]);
        store.write("humidity", vec![num_item(ts2, 55.0)]);

        assert_eq!(store.len("temp"), 1);
        assert_eq!(store.len("humidity"), 1);

        let temp_items = store.read("temp", None, None);
        assert_eq!(temp_items.len(), 1);
        assert_eq!(temp_items[0].ts, ts1);

        let hum_items = store.read("humidity", None, None);
        assert_eq!(hum_items.len(), 1);
        assert_eq!(hum_items[0].ts, ts2);
    }

    #[test]
    fn sorted_order_maintained() {
        let store = HisStore::new();
        let ts1 = utc_dt(2024, 3, 1, 12);
        let ts2 = utc_dt(2024, 3, 1, 8);
        let ts3 = utc_dt(2024, 3, 1, 16);
        let ts4 = utc_dt(2024, 3, 1, 10);

        // Write out of order.
        store.write("p1", vec![num_item(ts1, 1.0), num_item(ts3, 3.0)]);
        store.write("p1", vec![num_item(ts2, 2.0), num_item(ts4, 4.0)]);

        let items = store.read("p1", None, None);
        assert_eq!(items.len(), 4);
        // Verify strictly ascending order.
        for window in items.windows(2) {
            assert!(window[0].ts < window[1].ts);
        }
        assert_eq!(items[0].ts, ts2); // 08:00
        assert_eq!(items[1].ts, ts4); // 10:00
        assert_eq!(items[2].ts, ts1); // 12:00
        assert_eq!(items[3].ts, ts3); // 16:00
    }

    #[test]
    fn duplicate_timestamp_replaces_value() {
        let store = HisStore::new();
        let ts = utc_dt(2024, 6, 1, 10);

        store.write("p1", vec![num_item(ts, 72.0)]);
        store.write("p1", vec![num_item(ts, 99.0)]);

        let items = store.read("p1", None, None);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].val, Kind::Number(Number::unitless(99.0)));
    }
}

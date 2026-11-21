use chrono::{Duration, FixedOffset, TimeZone};
use haystack_app::HisItem;
use haystack_core::data::HDict;
use haystack_core::kinds::{HDateTime, Kind, Number};

pub fn history_items(count: usize, start_minutes: i64) -> Vec<HisItem> {
    assert!(count <= 10_000, "benchmark history cap");
    let start = FixedOffset::east_opt(0)
        .unwrap()
        .with_ymd_and_hms(2024, 6, 1, 0, 0, 0)
        .unwrap()
        + Duration::minutes(start_minutes);
    (0..count)
        .map(|index| HisItem {
            ts: start + Duration::minutes(index as i64),
            val: Kind::Number(Number::unitless(70.0 + index as f64 * 0.25)),
        })
        .collect()
}

pub fn history_dicts(count: usize, start_minutes: i64) -> Vec<HDict> {
    history_items(count, start_minutes)
        .into_iter()
        .map(|item| {
            let mut row = HDict::new();
            row.set("ts", Kind::DateTime(HDateTime::new(item.ts, "UTC")));
            row.set("val", item.val);
            row
        })
        .collect()
}

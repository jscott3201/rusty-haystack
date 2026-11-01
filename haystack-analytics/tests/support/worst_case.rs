//! Worst-case header construction, shared by the capture tests and the
//! format unit tests. Kept apart from `support` so test targets that do not
//! build these headers (for example `tests/sql.rs`) do not compile them.
use super::support::*;
use haystack_core::{
    codecs::{Codec, history::*, zinc::ZincCodec},
    kinds::{HDateTime, Kind},
};

/// The request a `value` answers.
pub fn request_for(value: &HistoryReadResult) -> HistoryReadRequest {
    HistoryReadRequest {
        id: value.metadata.id.clone(),
        range: value.metadata.requested_range.clone(),
    }
}
/// The header a `value` is served with.
pub fn header(value: &HistoryReadResult) -> HistoryReadHeader {
    HistoryReadHeader {
        metadata: value.metadata.clone(),
        terminal: value.terminal,
        returned_count: value.samples.len() as u64,
    }
}
/// Bytes the manifest spends on `header`: the Zinc envelope as a JSON string,
/// so both escaping layers (control JSON inside a Zinc string, then that Zinc
/// text inside the manifest JSON) are counted.
pub fn header_json_bytes(header: &HistoryReadHeader) -> usize {
    let text = ZincCodec
        .encode_grid(&result_header_grid(header).unwrap())
        .unwrap();
    serde_json::to_string(&text).unwrap().len()
}
/// Every character `str::trim` strips. `validate_range` trims the requested
/// range, so a range may be padded with any of them up to its 1,024 bytes.
/// The HTTP client further refuses non-ASCII and raw CR/LF ranges before
/// dispatch, leaving tab, VT, FF and space for a real session.
pub fn trim_whitespace() -> Vec<char> {
    (0..=u32::from(char::MAX))
        .filter_map(char::from_u32)
        .filter(|c| c.is_whitespace())
        .collect()
}
/// Admitted `rows`-row results whose recorded header is as large as the
/// bounded-history codec allows (but for `returnedCount`, which is `rows`),
/// one per [`trim_whitespace`] padding character. Built by construction
/// rather than by example:
///
/// - the 256-byte id cap; the 1,024-byte range cap spent on padding around
///   `today` (the shortest range the codec resolves), any odd remainder
///   padded with VT, which escapes widest;
/// - the zone and unit whose recorded header is longest, chosen by measuring
///   every admitted candidate (each field is escaped independently, so the
///   choices are additive);
/// - every counter at `u64::MAX` and every bound at its codec cap;
/// - year-9999 timestamps with full nanoseconds, and retained coverage that
///   brackets the window so `retainedCount` is unconstrained.
pub fn worst_case_results(rows: usize) -> Vec<HistoryReadResult> {
    use chrono::{DateTime, FixedOffset, NaiveDate, TimeDelta};
    use haystack_core::kinds::{Number, offset_at, resolve_local_offset, tz_for, tz_map};
    const CORE: &str = "today";
    let at = |zone: &str, instant: DateTime<FixedOffset>| -> Option<HDateTime> {
        Some(HDateTime::new(
            instant.with_timezone(&offset_at(zone, instant)?),
            zone,
        ))
    };
    let build = |zone: &str, unit: &str, range: String| -> Option<HistoryReadResult> {
        let midnight = |day| {
            let local = NaiveDate::from_ymd_opt(9999, 12, day)?.and_hms_opt(0, 0, 0)?;
            let offset = resolve_local_offset(zone, local)?.single()?;
            local.and_local_timezone(offset).single()
        };
        let (start, end) = (midnight(30)?, midnight(31)?);
        let ns = TimeDelta::nanoseconds;
        let mut value = result(rows, 0);
        let m = &mut value.metadata;
        m.id = "p".repeat(256);
        m.requested_range = range;
        m.start = at(zone, start)?;
        m.end = at(zone, end)?;
        m.evaluated_at = at(zone, end - ns(1))?;
        m.schema.kind = HistoryKind::Number;
        m.schema.unit = Some(unit.into());
        m.schema.timezone = zone.into();
        m.bounds = HistoryBounds {
            batch_rows: MAX_ROWS as u64,
            batch_bytes: MAX_GRID_BYTES as u64,
            total_rows: MAX_ROWS as u64,
            total_bytes: MAX_GRID_BYTES as u64,
            total_work: 256 * 1024 * 1024,
            response_bytes: MAX_GRID_BYTES as u64,
        };
        m.graph.revision = u64::MAX;
        m.graph.catalog_generation = u64::MAX;
        m.history.generation = u64::MAX;
        m.coverage = HistoryCoverage {
            retained_start: Some(at(zone, start - ns(1))?),
            retained_end: Some(at(zone, end + ns(999_999_999))?),
            retained_count: u64::MAX,
            evicted_through: Some(at(zone, start - ns(2))?),
        };
        for (i, sample) in value.samples.iter_mut().enumerate() {
            sample.ts = at(zone, start + TimeDelta::seconds(i as i64))?;
            sample.val = Kind::Number(Number::new(f64::MAX, Some(unit.into())));
        }
        validate_for_request(&value, &request_for(&value)).ok()?;
        Some(value)
    };
    let longest = |candidates: Vec<String>, make: &dyn Fn(&str) -> Option<HistoryReadResult>| {
        candidates
            .iter()
            .filter_map(|c| Some((header_json_bytes(&header(&make(c)?)), c.clone())))
            .max()
            .unwrap()
            .1
    };
    let zones = tz_map()
        .keys()
        .filter(|z| z.len() <= 128 && tz_for(z).is_some())
        .cloned()
        .collect();
    let zone = longest(zones, &|z| build(z, "%", CORE.into()));
    let units = haystack_core::kinds::units_by_name()
        .keys()
        .chain(haystack_core::kinds::units_by_symbol().keys())
        .filter(|u| u.len() <= 128)
        .cloned()
        .collect();
    let unit = longest(units, &|u| build(&zone, u, CORE.into()));
    trim_whitespace()
        .into_iter()
        .map(|pad| {
            let room = 1024 - CORE.len();
            let mut range = pad.to_string().repeat(room / pad.len_utf8());
            range.push_str(CORE);
            range.push_str(&"\u{0b}".repeat(room % pad.len_utf8()));
            build(&zone, &unit, range).unwrap()
        })
        .collect()
}

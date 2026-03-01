//! The `hisRead` and `hisWrite` ops — historical time-series data.
//!
//! Backed by the in-memory `HisStore` stored in `AppState`.

use actix_web::{HttpRequest, HttpResponse, web};
use chrono::{DateTime, FixedOffset, Local, NaiveDate, NaiveTime, TimeZone};

use haystack_core::data::{HCol, HDict, HGrid};
use haystack_core::kinds::{HDateTime, HRef, Kind};

use crate::content;
use crate::error::HaystackError;
use crate::his_store::HisItem;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// hisRead
// ---------------------------------------------------------------------------

/// POST /api/hisRead
///
/// Request grid has one row with `id` (Ref) and `range` (Str) columns.
///
/// Supported `range` formats:
///   - `"today"` / `"yesterday"` — date ranges based on local time
///   - `"YYYY-MM-DD"` — a single date (midnight to midnight)
///   - `"YYYY-MM-DD,YYYY-MM-DD"` — explicit start,end dates (start inclusive, end exclusive midnight)
pub async fn handle_read(
    req: HttpRequest,
    body: String,
    state: web::Data<AppState>,
) -> Result<HttpResponse, HaystackError> {
    let content_type = req
        .headers()
        .get("Content-Type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let accept = req
        .headers()
        .get("Accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let request_grid = content::decode_request_grid(&body, content_type)
        .map_err(|e| HaystackError::bad_request(format!("failed to decode request: {e}")))?;

    // Extract the first row.
    let row = request_grid
        .row(0)
        .ok_or_else(|| HaystackError::bad_request("hisRead request has no rows"))?;

    // Extract id.
    let id = match row.get("id") {
        Some(Kind::Ref(r)) => r.val.clone(),
        _ => {
            return Err(HaystackError::bad_request(
                "hisRead: missing or invalid 'id' Ref",
            ));
        }
    };

    // Extract range string.
    let range_str = match row.get("range") {
        Some(Kind::Str(s)) => s.as_str(),
        _ => {
            return Err(HaystackError::bad_request(
                "hisRead: missing or invalid 'range' Str",
            ));
        }
    };

    // Parse range into (start, end) pair of DateTime<FixedOffset>.
    let (start, end) = parse_range(range_str)
        .map_err(|e| HaystackError::bad_request(format!("hisRead: bad range: {e}")))?;

    // Query the store.
    let items = state.his.read(&id, Some(start), Some(end));

    // Build response grid.
    let cols = vec![HCol::new("ts"), HCol::new("val")];
    let rows: Vec<HDict> = items
        .into_iter()
        .map(|item| {
            let mut d = HDict::new();
            d.set("ts", Kind::DateTime(HDateTime::new(item.ts, "UTC")));
            d.set("val", item.val);
            d
        })
        .collect();

    let mut meta = HDict::new();
    meta.set("id", Kind::Ref(HRef::from_val(&id)));
    let grid = HGrid::from_parts(meta, cols, rows);

    log::info!("hisRead: returning {} rows for point {}", grid.len(), id);
    let (encoded, ct) = content::encode_response_grid(&grid, accept)
        .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;

    Ok(HttpResponse::Ok().content_type(ct).body(encoded))
}

/// Parse a range string into a (start, end) pair of `DateTime<FixedOffset>`.
///
/// Supported formats:
///   - `"today"` — midnight-to-midnight of the current local date
///   - `"yesterday"` — midnight-to-midnight of yesterday's local date
///   - `"YYYY-MM-DD"` — a single date
///   - `"YYYY-MM-DD,YYYY-MM-DD"` — explicit start,end
fn parse_range(range: &str) -> Result<(DateTime<FixedOffset>, DateTime<FixedOffset>), String> {
    let range = range.trim();

    match range {
        "today" => {
            let today = Local::now().date_naive();
            Ok(date_range(today, today))
        }
        "yesterday" => {
            let yesterday = Local::now().date_naive() - chrono::Duration::days(1);
            Ok(date_range(yesterday, yesterday))
        }
        _ => {
            if range.contains(',') {
                let parts: Vec<&str> = range.splitn(2, ',').collect();
                let start_date = parse_date(parts[0].trim())?;
                let end_date = parse_date(parts[1].trim())?;
                Ok(date_range(start_date, end_date))
            } else {
                let date = parse_date(range)?;
                Ok(date_range(date, date))
            }
        }
    }
}

/// Parse a "YYYY-MM-DD" string into a NaiveDate.
fn parse_date(s: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|e| format!("invalid date '{s}': {e}"))
}

/// Build a (start, end) DateTime pair from date(s).
///
/// Start is midnight on `start_date`, end is 23:59:59 on `end_date`, both at UTC.
fn date_range(
    start_date: NaiveDate,
    end_date: NaiveDate,
) -> (DateTime<FixedOffset>, DateTime<FixedOffset>) {
    let utc = FixedOffset::east_opt(0).unwrap();
    let start = utc
        .from_local_datetime(&start_date.and_time(NaiveTime::MIN))
        .unwrap();
    let end = utc
        .from_local_datetime(&end_date.and_hms_opt(23, 59, 59).unwrap())
        .unwrap();
    (start, end)
}

// ---------------------------------------------------------------------------
// hisWrite
// ---------------------------------------------------------------------------

/// POST /api/hisWrite
///
/// Request grid meta must contain `id` (Ref). Rows contain `ts` and `val`
/// columns. Data is stored in the in-memory `HisStore`.
pub async fn handle_write(
    req: HttpRequest,
    body: String,
    state: web::Data<AppState>,
) -> Result<HttpResponse, HaystackError> {
    let content_type = req
        .headers()
        .get("Content-Type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let accept = req
        .headers()
        .get("Accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let request_grid = content::decode_request_grid(&body, content_type)
        .map_err(|e| HaystackError::bad_request(format!("failed to decode request: {e}")))?;

    // Extract point id from grid meta.
    let id = match request_grid.meta.get("id") {
        Some(Kind::Ref(r)) => r.val.clone(),
        _ => {
            return Err(HaystackError::bad_request(
                "hisWrite: grid meta must contain 'id' Ref",
            ));
        }
    };

    // Parse rows into HisItems.
    let mut items = Vec::with_capacity(request_grid.len());
    for (i, row) in request_grid.iter().enumerate() {
        let ts = match row.get("ts") {
            Some(Kind::DateTime(hdt)) => hdt.dt,
            _ => {
                return Err(HaystackError::bad_request(format!(
                    "hisWrite: row {i} missing or invalid 'ts' DateTime"
                )));
            }
        };
        let val = row.get("val").cloned().unwrap_or(Kind::Null);

        items.push(HisItem { ts, val });
    }

    let count = items.len();
    state.his.write(&id, items);

    log::info!("hisWrite: stored {} items for point {}", count, id);
    let grid = HGrid::new();
    let (encoded, ct) = content::encode_response_grid(&grid, accept)
        .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;

    Ok(HttpResponse::Ok().content_type(ct).body(encoded))
}

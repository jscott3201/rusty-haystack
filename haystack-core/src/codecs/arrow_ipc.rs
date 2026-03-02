//! Arrow IPC codec for converting HGrid ↔ Arrow RecordBatch.
//!
//! Provides standalone functions (not the `Codec` trait) because Arrow IPC
//! is a binary format while `Codec` returns `String`.

use arrow::array::*;
use arrow::datatypes::*;
use arrow::error::ArrowError;
use arrow::ipc::reader::StreamReader;
use arrow::ipc::writer::StreamWriter;
use arrow::record_batch::RecordBatch;
use std::sync::Arc;

use crate::data::{HCol, HDict, HGrid};
use crate::kinds::{Kind, Number};

/// Convert an HGrid to an Arrow RecordBatch.
pub fn grid_to_record_batch(grid: &HGrid) -> Result<RecordBatch, ArrowError> {
    if grid.cols.is_empty() || grid.rows.is_empty() {
        let schema = Schema::new(Vec::<Field>::new());
        return RecordBatch::try_new(Arc::new(schema), vec![]);
    }

    // Pass 1: determine Arrow type for each column by scanning rows
    let mut col_types: Vec<DataType> = Vec::new();
    let mut fields: Vec<Field> = Vec::new();

    for col in &grid.cols {
        let mut found_type = None;
        for row in &grid.rows {
            if let Some(kind) = row.get(&col.name) {
                if !matches!(kind, Kind::Null | Kind::NA | Kind::Remove) {
                    found_type = Some(kind_to_arrow_type(kind));
                    break;
                }
            }
        }
        let dt = found_type.unwrap_or(DataType::Utf8);

        let mut metadata = std::collections::HashMap::new();
        if dt == DataType::Float64 {
            for row in &grid.rows {
                if let Some(Kind::Number(n)) = row.get(&col.name) {
                    if let Some(ref unit) = n.unit {
                        metadata.insert("unit".to_string(), unit.clone());
                        break;
                    }
                }
            }
        }

        let field = if metadata.is_empty() {
            Field::new(&col.name, dt.clone(), true)
        } else {
            Field::new(&col.name, dt.clone(), true).with_metadata(metadata)
        };
        fields.push(field);
        col_types.push(dt);
    }

    let schema = Arc::new(Schema::new(fields));

    // Pass 2: build arrays
    let mut arrays: Vec<ArrayRef> = Vec::new();
    for (i, col) in grid.cols.iter().enumerate() {
        let array = build_array(&grid.rows, &col.name, &col_types[i]);
        arrays.push(array);
    }

    RecordBatch::try_new(schema, arrays)
}

fn kind_to_arrow_type(kind: &Kind) -> DataType {
    match kind {
        Kind::Bool(_) | Kind::Marker => DataType::Boolean,
        Kind::Number(_) => DataType::Float64,
        Kind::Str(_) | Kind::Ref(_) | Kind::Uri(_) | Kind::Symbol(_) | Kind::XStr(_) => {
            DataType::Utf8
        }
        Kind::Date(_) => DataType::Date32,
        Kind::Time(_) => DataType::Time64(TimeUnit::Nanosecond),
        Kind::DateTime(_) => DataType::Timestamp(TimeUnit::Millisecond, None),
        Kind::Coord(_) => DataType::Utf8,
        _ => DataType::Utf8,
    }
}

fn build_array(rows: &[HDict], col_name: &str, dt: &DataType) -> ArrayRef {
    match dt {
        DataType::Boolean => {
            let arr: BooleanArray = rows
                .iter()
                .map(|row| match row.get(col_name) {
                    Some(Kind::Bool(b)) => Some(*b),
                    Some(Kind::Marker) => Some(true),
                    _ => None,
                })
                .collect();
            Arc::new(arr)
        }
        DataType::Float64 => {
            let arr: Float64Array = rows
                .iter()
                .map(|row| match row.get(col_name) {
                    Some(Kind::Number(n)) => Some(n.val),
                    _ => None,
                })
                .collect();
            Arc::new(arr)
        }
        DataType::Date32 => {
            let arr: Date32Array = rows
                .iter()
                .map(|row| match row.get(col_name) {
                    Some(Kind::Date(d)) => {
                        let epoch = chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
                        Some((*d - epoch).num_days() as i32)
                    }
                    _ => None,
                })
                .collect();
            Arc::new(arr)
        }
        DataType::Time64(TimeUnit::Nanosecond) => {
            let arr: Time64NanosecondArray = rows
                .iter()
                .map(|row| match row.get(col_name) {
                    Some(Kind::Time(t)) => t
                        .signed_duration_since(chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap())
                        .num_nanoseconds(),
                    _ => None,
                })
                .collect();
            Arc::new(arr)
        }
        DataType::Timestamp(TimeUnit::Millisecond, _) => {
            let arr: TimestampMillisecondArray = rows
                .iter()
                .map(|row| match row.get(col_name) {
                    Some(Kind::DateTime(dt)) => Some(dt.dt.timestamp_millis()),
                    _ => None,
                })
                .collect();
            Arc::new(arr)
        }
        // Utf8 handles Str, Ref, Uri, Symbol, Coord, XStr, and fallbacks
        _ => {
            let arr: StringArray = rows
                .iter()
                .map(|row| match row.get(col_name) {
                    Some(Kind::Str(s)) => Some(s.clone()),
                    Some(Kind::Ref(r)) => Some(r.val.clone()),
                    Some(Kind::Uri(u)) => Some(u.0.clone()),
                    Some(Kind::Symbol(s)) => Some(s.0.clone()),
                    Some(Kind::Coord(c)) => Some(format!("{},{}", c.lat, c.lng)),
                    Some(Kind::XStr(x)) => Some(format!("{}:{}", x.type_name, x.val)),
                    Some(Kind::Null | Kind::NA | Kind::Remove) | None => None,
                    Some(other) => Some(format!("{other}")),
                })
                .collect();
            Arc::new(arr)
        }
    }
}

/// Convert an Arrow RecordBatch back to an HGrid.
pub fn record_batch_to_grid(batch: &RecordBatch) -> Result<HGrid, ArrowError> {
    let schema = batch.schema();
    let cols: Vec<HCol> = schema
        .fields()
        .iter()
        .map(|f| {
            let mut meta = HDict::new();
            if let Some(unit) = f.metadata().get("unit") {
                meta.set("unit".to_string(), Kind::Str(unit.clone()));
            }
            HCol {
                name: f.name().clone(),
                meta,
            }
        })
        .collect();

    let mut rows = Vec::new();
    for row_idx in 0..batch.num_rows() {
        let mut dict = HDict::new();
        for (col_idx, col) in cols.iter().enumerate() {
            let array = batch.column(col_idx);
            if !array.is_null(row_idx) {
                if let Some(kind) = arrow_value_to_kind(array, row_idx, array.data_type()) {
                    dict.set(col.name.clone(), kind);
                }
            }
        }
        rows.push(dict);
    }

    Ok(HGrid::from_parts(HDict::new(), cols, rows))
}

fn arrow_value_to_kind(array: &dyn Array, idx: usize, dt: &DataType) -> Option<Kind> {
    match dt {
        DataType::Boolean => {
            let arr = array.as_any().downcast_ref::<BooleanArray>()?;
            Some(Kind::Bool(arr.value(idx)))
        }
        DataType::Float64 => {
            let arr = array.as_any().downcast_ref::<Float64Array>()?;
            Some(Kind::Number(Number::unitless(arr.value(idx))))
        }
        DataType::Utf8 => {
            let arr = array.as_any().downcast_ref::<StringArray>()?;
            Some(Kind::Str(arr.value(idx).to_string()))
        }
        DataType::Date32 => {
            let arr = array.as_any().downcast_ref::<Date32Array>()?;
            let epoch = chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
            let date = epoch + chrono::Duration::days(arr.value(idx) as i64);
            Some(Kind::Date(date))
        }
        DataType::Time64(TimeUnit::Nanosecond) => {
            let arr = array.as_any().downcast_ref::<Time64NanosecondArray>()?;
            let nanos = arr.value(idx);
            let secs = (nanos / 1_000_000_000) as u32;
            let nano_rem = (nanos % 1_000_000_000) as u32;
            let time = chrono::NaiveTime::from_num_seconds_from_midnight_opt(secs, nano_rem)?;
            Some(Kind::Time(time))
        }
        DataType::Timestamp(TimeUnit::Millisecond, _) => {
            let arr = array.as_any().downcast_ref::<TimestampMillisecondArray>()?;
            let millis = arr.value(idx);
            let utc_dt = chrono::DateTime::from_timestamp_millis(millis)?;
            let fixed = utc_dt.with_timezone(&chrono::FixedOffset::east_opt(0).unwrap());
            Some(Kind::DateTime(crate::kinds::HDateTime::new(fixed, "UTC")))
        }
        _ => {
            // Fallback: try as string
            let arr = array.as_any().downcast_ref::<StringArray>()?;
            Some(Kind::Str(arr.value(idx).to_string()))
        }
    }
}

/// Encode a grid to Arrow IPC streaming bytes.
///
/// An empty grid (no columns and no rows) produces a zero-length byte vector.
pub fn grid_to_ipc_bytes(grid: &HGrid) -> Result<Vec<u8>, ArrowError> {
    if grid.cols.is_empty() && grid.rows.is_empty() {
        return Ok(Vec::new());
    }
    let batch = grid_to_record_batch(grid)?;
    let mut buf = Vec::new();
    {
        let mut writer = StreamWriter::try_new(&mut buf, &batch.schema())?;
        writer.write(&batch)?;
        writer.finish()?;
    }
    Ok(buf)
}

/// Decode a grid from Arrow IPC streaming bytes.
///
/// An empty byte slice produces an empty HGrid.
pub fn ipc_bytes_to_grid(bytes: &[u8]) -> Result<HGrid, ArrowError> {
    if bytes.is_empty() {
        return Ok(HGrid::new());
    }
    let cursor = std::io::Cursor::new(bytes);
    let reader = StreamReader::try_new(cursor, None)?;
    let batches: Result<Vec<_>, _> = reader.collect();
    let batches = batches?;
    if batches.is_empty() {
        return Ok(HGrid::new());
    }
    record_batch_to_grid(&batches[0])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kinds::{Coord, HRef};

    #[test]
    fn round_trip_empty_grid() {
        let grid = HGrid::new();
        let bytes = grid_to_ipc_bytes(&grid).unwrap();
        let result = ipc_bytes_to_grid(&bytes).unwrap();
        assert!(result.is_empty());
        assert_eq!(result.num_cols(), 0);
    }

    #[test]
    fn round_trip_bool_number_string() {
        let cols = vec![HCol::new("flag"), HCol::new("val"), HCol::new("name")];
        let mut row1 = HDict::new();
        row1.set("flag", Kind::Bool(true));
        row1.set("val", Kind::Number(Number::unitless(42.0)));
        row1.set("name", Kind::Str("alpha".into()));

        let mut row2 = HDict::new();
        row2.set("flag", Kind::Bool(false));
        row2.set("val", Kind::Number(Number::unitless(99.5)));
        row2.set("name", Kind::Str("beta".into()));

        let grid = HGrid::from_parts(HDict::new(), cols, vec![row1, row2]);
        let batch = grid_to_record_batch(&grid).unwrap();
        let result = record_batch_to_grid(&batch).unwrap();

        assert_eq!(result.len(), 2);
        assert_eq!(result.num_cols(), 3);
        assert_eq!(result.rows[0].get("flag"), Some(&Kind::Bool(true)));
        assert_eq!(
            result.rows[0].get("val"),
            Some(&Kind::Number(Number::unitless(42.0)))
        );
        assert_eq!(result.rows[0].get("name"), Some(&Kind::Str("alpha".into())));
        assert_eq!(result.rows[1].get("flag"), Some(&Kind::Bool(false)));
    }

    #[test]
    fn round_trip_date_columns() {
        let cols = vec![HCol::new("date")];
        let d = chrono::NaiveDate::from_ymd_opt(2024, 6, 15).unwrap();
        let mut row = HDict::new();
        row.set("date", Kind::Date(d));

        let grid = HGrid::from_parts(HDict::new(), cols, vec![row]);
        let batch = grid_to_record_batch(&grid).unwrap();
        let result = record_batch_to_grid(&batch).unwrap();

        assert_eq!(result.rows[0].get("date"), Some(&Kind::Date(d)));
    }

    #[test]
    fn unit_metadata_preserved() {
        let cols = vec![HCol::new("temp")];
        let mut row = HDict::new();
        row.set("temp", Kind::Number(Number::new(72.5, Some("°F".into()))));

        let grid = HGrid::from_parts(HDict::new(), cols, vec![row]);
        let batch = grid_to_record_batch(&grid).unwrap();

        let schema = batch.schema();
        let field = schema.field(0);
        assert_eq!(field.metadata().get("unit"), Some(&"°F".to_string()));
    }

    #[test]
    fn round_trip_ipc_bytes() {
        let cols = vec![HCol::new("dis"), HCol::new("area")];
        let mut row = HDict::new();
        row.set("dis", Kind::Str("Building A".into()));
        row.set("area", Kind::Number(Number::unitless(5000.0)));

        let grid = HGrid::from_parts(HDict::new(), cols, vec![row]);
        let bytes = grid_to_ipc_bytes(&grid).unwrap();
        assert!(!bytes.is_empty());

        let result = ipc_bytes_to_grid(&bytes).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(
            result.rows[0].get("dis"),
            Some(&Kind::Str("Building A".into()))
        );
        assert_eq!(
            result.rows[0].get("area"),
            Some(&Kind::Number(Number::unitless(5000.0)))
        );
    }

    #[test]
    fn preserves_column_and_row_count() {
        let cols = vec![HCol::new("a"), HCol::new("b"), HCol::new("c")];
        let mut r1 = HDict::new();
        r1.set("a", Kind::Str("1".into()));
        r1.set("b", Kind::Str("2".into()));
        r1.set("c", Kind::Str("3".into()));
        let mut r2 = HDict::new();
        r2.set("a", Kind::Str("4".into()));
        r2.set("b", Kind::Str("5".into()));
        r2.set("c", Kind::Str("6".into()));
        let mut r3 = HDict::new();
        r3.set("a", Kind::Str("7".into()));
        r3.set("b", Kind::Str("8".into()));
        r3.set("c", Kind::Str("9".into()));

        let grid = HGrid::from_parts(HDict::new(), cols, vec![r1, r2, r3]);
        let batch = grid_to_record_batch(&grid).unwrap();

        assert_eq!(batch.num_columns(), 3);
        assert_eq!(batch.num_rows(), 3);
    }

    #[test]
    fn marker_columns_become_boolean() {
        let cols = vec![HCol::new("site"), HCol::new("dis")];
        let mut row = HDict::new();
        row.set("site", Kind::Marker);
        row.set("dis", Kind::Str("My Site".into()));

        let grid = HGrid::from_parts(HDict::new(), cols, vec![row]);
        let batch = grid_to_record_batch(&grid).unwrap();

        assert_eq!(*batch.schema().field(0).data_type(), DataType::Boolean);
        let arr = batch
            .column(0)
            .as_any()
            .downcast_ref::<BooleanArray>()
            .unwrap();
        assert_eq!(arr.value(0), true);
    }

    #[test]
    fn null_values_handled() {
        let cols = vec![HCol::new("name"), HCol::new("val")];
        let mut row1 = HDict::new();
        row1.set("name", Kind::Str("present".into()));
        // val is missing in row1

        let mut row2 = HDict::new();
        // name is missing in row2
        row2.set("val", Kind::Number(Number::unitless(10.0)));

        let grid = HGrid::from_parts(HDict::new(), cols, vec![row1, row2]);
        let batch = grid_to_record_batch(&grid).unwrap();

        // val column: row 0 should be null
        assert!(batch.column(1).is_null(0));
        assert!(!batch.column(1).is_null(1));

        // name column: row 1 should be null
        assert!(!batch.column(0).is_null(0));
        assert!(batch.column(0).is_null(1));
    }

    #[test]
    fn ref_and_uri_as_utf8() {
        let cols = vec![HCol::new("id"), HCol::new("link")];
        let mut row = HDict::new();
        row.set("id", Kind::Ref(HRef::from_val("site-1")));
        row.set(
            "link",
            Kind::Uri(crate::kinds::Uri::new("http://example.com")),
        );

        let grid = HGrid::from_parts(HDict::new(), cols, vec![row]);
        let batch = grid_to_record_batch(&grid).unwrap();

        assert_eq!(*batch.schema().field(0).data_type(), DataType::Utf8);
        assert_eq!(*batch.schema().field(1).data_type(), DataType::Utf8);

        let result = record_batch_to_grid(&batch).unwrap();
        // Refs/Uris become Str on round-trip (type info lost in Arrow Utf8)
        assert_eq!(result.rows[0].get("id"), Some(&Kind::Str("site-1".into())));
        assert_eq!(
            result.rows[0].get("link"),
            Some(&Kind::Str("http://example.com".into()))
        );
    }

    #[test]
    fn coord_serialized_as_string() {
        let cols = vec![HCol::new("loc")];
        let mut row = HDict::new();
        row.set("loc", Kind::Coord(Coord::new(37.55, -77.45)));

        let grid = HGrid::from_parts(HDict::new(), cols, vec![row]);
        let batch = grid_to_record_batch(&grid).unwrap();

        let arr = batch
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(arr.value(0), "37.55,-77.45");
    }
}

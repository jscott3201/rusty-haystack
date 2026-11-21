//! SQL-facing schema, sample conversion and the sealed read-only providers.
use super::SnapshotIdentity;
use crate::{Dataset, Error, Result};
use arrow::{
    array::{
        ArrayRef, BooleanArray, Float64Array, Int32Array, Int64Array, RecordBatch, StringArray,
        TimestampNanosecondArray, UInt32Array, UInt64Array,
    },
    datatypes::{DataType, Field, Schema, SchemaRef, TimeUnit},
};
use async_trait::async_trait;
use datafusion::{
    catalog::{Session, TableProvider},
    datasource::{TableType, memory::MemorySourceConfig},
    logical_expr::Expr,
    physical_plan::ExecutionPlan,
};
use haystack_core::{
    codecs::history::{HistoryKind, HistoryReadHeader, HistorySample},
    kinds::Kind,
};
use std::{
    fmt,
    sync::{Arc, LazyLock},
};

const NANOS_PER_SECOND: i64 = 1_000_000_000;

static SAMPLE_SCHEMA: LazyLock<SchemaRef> = LazyLock::new(|| {
    Arc::new(Schema::new(vec![
        Field::new("point_id", DataType::Utf8, false),
        Field::new(
            "ts",
            DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())),
            false,
        ),
        Field::new("utc_seconds", DataType::Int64, false),
        Field::new("nanoseconds", DataType::UInt32, false),
        Field::new("offset_seconds", DataType::Int32, false),
        Field::new(
            "ts_local",
            DataType::Timestamp(TimeUnit::Nanosecond, None),
            false,
        ),
        Field::new("tz", DataType::Utf8, false),
        Field::new("present", DataType::Boolean, false),
        Field::new("bool_value", DataType::Boolean, true),
        Field::new("number_value", DataType::Float64, true),
        Field::new("string_value", DataType::Utf8, true),
        Field::new("unit", DataType::Utf8, true),
    ]))
});

static PROVENANCE_SCHEMA: LazyLock<SchemaRef> = LazyLock::new(|| {
    Arc::new(Schema::new(vec![
        Field::new("table_name", DataType::Utf8, false),
        Field::new("export_id", DataType::Utf8, false),
        Field::new("manifest_sha256", DataType::Utf8, false),
        Field::new("point_id", DataType::Utf8, false),
        Field::new("kind", DataType::Utf8, false),
        Field::new("timezone", DataType::Utf8, false),
        Field::new("point_unit", DataType::Utf8, true),
        Field::new("requested_range", DataType::Utf8, false),
        Field::new("rows", DataType::UInt64, false),
        Field::new("history_authority", DataType::Utf8, false),
        Field::new("history_incarnation", DataType::Utf8, false),
        Field::new("history_generation", DataType::UInt64, false),
    ]))
});

/// Schema of every snapshot table. It carries no metadata, so snapshots of
/// different points can be combined with `UNION ALL`.
pub(crate) fn sample_schema() -> SchemaRef {
    SAMPLE_SCHEMA.clone()
}

/// `utc_seconds * 1e9 + nanoseconds`. A leap-second fraction
/// (`nanoseconds >= 1e9`) therefore lands in the following second, exactly
/// as the components say; a value outside `i64` nanoseconds (about years
/// 1677 to 2262) is `Error::Limit`.
pub(crate) fn epoch_nanos(seconds: i64, nanos: u32) -> Result<i64> {
    seconds
        .checked_mul(NANOS_PER_SECOND)
        .and_then(|v| v.checked_add(i64::from(nanos)))
        .ok_or(Error::Limit)
}

/// SQL-facing batch of validated samples. NA is `present = false` with every
/// value column and `unit` NULL; a present value fills only its kind's column.
pub(crate) fn sample_batch(
    header: &HistoryReadHeader,
    samples: &[HistorySample],
) -> Result<RecordBatch> {
    let mut ts = Vec::with_capacity(samples.len());
    let mut local = Vec::with_capacity(samples.len());
    for sample in samples {
        // Validated samples carry only NA or their schema kind's value.
        if !matches!(
            (&sample.val, header.metadata.schema.kind),
            (Kind::NA, _)
                | (Kind::Bool(_), HistoryKind::Bool)
                | (Kind::Number(_), HistoryKind::Number)
                | (Kind::Str(_), HistoryKind::Str)
        ) {
            return Err(Error::Invalid);
        }
        let utc = epoch_nanos(
            sample.ts.dt.timestamp(),
            sample.ts.dt.timestamp_subsec_nanos(),
        )?;
        let offset = i64::from(sample.ts.dt.offset().local_minus_utc()) * NANOS_PER_SECOND;
        ts.push(utc);
        local.push(utc.checked_add(offset).ok_or(Error::Limit)?);
    }
    let id = header.metadata.id.as_str();
    let columns: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from_iter_values(samples.iter().map(|_| id))),
        Arc::new(TimestampNanosecondArray::from(ts).with_timezone("UTC")),
        Arc::new(Int64Array::from_iter_values(
            samples.iter().map(|s| s.ts.dt.timestamp()),
        )),
        Arc::new(UInt32Array::from_iter_values(
            samples.iter().map(|s| s.ts.dt.timestamp_subsec_nanos()),
        )),
        Arc::new(Int32Array::from_iter_values(
            samples.iter().map(|s| s.ts.dt.offset().local_minus_utc()),
        )),
        Arc::new(TimestampNanosecondArray::from(local)),
        Arc::new(StringArray::from_iter_values(
            samples.iter().map(|s| s.ts.tz_name.as_str()),
        )),
        Arc::new(BooleanArray::from_iter(
            samples.iter().map(|s| Some(!matches!(s.val, Kind::NA))),
        )),
        Arc::new(BooleanArray::from_iter(samples.iter().map(
            |s| match s.val {
                Kind::Bool(v) => Some(v),
                _ => None,
            },
        ))),
        Arc::new(Float64Array::from_iter(samples.iter().map(
            |s| match &s.val {
                Kind::Number(n) => Some(n.val),
                _ => None,
            },
        ))),
        Arc::new(StringArray::from_iter(samples.iter().map(
            |s| match &s.val {
                Kind::Str(v) => Some(v.as_str()),
                _ => None,
            },
        ))),
        Arc::new(StringArray::from_iter(samples.iter().map(
            |s| match &s.val {
                Kind::Number(n) => n.unit.as_deref(),
                _ => None,
            },
        ))),
    ];
    Ok(RecordBatch::try_new(sample_schema(), columns)?)
}

/// One sealed snapshot: validated batches in memory, served read-only.
pub(crate) struct Snapshot {
    pub(crate) identity: SnapshotIdentity,
    pub(crate) header: HistoryReadHeader,
    pub(crate) rows: u64,
    pub(crate) bytes: usize,
    batches: Vec<RecordBatch>,
}
impl fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Snapshot")
            .field("identity", &self.identity)
            .field("rows", &self.rows)
            .finish_non_exhaustive()
    }
}
impl Snapshot {
    /// Drains `dataset` completely and seals it. Any reader error, a reader
    /// that did not reach semantic completion, or sealed memory over
    /// `budget` fails the whole registration.
    pub(crate) fn load(table: String, mut dataset: Dataset, budget: usize) -> Result<Self> {
        let header = dataset.header().clone();
        let (mut batches, mut rows, mut bytes) = (Vec::new(), 0u64, 0usize);
        while let Some(samples) = dataset.next_batch()? {
            if samples.is_empty() {
                continue;
            }
            let batch = sample_batch(&header, &samples)?;
            rows += batch.num_rows() as u64;
            bytes = bytes
                .checked_add(batch.get_array_memory_size())
                .ok_or(Error::Limit)?;
            if bytes > budget {
                return Err(Error::Limit);
            }
            batches.push(batch);
        }
        if !dataset.semantically_complete() || rows != dataset.manifest().rows {
            return Err(Error::Invalid);
        }
        Ok(Self {
            identity: SnapshotIdentity {
                table,
                export_id: dataset.manifest().export_id.clone(),
                manifest_sha256: dataset.manifest_sha256().to_owned(),
            },
            header,
            rows,
            bytes,
            batches,
        })
    }
}
#[async_trait]
impl TableProvider for Snapshot {
    fn schema(&self) -> SchemaRef {
        sample_schema()
    }
    fn table_type(&self) -> TableType {
        TableType::Base
    }
    async fn scan(
        &self,
        _state: &dyn Session,
        projection: Option<&Vec<usize>>,
        _filters: &[Expr],
        _limit: Option<usize>,
    ) -> datafusion::error::Result<Arc<dyn ExecutionPlan>> {
        let exec = MemorySourceConfig::try_new_exec(
            std::slice::from_ref(&self.batches),
            sample_schema(),
            projection.cloned(),
        )?;
        Ok(exec)
    }
}

/// The `snapshots.kind` spelling, independent of `HistoryKind`'s `Debug`.
fn kind_name(kind: HistoryKind) -> &'static str {
    match kind {
        HistoryKind::Bool => "Bool",
        HistoryKind::Number => "Number",
        HistoryKind::Str => "Str",
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The read-only `snapshots` table: one row of provenance per snapshot.
pub(crate) struct Provenance {
    batch: RecordBatch,
}
impl fmt::Debug for Provenance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Provenance")
            .field("rows", &self.batch.num_rows())
            .finish()
    }
}
impl Provenance {
    pub(crate) fn new(snapshots: &[Arc<Snapshot>]) -> Result<Self> {
        let text = |f: fn(&Snapshot) -> String| -> ArrayRef {
            Arc::new(StringArray::from_iter_values(
                snapshots.iter().map(|s| f(s)),
            ))
        };
        let number = |f: fn(&Snapshot) -> u64| -> ArrayRef {
            Arc::new(UInt64Array::from_iter_values(
                snapshots.iter().map(|s| f(s)),
            ))
        };
        let columns: Vec<ArrayRef> = vec![
            text(|s| s.identity.table.clone()),
            text(|s| s.identity.export_id.clone()),
            text(|s| s.identity.manifest_sha256.clone()),
            text(|s| s.header.metadata.id.clone()),
            text(|s| kind_name(s.header.metadata.schema.kind).to_owned()),
            text(|s| s.header.metadata.schema.timezone.clone()),
            Arc::new(StringArray::from_iter(
                snapshots
                    .iter()
                    .map(|s| s.header.metadata.schema.unit.clone()),
            )),
            text(|s| s.header.metadata.requested_range.clone()),
            number(|s| s.rows),
            text(|s| hex(&s.header.metadata.history.authority)),
            text(|s| hex(&s.header.metadata.history.incarnation)),
            number(|s| s.header.metadata.history.generation),
        ];
        Ok(Self {
            batch: RecordBatch::try_new(PROVENANCE_SCHEMA.clone(), columns)?,
        })
    }
}
#[async_trait]
impl TableProvider for Provenance {
    fn schema(&self) -> SchemaRef {
        PROVENANCE_SCHEMA.clone()
    }
    fn table_type(&self) -> TableType {
        TableType::Base
    }
    async fn scan(
        &self,
        _state: &dyn Session,
        projection: Option<&Vec<usize>>,
        _filters: &[Expr],
        _limit: Option<usize>,
    ) -> datafusion::error::Result<Arc<dyn ExecutionPlan>> {
        let exec = MemorySourceConfig::try_new_exec(
            &[vec![self.batch.clone()]],
            PROVENANCE_SCHEMA.clone(),
            projection.cloned(),
        )?;
        Ok(exec)
    }
}

//! Bounded, immutable local captures of one remote `bounded-history-v1` session.
//!
//! [`Export`] owns source cleanup and blocking filesystem work, including when
//! a wait is abandoned. [`Dataset`] reads committed manifests only. Opening
//! verifies integrity; semantic completeness is reported only after every row
//! has been consumed successfully. See `docs/parquet-history.md` in the repository
//! for the format, limits and filesystem qualification boundary.
mod checkpoint;
mod export;
mod format;
mod reader;
mod writer;

pub use export::{
    DEFAULT_EXPORT_TIMEOUT, Export, ExportOptions, ExportReceipt, ExportStats,
    MAX_CONCURRENT_EXPORTS,
};
pub use format::{Limits, MANIFEST_NAME, Manifest, Part};
pub use reader::Dataset;

/// Diagnostics never include source URLs, credentials or remote text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("invalid capture options")]
    Options,
    #[error("no Tokio runtime is available to own the export")]
    Runtime,
    #[error("capture capacity exhausted")]
    Capacity,
    #[error("capture cancelled")]
    Cancelled,
    #[error("capture deadline expired")]
    Deadline,
    #[error("capture limit exceeded")]
    Limit,
    #[error("invalid or incompatible dataset")]
    Invalid,
    #[error("source did not complete: {0:?}")]
    Source(haystack_client::history::HistoryStreamEnd),
    #[error("filesystem operation failed: {0:?}")]
    Io(std::io::ErrorKind),
    #[error("Parquet operation failed")]
    Parquet,
    #[error("owned capture task failed")]
    Worker,
    #[error("manifest was published, but durability acknowledgment is uncertain: {export_id}")]
    PublishedUncertain { export_id: String },
}
impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value.kind())
    }
}
impl From<parquet::errors::ParquetError> for Error {
    fn from(_: parquet::errors::ParquetError) -> Self {
        Self::Parquet
    }
}
impl From<arrow_schema::ArrowError> for Error {
    fn from(_: arrow_schema::ArrowError) -> Self {
        Self::Invalid
    }
}
type Result<T> = std::result::Result<T, Error>;

/// Backstop for upstream decoder panics on hostile input. The contract is the
/// preflight guards (`format::header_preflight`, `reader::footer_guard`,
/// `reader::page_guard`), which reject such input before upstream sees it;
/// this only converts a panic they did not anticipate into `Error::Invalid`.
/// The process panic hook still runs, so such a panic remains visible.
/// It does nothing under `panic = "abort"` and cannot catch an allocation
/// failure, so the guards must bound every count upstream allocates from.
pub(crate) fn contain<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    #[cfg(test)]
    if tests::BACKSTOP_DISABLED.with(std::cell::Cell::get) {
        return f();
    }
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or(Err(Error::Invalid))
}

#[cfg(test)]
mod tests;

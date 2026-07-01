//! Pluggable history storage backend.
//!
//! The default implementation is [`HisStore`](crate::his_store::HisStore) (in-memory).
//! Implement this trait to back hisRead/hisWrite with a database.

use std::future::Future;
use std::pin::Pin;

use chrono::{DateTime, FixedOffset};

use crate::his_store::HisItem;

/// Trait for history storage backends.
pub trait HistoryProvider: Send + Sync + 'static {
    /// Read historical items for an entity within the optional time range.
    fn his_read(
        &self,
        id: &str,
        start: Option<DateTime<FixedOffset>>,
        end: Option<DateTime<FixedOffset>>,
    ) -> Pin<Box<dyn Future<Output = Vec<HisItem>> + Send + '_>>;

    /// Write historical items for an entity.
    fn his_write(
        &self,
        id: &str,
        items: Vec<HisItem>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

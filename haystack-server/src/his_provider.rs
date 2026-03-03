//! Pluggable history storage backend.
//!
//! The default implementation is [`HisStore`](crate::his_store::HisStore) (in-memory).
//! Implement this trait to back hisRead/hisWrite with a database.

use async_trait::async_trait;
use chrono::{DateTime, FixedOffset};

use crate::his_store::HisItem;

/// Trait for history storage backends.
#[async_trait]
pub trait HistoryProvider: Send + Sync + 'static {
    /// Read historical items for an entity within the optional time range.
    async fn his_read(
        &self,
        id: &str,
        start: Option<DateTime<FixedOffset>>,
        end: Option<DateTime<FixedOffset>>,
    ) -> Vec<HisItem>;

    /// Write historical items for an entity.
    async fn his_write(&self, id: &str, items: Vec<HisItem>);
}

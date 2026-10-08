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
    /// Called once when this provider is transferred to an owned legacy HTTP
    /// application. Borrowed providers and disabled history profiles skip it.
    /// Store partial acquisitions in self or cancellation-safe RAII state.
    fn initialize(&self) -> haystack_app::ResourceFuture<'_> {
        Box::pin(async { Ok(()) })
    }

    /// Release partial acquisitions after a failed or cancelled initialize.
    /// Override together with initialize when it can acquire external resources.
    fn rollback_initialize(&self) -> haystack_app::ResourceFuture<'_> {
        Box::pin(async { Ok(()) })
    }

    /// Close after successful initialization and actual owned work completion.
    /// This runs independently of Arc lifetime; retained adapter state must not
    /// keep an owned provider operating after this hook completes.
    fn close(&self) -> haystack_app::ResourceFuture<'_> {
        Box::pin(async { Ok(()) })
    }

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

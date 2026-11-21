//! Bounded provider interface. A provider must not detach untracked work from
//! open/pull/close futures. The application retains and awaits those futures
//! after cancellation; returning is a completion receipt for that operation.
use crate::{CancellationToken, ResourceFuture};
use chrono::{DateTime, FixedOffset};
use haystack_core::{
    codecs::history::{HistoryCapabilities, HistoryState, HistoryTerminal},
    kinds::Kind,
};
use std::{future::Future, pin::Pin, time::Instant};

#[derive(Debug, Clone)]
pub struct HisItem {
    pub ts: DateTime<FixedOffset>,
    pub val: Kind,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HistoryProviderError {
    #[error("history generation changed")]
    Changed,
    #[error("history provider stopped")]
    Stopped,
    #[error("history provider budget exhausted")]
    Limit,
    #[error("history provider failed")]
    Failed,
    #[error("history generation exhausted")]
    Exhausted,
    #[error("history writes are unsupported")]
    Unsupported,
}
pub type HistoryFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, HistoryProviderError>> + Send + 'a>>;
#[derive(Clone)]
pub struct HistoryPullBudget {
    pub max_rows: usize,
    pub max_bytes: usize,
    pub max_work: usize,
    pub deadline: Instant,
    pub cancellation: CancellationToken,
}
impl HistoryPullBudget {
    pub fn check(&self) -> Result<(), HistoryProviderError> {
        if self.cancellation.is_cancelled() || Instant::now() >= self.deadline {
            Err(HistoryProviderError::Stopped)
        } else {
            Ok(())
        }
    }
}
#[derive(Debug, Clone)]
pub struct ProviderCoverage {
    pub retained_start: Option<DateTime<FixedOffset>>,
    pub retained_end: Option<DateTime<FixedOffset>>,
    pub retained_count: u64,
    pub evicted_through: Option<DateTime<FixedOffset>>,
}
#[derive(Debug, Clone)]
pub struct ProviderHistoryMetadata {
    pub state: HistoryState,
    pub coverage: ProviderCoverage,
    pub capabilities: HistoryCapabilities,
}
#[derive(Debug)]
pub struct ProviderHistoryBatch {
    pub items: Vec<HisItem>,
    pub terminal: Option<HistoryTerminal>,
}
pub trait HistorySession: Send + 'static {
    fn metadata(&self) -> &ProviderHistoryMetadata;
    fn pull(&mut self, budget: HistoryPullBudget) -> HistoryFuture<'_, ProviderHistoryBatch>;
    fn close(&mut self) -> HistoryFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
pub trait HistoryProvider: Send + Sync + 'static {
    fn initialize(&self) -> ResourceFuture<'_> {
        Box::pin(async { Ok(()) })
    }
    fn rollback_initialize(&self) -> ResourceFuture<'_> {
        Box::pin(async { Ok(()) })
    }
    fn close(&self) -> ResourceFuture<'_> {
        Box::pin(async { Ok(()) })
    }
    fn open(
        &self,
        id: String,
        start: DateTime<FixedOffset>,
        end: DateTime<FixedOffset>,
        budget: HistoryPullBudget,
    ) -> HistoryFuture<'_, Box<dyn HistorySession>>;
    /// Explicit scoped write opt-in, binding the same series/receipt authority
    /// used by reads and every native writer. No capability means read-only.
    fn history_write_capability(&self) -> Option<crate::HistoryWriteCapability> {
        None
    }
    /// A provider may delay/retain this opaque plan, publish it at most once, or
    /// reject before effects. Detached work must retain the plan's work lease.
    fn commit_history(
        &self,
        prepared: crate::PreparedHistoryMutation,
    ) -> crate::HistoryWriteOutcome {
        prepared.reject(crate::HistoryWriteRejection::Unsupported)
    }
    /// Trusted native compatibility. Implementations must advance the same
    /// authority/generation advertised by history_write_capability.
    fn his_write(&self, _id: &str, _items: Vec<HisItem>) -> HistoryFuture<'_, ()> {
        Box::pin(async { Err(HistoryProviderError::Unsupported) })
    }
}

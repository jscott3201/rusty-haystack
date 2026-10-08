//! One in-memory history authority. Publication lock order is policy -> graph
//! read -> authority -> point. Native writes/reset use authority -> point only;
//! read sessions retain a point handle and never acquire authority while held.
use crate::history_mutation::{HistoryBinding, HistoryRecord};
use crate::history_provider::*;
use chrono::{DateTime, FixedOffset};
use haystack_core::{
    codecs::{
        history::{HistoryCapabilities, HistoryReason, HistoryState, HistoryTerminal},
        history_mutation::HistoryOperationIdentity,
    },
    kinds::Kind,
};
use parking_lot::{Mutex, RwLock};
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::Duration,
};

const MAX_ITEMS_PER_SERIES: usize = 1_000_000;
#[derive(Debug, Clone)]
pub struct HistoryStoreLimits {
    pub max_items_per_point: usize,
    pub max_points: usize,
    pub receipt_capacity: usize,
    pub receipt_bytes: usize,
    pub change_capacity: usize,
}
impl Default for HistoryStoreLimits {
    fn default() -> Self {
        Self {
            max_items_per_point: MAX_ITEMS_PER_SERIES,
            max_points: 4096,
            receipt_capacity: 1024,
            receipt_bytes: 64 * 1024 * 1024,
            change_capacity: 4096,
        }
    }
}
#[derive(Clone)]
pub(crate) struct Series {
    pub(crate) incarnation: [u8; 16],
    pub(crate) generation: u64,
    pub(crate) items: Vec<HisItem>,
    pub(crate) evicted: Option<DateTime<FixedOffset>>,
}
impl Default for Series {
    fn default() -> Self {
        Self {
            incarnation: rand::random(),
            generation: 0,
            items: vec![],
            evicted: None,
        }
    }
}
/// Required bounded in-process publication record. This is not a public stream
/// protocol, nor a promise to retain all historical values or records forever.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryChangeRecord {
    pub sequence: u64,
    pub point: String,
    pub before: HistoryState,
    pub after: HistoryState,
    pub submitted_samples: u64,
    pub unique_samples: u64,
    pub retained_samples: u64,
    pub evicted_samples: u64,
    pub evicted_through: Option<DateTime<FixedOffset>>,
    /// None denotes a trusted native write or explicit point reset.
    pub operation: Option<HistoryOperationIdentity>,
}
pub(crate) struct AuthorityState {
    pub(crate) points: HashMap<String, Arc<RwLock<Series>>>,
    pub(crate) receipts: HashMap<HistoryBinding, HistoryRecord>,
    pub(crate) receipt_bytes: usize,
    pub(crate) changes: VecDeque<HistoryChangeRecord>,
    pub(crate) sequence: u64,
}
pub(crate) struct HistoryStoreInner {
    pub(crate) authority: [u8; 16],
    pub(crate) limits: HistoryStoreLimits,
    pub(crate) state: Mutex<AuthorityState>,
}
/// Retain this handle to preserve series, incarnation/generation and receipts
/// together. A fresh store has a fresh authority; none of this survives a crash.
#[derive(Clone)]
pub struct HisStore {
    pub(crate) inner: Arc<HistoryStoreInner>,
}
impl Default for HisStore {
    fn default() -> Self {
        Self::with_limits(HistoryStoreLimits::default()).expect("valid memory history defaults")
    }
}
impl HisStore {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_retention(max_items_per_point: usize) -> Result<Self, HistoryProviderError> {
        Self::with_limits(HistoryStoreLimits {
            max_items_per_point,
            ..HistoryStoreLimits::default()
        })
    }
    pub fn with_limits(limits: HistoryStoreLimits) -> Result<Self, HistoryProviderError> {
        if limits.max_items_per_point == 0
            || limits.max_items_per_point > MAX_ITEMS_PER_SERIES
            || limits.max_points == 0
            || limits.max_points > 100_000
            || limits.receipt_capacity == 0
            || limits.receipt_capacity > 100_000
            || limits.receipt_bytes == 0
            || limits.receipt_bytes > 256 * 1024 * 1024
            || limits.change_capacity == 0
            || limits.change_capacity > 100_000
        {
            return Err(HistoryProviderError::Limit);
        }
        let mut changes = VecDeque::new();
        // Reserving the finite ledger once makes publication allocation-free.
        changes
            .try_reserve_exact(limits.change_capacity)
            .map_err(|_| HistoryProviderError::Limit)?;
        Ok(Self {
            inner: Arc::new(HistoryStoreInner {
                authority: rand::random(),
                limits,
                state: Mutex::new(AuthorityState {
                    points: HashMap::new(),
                    receipts: HashMap::new(),
                    receipt_bytes: 0,
                    changes,
                    sequence: 0,
                }),
            }),
        })
    }
    pub fn authority(&self) -> [u8; 16] {
        self.inner.authority
    }
    pub fn shares_authority(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
    pub fn receipt_count(&self) -> usize {
        self.inner.state.lock().receipts.len()
    }
    pub fn point_count(&self) -> usize {
        self.inner.state.lock().points.len()
    }
    /// Trusted in-process fixture/inspection view of the bounded change ledger.
    pub fn retained_changes(&self) -> Vec<HistoryChangeRecord> {
        self.inner.state.lock().changes.iter().cloned().collect()
    }
    pub(crate) fn point_in(
        &self,
        state: &mut AuthorityState,
        id: &str,
    ) -> Result<Arc<RwLock<Series>>, HistoryProviderError> {
        if id.is_empty() || id.len() > 256 {
            return Err(HistoryProviderError::Limit);
        }
        if let Some(point) = state.points.get(id) {
            return Ok(point.clone());
        }
        if state.points.len() >= self.inner.limits.max_points {
            return Err(HistoryProviderError::Limit);
        }
        state
            .points
            .try_reserve(1)
            .map_err(|_| HistoryProviderError::Limit)?;
        let point = Arc::new(RwLock::new(Series::default()));
        state.points.insert(id.to_owned(), point.clone());
        Ok(point)
    }
    /// Trusted observation, allocating at most one bounded empty point identity.
    pub fn state(&self, id: &str) -> Result<HistoryState, HistoryProviderError> {
        let mut state = self.inner.state.lock();
        let point = self.point_in(&mut state, id)?;
        let series = point.read();
        Ok(self.series_state(&series))
    }
    pub(crate) fn series_state(&self, series: &Series) -> HistoryState {
        HistoryState {
            authority: self.authority(),
            incarnation: series.incarnation,
            generation: series.generation,
        }
    }
    pub(crate) fn append_change(&self, state: &mut AuthorityState, change: HistoryChangeRecord) {
        if state.changes.len() == self.inner.limits.change_capacity {
            state.changes.pop_front();
        }
        state.sequence = change.sequence;
        state.changes.push_back(change);
    }
    /// Explicit trusted history reset, independent of graph replacement. The
    /// same point handle receives a newly minted incarnation; old plans/sessions
    /// cannot revive previous state. Recognized receipts remain in this authority.
    pub fn reset_point(&self, id: &str) -> Result<HistoryState, HistoryProviderError> {
        let mut state = self.inner.state.lock();
        let sequence = state
            .sequence
            .checked_add(1)
            .ok_or(HistoryProviderError::Exhausted)?;
        let point = self.point_in(&mut state, id)?;
        let mut series = point.write();
        let before = self.series_state(&series);
        let next = Series::default();
        let after = self.series_state(&next);
        let change = HistoryChangeRecord {
            sequence,
            point: id.to_owned(),
            before,
            after,
            submitted_samples: 0,
            unique_samples: 0,
            retained_samples: 0,
            evicted_samples: series.items.len() as u64,
            evicted_through: series.items.last().map(|item| item.ts),
            operation: None,
        };
        *series = next;
        self.append_change(&mut state, change);
        Ok(after)
    }
    /// Trusted native write. A nonempty write advances the same generation and
    /// change ledger once. Empty native writes have no effect or allocation.
    /// This compatibility API does not perform scoped schema/policy admission.
    pub fn write(&self, id: &str, items: Vec<HisItem>) -> Result<(), HistoryProviderError> {
        if items.is_empty() {
            return Ok(());
        }
        let mut state = self.inner.state.lock();
        let sequence = state
            .sequence
            .checked_add(1)
            .ok_or(HistoryProviderError::Exhausted)?;
        let point = self.point_in(&mut state, id)?;
        let mut series = point.write();
        let before = self.series_state(&series);
        let generation = series
            .generation
            .checked_add(1)
            .ok_or(HistoryProviderError::Exhausted)?;
        let submitted_samples = items.len() as u64;
        let mut instants = items.iter().map(|item| item.ts).collect::<Vec<_>>();
        instants.sort_unstable();
        instants.dedup();
        // Native compatibility keeps the existing allocation behavior: reserve
        // before effects, then move incoming values under the publication locks.
        // No full series clone is needed for this trusted writer.
        series
            .items
            .try_reserve(items.len())
            .map_err(|_| HistoryProviderError::Limit)?;
        let point_id = id.to_owned();
        for item in items {
            match series
                .items
                .binary_search_by(|existing| existing.ts.cmp(&item.ts))
            {
                Ok(index) => series.items[index] = item,
                Err(index) => series.items.insert(index, item),
            }
        }
        let evicted_samples = trim(&mut series, self.inner.limits.max_items_per_point);
        series.generation = generation;
        let change = HistoryChangeRecord {
            sequence,
            point: point_id,
            before,
            after: self.series_state(&series),
            submitted_samples,
            unique_samples: instants.len() as u64,
            retained_samples: series.items.len() as u64,
            evicted_samples: evicted_samples as u64,
            evicted_through: series.evicted,
            operation: None,
        };
        self.append_change(&mut state, change);
        Ok(())
    }
    /// Unbounded trusted compatibility read; legacy end is inclusive.
    pub fn read(
        &self,
        id: &str,
        start: Option<DateTime<FixedOffset>>,
        end: Option<DateTime<FixedOffset>>,
    ) -> Vec<HisItem> {
        let point = self.inner.state.lock().points.get(id).cloned();
        let Some(point) = point else {
            return vec![];
        };
        point
            .read()
            .items
            .iter()
            .filter(|item| start.is_none_or(|s| item.ts >= s) && end.is_none_or(|e| item.ts <= e))
            .cloned()
            .collect()
    }
    pub fn len(&self, id: &str) -> usize {
        let point = self.inner.state.lock().points.get(id).cloned();
        point.map_or(0, |point| point.read().items.len())
    }
    pub fn is_empty(&self, id: &str) -> bool {
        self.len(id) == 0
    }
}
pub(crate) fn trim(series: &mut Series, capacity: usize) -> usize {
    let excess = series.items.len().saturating_sub(capacity);
    if excess > 0 {
        let through = series.items[excess - 1].ts;
        series.evicted = Some(series.evicted.map_or(through, |old| old.max(through)));
        series.items.drain(..excess);
    }
    excess
}
/// Conservative source allocation measure, before any sample clone. Unsupported
/// rich values are rejected without traversal or projection/type erasure.
pub(crate) fn sample_bytes(value: &Kind) -> Option<usize> {
    Some(512usize.saturating_add(match value {
        Kind::Bool(_) | Kind::NA => 0,
        Kind::Number(number) => number.unit.as_ref().map_or(0, String::len),
        Kind::Str(value) => value.len(),
        _ => return None,
    }))
}
fn quantum(budget: &HistoryPullBudget) -> Result<Duration, HistoryProviderError> {
    budget.check()?;
    Ok(budget
        .deadline
        .saturating_duration_since(std::time::Instant::now())
        .min(Duration::from_millis(5)))
}
fn open_memory(
    store: HisStore,
    id: String,
    start: DateTime<FixedOffset>,
    end: DateTime<FixedOffset>,
    budget: HistoryPullBudget,
) -> Result<Box<dyn HistorySession>, HistoryProviderError> {
    let point = loop {
        if let Some(mut state) = store.inner.state.try_lock_for(quantum(&budget)?) {
            if !state.points.contains_key(&id)
                && (budget.max_bytes < 512 + id.len() || budget.max_work < 512 + id.len())
            {
                return Err(HistoryProviderError::Limit);
            }
            break store.point_in(&mut state, &id)?;
        }
    };
    let (cursor, state, coverage) = loop {
        if let Some(series) = point.try_read_for(quantum(&budget)?) {
            break (
                series.items.partition_point(|item| item.ts < start),
                store.series_state(&series),
                ProviderCoverage {
                    retained_start: series.items.first().map(|item| item.ts),
                    retained_end: series.items.last().map(|item| item.ts),
                    retained_count: series.items.len() as u64,
                    evicted_through: series.evicted,
                },
            );
        }
    };
    Ok(Box::new(MemorySession {
        point,
        cursor,
        end,
        metadata: ProviderHistoryMetadata {
            state,
            coverage,
            capabilities: HistoryCapabilities::BOUNDED_LIVE,
        },
        closed: false,
    }))
}
#[derive(Clone)]
struct MemorySession {
    point: Arc<RwLock<Series>>,
    cursor: usize,
    end: DateTime<FixedOffset>,
    metadata: ProviderHistoryMetadata,
    closed: bool,
}
impl MemorySession {
    fn next(
        &mut self,
        budget: &HistoryPullBudget,
    ) -> Result<ProviderHistoryBatch, HistoryProviderError> {
        budget.check()?;
        if self.closed {
            return Err(HistoryProviderError::Stopped);
        }
        let mut items = Vec::new();
        let series = loop {
            if let Some(series) = self.point.try_read_for(quantum(budget)?) {
                break series;
            }
        };
        if series.generation != self.metadata.state.generation
            || series.incarnation != self.metadata.state.incarnation
        {
            return Err(HistoryProviderError::Changed);
        }
        let mut bytes = 0usize;
        let mut work = 0usize;
        while self.cursor < series.items.len() && series.items[self.cursor].ts < self.end {
            budget.check()?;
            if items.len() >= budget.max_rows {
                break;
            }
            let item = &series.items[self.cursor];
            let Some(size) = sample_bytes(&item.val) else {
                return Ok(ProviderHistoryBatch {
                    items,
                    terminal: Some(HistoryTerminal::Failed(HistoryReason::UnsupportedValue)),
                });
            };
            if size > budget.max_bytes.saturating_sub(bytes)
                || size > budget.max_work.saturating_sub(work)
            {
                let terminal = items.is_empty().then_some(HistoryTerminal::Limited(
                    if size > budget.max_bytes.saturating_sub(bytes) {
                        HistoryReason::Bytes
                    } else {
                        HistoryReason::Work
                    },
                ));
                return Ok(ProviderHistoryBatch { items, terminal });
            }
            // Admission precedes allocation, including the first oversized value.
            bytes += size;
            work += size;
            items.push(item.clone());
            self.cursor += 1;
        }
        let complete =
            self.cursor == series.items.len() || series.items[self.cursor].ts >= self.end;
        Ok(ProviderHistoryBatch {
            items,
            terminal: complete.then_some(HistoryTerminal::Complete),
        })
    }
}
impl HistorySession for MemorySession {
    fn metadata(&self) -> &ProviderHistoryMetadata {
        &self.metadata
    }
    fn pull(&mut self, budget: HistoryPullBudget) -> HistoryFuture<'_, ProviderHistoryBatch> {
        // A contended native writer must not block the runtime that delivers
        // cancellation. This future observes the real blocking job completion.
        let mut worker = self.clone();
        Box::pin(async move {
            let (cursor, result) = tokio::task::spawn_blocking(move || {
                let result = worker.next(&budget);
                (worker.cursor, result)
            })
            .await
            .map_err(|_| HistoryProviderError::Failed)?;
            self.cursor = cursor;
            result
        })
    }
    fn close(&mut self) -> HistoryFuture<'_, ()> {
        self.closed = true;
        Box::pin(async { Ok(()) })
    }
}
impl HistoryProvider for HisStore {
    fn history_write_capability(&self) -> Option<crate::HistoryWriteCapability> {
        Some(crate::HistoryWriteCapability {
            store: self.clone(),
            qualification: crate::HistoryReceiptQualification::EphemeralMemory,
        })
    }
    fn commit_history(
        &self,
        prepared: crate::PreparedHistoryMutation,
    ) -> crate::HistoryWriteOutcome {
        prepared.publish()
    }

    fn open(
        &self,
        id: String,
        start: DateTime<FixedOffset>,
        end: DateTime<FixedOffset>,
        budget: HistoryPullBudget,
    ) -> HistoryFuture<'_, Box<dyn HistorySession>> {
        let store = self.clone();
        Box::pin(async move {
            if id.len() > 256 {
                return Err(HistoryProviderError::Limit);
            }
            tokio::task::spawn_blocking(move || open_memory(store, id, start, end, budget))
                .await
                .map_err(|_| HistoryProviderError::Failed)?
        })
    }
    fn his_write(&self, id: &str, items: Vec<HisItem>) -> HistoryFuture<'_, ()> {
        let result = self.write(id, items);
        Box::pin(async move { result })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haystack_core::kinds::Number;
    fn item(second: i64, value: f64) -> HisItem {
        HisItem {
            ts: DateTime::from_timestamp(second, 0).unwrap().fixed_offset(),
            val: Kind::Number(Number::unitless(value)),
        }
    }
    #[test]
    fn native_writes_preserve_order_and_last_duplicate_wins() {
        let store = HisStore::new();
        store
            .write(
                "p",
                vec![item(3, 3.0), item(1, 1.0), item(2, 2.0), item(1, 4.0)],
            )
            .unwrap();
        let items = store.read("p", None, None);
        assert_eq!(
            items
                .iter()
                .map(|item| item.ts.timestamp())
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(items[0].val, Kind::Number(Number::unitless(4.0)));
        assert_eq!(
            store.read("p", Some(items[1].ts), Some(items[1].ts)).len(),
            1
        );
    }
    #[test]
    fn generation_exhaustion_has_no_native_write_effect() {
        let store = HisStore::new();
        store.write("p", vec![item(1, 1.0)]).unwrap();
        store
            .inner
            .state
            .lock()
            .points
            .get("p")
            .unwrap()
            .write()
            .generation = u64::MAX;
        assert_eq!(
            store.write("p", vec![item(1, 9.0), item(2, 2.0)]),
            Err(HistoryProviderError::Exhausted)
        );
        let items = store.read("p", None, None);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].val, Kind::Number(Number::unitless(1.0)));
    }
    #[tokio::test]
    async fn memory_lock_wait_does_not_block_the_runtime_cancellation_task() {
        let store = HisStore::new();
        store.write("p", vec![item(1, 1.0)]).unwrap();
        let cancellation = crate::CancellationToken::new();
        let budget = HistoryPullBudget {
            max_rows: 1,
            max_bytes: 1024,
            max_work: 1024,
            deadline: std::time::Instant::now() + Duration::from_millis(250),
            cancellation: cancellation.clone(),
        };
        let mut session = store
            .open("p".into(), item(0, 0.0).ts, item(3, 0.0).ts, budget.clone())
            .await
            .unwrap();
        let point = store.inner.state.lock().points.get("p").unwrap().clone();
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let _guard = point.write();
            held_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        held_rx.recv().unwrap();
        let token = cancellation.clone();
        let timer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            token.cancel();
        });
        let outcome = session.pull(budget).await;
        let cancelled_before_return = cancellation.is_cancelled();
        release_tx.send(()).unwrap();
        thread.join().unwrap();
        timer.await.unwrap();
        assert!(matches!(outcome, Err(HistoryProviderError::Stopped)));
        assert!(
            cancelled_before_return,
            "memory lock wait blocked the runtime until its own deadline"
        );
    }
}

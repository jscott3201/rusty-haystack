//! Generation-checked memory history. Sessions retain one point handle and a
//! cursor, never a range snapshot; copying happens only in a requested batch.
use crate::history_provider::*;
use chrono::{DateTime, FixedOffset};
use haystack_core::{
    codecs::history::{HistoryCapabilities, HistoryReason, HistoryState, HistoryTerminal},
    kinds::Kind,
};
use parking_lot::RwLock;
use std::{collections::HashMap, sync::Arc, time::Duration};

const MAX_ITEMS_PER_SERIES: usize = 1_000_000;
struct Series {
    incarnation: [u8; 16],
    generation: u64,
    items: Vec<HisItem>,
    evicted: Option<DateTime<FixedOffset>>,
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
type Points = Arc<RwLock<HashMap<String, Arc<RwLock<Series>>>>>;
#[derive(Clone)]
pub struct HisStore {
    points: Points,
    authority: [u8; 16],
    capacity: usize,
}
impl Default for HisStore {
    fn default() -> Self {
        Self::with_retention(MAX_ITEMS_PER_SERIES).expect("positive default retention")
    }
}
impl HisStore {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_retention(max_items_per_point: usize) -> Result<Self, HistoryProviderError> {
        if max_items_per_point == 0 || max_items_per_point > MAX_ITEMS_PER_SERIES {
            return Err(HistoryProviderError::Limit);
        }
        Ok(Self {
            points: Arc::new(RwLock::new(HashMap::new())),
            authority: rand::random(),
            capacity: max_items_per_point,
        })
    }
    /// Trusted in-process write. Duplicate timestamps use the last input value.
    /// Any nonempty successful write advances this point's generation once.
    pub fn write(&self, id: &str, items: Vec<HisItem>) -> Result<(), HistoryProviderError> {
        if items.is_empty() {
            return Ok(());
        }
        let point = self
            .points
            .write()
            .entry(id.to_owned())
            .or_default()
            .clone();
        let mut series = point.write();
        let generation = series
            .generation
            .checked_add(1)
            .ok_or(HistoryProviderError::Exhausted)?;
        for item in items {
            match series
                .items
                .binary_search_by(|existing| existing.ts.cmp(&item.ts))
            {
                Ok(index) => series.items[index] = item,
                Err(index) => series.items.insert(index, item),
            }
        }
        let excess = series.items.len().saturating_sub(self.capacity);
        if excess > 0 {
            let through = series.items[excess - 1].ts;
            series.evicted = Some(series.evicted.map_or(through, |old| old.max(through)));
            series.items.drain(..excess);
        }
        series.generation = generation;
        Ok(())
    }
    /// Unbounded trusted native compatibility read; neither authorized nor used
    /// by the bounded service. Its legacy end remains inclusive.
    pub fn read(
        &self,
        id: &str,
        start: Option<DateTime<FixedOffset>>,
        end: Option<DateTime<FixedOffset>>,
    ) -> Vec<HisItem> {
        let point = self.points.read().get(id).cloned();
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
        self.points
            .read()
            .get(id)
            .map_or(0, |point| point.read().items.len())
    }
    pub fn is_empty(&self, id: &str) -> bool {
        self.len(id) == 0
    }
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
        if let Some(map) = store.points.try_read_for(quantum(&budget)?) {
            break map.get(&id).cloned();
        }
    };
    let mut cursor = 0;
    let (state, coverage) = match &point {
        Some(point) => loop {
            if let Some(series) = point.try_read_for(quantum(&budget)?) {
                cursor = series.items.partition_point(|item| item.ts < start);
                break (
                    HistoryState {
                        authority: store.authority,
                        incarnation: series.incarnation,
                        generation: series.generation,
                    },
                    ProviderCoverage {
                        retained_start: series.items.first().map(|item| item.ts),
                        retained_end: series.items.last().map(|item| item.ts),
                        retained_count: series.items.len() as u64,
                        evicted_through: series.evicted,
                    },
                );
            }
        },
        None => (
            HistoryState {
                authority: store.authority,
                incarnation: rand::random(),
                generation: 0,
            },
            ProviderCoverage {
                retained_start: None,
                retained_end: None,
                retained_count: 0,
                evicted_through: None,
            },
        ),
    };
    Ok(Box::new(MemorySession {
        store,
        id,
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
    store: HisStore,
    id: String,
    point: Option<Arc<RwLock<Series>>>,
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
        let Some(point) = &self.point else {
            loop {
                if let Some(map) = self.store.points.try_read_for(quantum(budget)?) {
                    if map.contains_key(&self.id) {
                        return Err(HistoryProviderError::Changed);
                    }
                    return Ok(ProviderHistoryBatch {
                        items,
                        terminal: Some(HistoryTerminal::Complete),
                    });
                }
            }
        };
        let series = loop {
            if let Some(series) = point.try_read_for(quantum(budget)?) {
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
        store.points.read().get("p").unwrap().write().generation = u64::MAX;
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
        let point = store.points.read().get("p").unwrap().clone();
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

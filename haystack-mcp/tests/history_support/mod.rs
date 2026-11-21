use chrono::{DateTime, FixedOffset};
use haystack_app::*;
use haystack_core::{
    data::HDict,
    graph::{EntityGraph, SharedGraph},
    kinds::{HRef, Kind},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Stage {
    Open,
    Pull,
    Close,
}
pub struct Probe {
    pub store: HisStore,
    pub graph: SharedGraph,
    state: Arc<State>,
}
struct State {
    stage: Option<Stage>,
    entered: tokio::sync::Semaphore,
    release: tokio::sync::Semaphore,
    opens: AtomicUsize,
    pulls: AtomicUsize,
    closes: AtomicUsize,
    closed: AtomicUsize,
    fail_after: AtomicUsize,
    gate_after: AtomicUsize,
}
impl State {
    async fn gate(&self, stage: Stage) {
        if self.stage == Some(stage) {
            self.entered.add_permits(1);
            self.release.acquire().await.unwrap().forget();
        }
    }
}
impl Probe {
    pub fn new(stage: Option<Stage>) -> Arc<Self> {
        let store = HisStore::new();
        let start = DateTime::parse_from_rfc3339("2024-06-01T00:00:00Z").unwrap();
        store
            .write(
                "a",
                [Kind::Bool(true), Kind::NA, Kind::Bool(false)]
                    .into_iter()
                    .enumerate()
                    .map(|(i, val)| HisItem {
                        ts: start + chrono::Duration::seconds(i as i64),
                        val,
                    })
                    .collect(),
            )
            .unwrap();
        Arc::new(Self {
            store,
            graph: graph(),
            state: Arc::new(State {
                stage,
                entered: tokio::sync::Semaphore::new(0),
                release: tokio::sync::Semaphore::new(0),
                opens: AtomicUsize::new(0),
                pulls: AtomicUsize::new(0),
                closes: AtomicUsize::new(0),
                closed: AtomicUsize::new(0),
                fail_after: AtomicUsize::new(usize::MAX),
                gate_after: AtomicUsize::new(0),
            }),
        })
    }
    pub async fn wait(&self) {
        tokio::time::timeout(Duration::from_secs(2), self.state.entered.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
    }
    pub fn release(&self) {
        self.state.release.add_permits(1);
    }
    pub fn opens(&self) -> usize {
        self.state.opens.load(Ordering::SeqCst)
    }
    pub fn pulls(&self) -> usize {
        self.state.pulls.load(Ordering::SeqCst)
    }
    pub fn closes(&self) -> usize {
        self.state.closes.load(Ordering::SeqCst)
    }
    pub fn closed(&self) -> usize {
        self.state.closed.load(Ordering::SeqCst)
    }
    pub fn fail_after(&self, pulls: usize) {
        self.state.fail_after.store(pulls, Ordering::SeqCst);
    }
    pub fn gate_after(&self, pulls: usize) {
        self.state.gate_after.store(pulls, Ordering::SeqCst);
    }
}
struct Session {
    inner: Box<dyn HistorySession>,
    state: Arc<State>,
}
impl HistoryProvider for Probe {
    fn open(
        &self,
        id: String,
        start: DateTime<FixedOffset>,
        end: DateTime<FixedOffset>,
        budget: HistoryPullBudget,
    ) -> HistoryFuture<'_, Box<dyn HistorySession>> {
        Box::pin(async move {
            let inner = self.store.open(id, start, end, budget).await?;
            self.state.opens.fetch_add(1, Ordering::SeqCst);
            self.state.gate(Stage::Open).await;
            Ok(Box::new(Session {
                inner,
                state: self.state.clone(),
            }) as Box<dyn HistorySession>)
        })
    }
}
impl HistorySession for Session {
    fn metadata(&self) -> &ProviderHistoryMetadata {
        self.inner.metadata()
    }
    fn pull(&mut self, budget: HistoryPullBudget) -> HistoryFuture<'_, ProviderHistoryBatch> {
        Box::pin(async move {
            let n = self.state.pulls.fetch_add(1, Ordering::SeqCst);
            if n >= self.state.gate_after.load(Ordering::SeqCst) {
                self.state.gate(Stage::Pull).await;
            }
            if n >= self.state.fail_after.load(Ordering::SeqCst) {
                return Err(HistoryProviderError::Failed);
            }
            self.inner.pull(budget).await
        })
    }
    fn close(&mut self) -> HistoryFuture<'_, ()> {
        Box::pin(async move {
            self.state.closes.fetch_add(1, Ordering::SeqCst);
            self.state.gate(Stage::Close).await;
            let result = self.inner.close().await;
            self.state.closed.fetch_add(1, Ordering::SeqCst);
            result
        })
    }
}
pub fn graph() -> SharedGraph {
    let graph = SharedGraph::new(EntityGraph::new());
    for id in ["a", "b", "denied"] {
        let mut row = HDict::new();
        row.set("id", Kind::Ref(HRef::from_val(id)));
        row.set("his", Kind::Marker);
        row.set("kind", Kind::Str("Bool".into()));
        row.set("tz", Kind::Str("UTC".into()));
        graph.add(row).unwrap();
    }
    graph
}
pub fn args(profile: haystack_mcp::OutputProfile) -> serde_json::Value {
    let mut args = serde_json::json!({"id":"a","start":"2024-06-01T00:00:00Z UTC","end":"2024-06-01T00:00:03Z UTC","max_rows":10,"max_bytes":haystack_mcp::HISTORY_RESPONSE_BYTES,"timeout_ms":2000});
    if profile == haystack_mcp::OutputProfile::Jeto {
        args["profile"] = serde_json::json!(profile.name());
    }
    args
}
pub fn decoded(
    value: &serde_json::Value,
    profile: haystack_mcp::OutputProfile,
) -> HistoryReadResult {
    use haystack_core::codecs::{codec_for, history, jeto};
    let result = &value["result"];
    let payload = &result["structuredContent"];
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(result["content"][0]["text"].as_str().unwrap())
            .unwrap(),
        *payload
    );
    let body = serde_json::to_vec(&payload["grid"]).unwrap();
    let decoded = match profile {
        haystack_mcp::OutputProfile::H4 => {
            history::decode_result(&body, codec_for("application/json").unwrap()).unwrap()
        }
        haystack_mcp::OutputProfile::Jeto => {
            let Kind::Grid(grid) = jeto::decode(
                &body,
                &jeto::Context::standard(),
                Some("sys::Grid"),
                jeto::Limits::default(),
            )
            .unwrap() else {
                panic!()
            };
            history::result_from_grid(&grid).unwrap()
        }
    };
    assert_eq!(
        payload["complete"],
        decoded.terminal == HistoryTerminal::Complete
    );
    assert_eq!(
        result["isError"].as_bool().unwrap_or(false),
        decoded.terminal != HistoryTerminal::Complete
    );
    assert_eq!(payload["row_count"], decoded.samples.len());
    decoded
}

//! One admitted, joined history collection and encoding operation.
use super::*;
use crate::{HistoryService, HistoryTerminal};
use haystack_core::codecs::history::{self, HistoryReadRequest};

/// Per-call caps only tighten the original admission and selected provider.
#[derive(Debug, Clone, Copy)]
pub struct HistoryReadOptions {
    pub max_rows: usize,
    /// Encoded grid bytes, before adapter framing. The shared history encoder
    /// requires more than 128 KiB so metadata cannot exhaust source headroom.
    pub max_output_bytes: usize,
    pub deadline: Instant,
}

#[derive(Debug)]
pub struct HistoryReadOutput {
    pub output: ReadOutput,
    pub terminal: HistoryTerminal,
    pub row_count: usize,
}

impl ReadAdmission {
    /// Consume the original admission, retaining it through provider open,
    /// pull, close and final encoding. All error paths await actual worker
    /// release. Caller drop only requests cancellation; an adapter must retain
    /// this future independently of an SDK handler it may abandon.
    pub async fn history_joined(
        mut self,
        history: HistoryService,
        request: HistoryReadRequest,
        options: HistoryReadOptions,
        profile: OutputProfile,
    ) -> Result<HistoryReadOutput, ReadError> {
        let completion = self.retain_work()?.completion.0.clone();
        let result = self
            .history_encoded(history, request, options, profile)
            .await;
        completion.wait().await;
        result
    }

    async fn history_encoded(
        mut self,
        history: HistoryService,
        request: HistoryReadRequest,
        options: HistoryReadOptions,
        profile: OutputProfile,
    ) -> Result<HistoryReadOutput, ReadError> {
        if !self.belongs_to(history.read_service()) {
            return Err(ReadError::Forbidden);
        }
        if options.max_rows == 0 {
            return Err(ReadError::InvalidLimits);
        }
        if !matches!(
            profile,
            OutputProfile::H4(H4Codec::Json) | OutputProfile::Jeto
        ) {
            return Err(ReadError::InvalidQuery(
                "unsupported history output profile",
            ));
        }
        let budget = self.budget_mut();
        let mut limits = (*budget.limits).clone();
        limits.max_rows = limits.max_rows.min(options.max_rows);
        limits.max_output_bytes = limits.max_output_bytes.min(options.max_output_bytes);
        if limits.max_output_bytes <= 128 * 1024 {
            return Err(ReadError::Budget(BudgetKind::Output));
        }
        budget.limits = Arc::new(limits);
        budget.deadline = budget.deadline.min(options.deadline);
        budget.check()?;
        let inner = self.inner.clone();
        let (scope, catalog) = self
            .stage()?
            .run_task(move |principal, budget| {
                let policy = inner.policy.snapshot(&principal)?;
                let scope = budget.copy_string(policy.scope_key())?;
                let catalog = if profile == OutputProfile::Jeto {
                    loop {
                        if let Some(value) = inner.graph.read_for(budget.wait_quantum()?, |graph| {
                            graph
                                .activated_catalog()
                                .cloned()
                                .ok_or(ReadError::Unavailable)
                        }) {
                            break Some(value?);
                        }
                    }
                } else {
                    None
                };
                Ok((scope, catalog))
            })
            .await?;
        self.budget_mut().typed_catalog = catalog.clone();
        // Both profiles reserve source copies and separate final-encoding
        // headroom before granting even the first provider pull.
        let mut session = history.open_typed(self.stage()?, request).await?;
        let mut result = session.collect_borrowed().await?;
        let terminal = {
            let completed = session.completed();
            tokio::pin!(completed);
            tokio::select! {
                terminal = &mut completed => terminal,
                _ = tokio::time::sleep_until(tokio::time::Instant::from_std(self.deadline())) => {
                    self.cancellation().cancel();
                    completed.await
                },
                _ = self.cancelled() => {
                    self.cancellation().cancel();
                    completed.await
                },
            }
        };
        if let Some(terminal) = terminal {
            result.terminal = terminal;
        }
        session.release_completed();
        drop(session);
        let inner = self.inner.clone();
        self.run_task(move |principal, budget| {
            let policy = inner.policy.snapshot(&principal)?;
            if policy.scope_key() != scope
                || !policy.operation(ReadOperation::History)
                || !policy.entity(&result.metadata.id)
                || ["id", "kind", "tz", "unit", "his", "val"]
                    .iter()
                    .any(|tag| !policy.tag(&result.metadata.id, tag))
            {
                return Err(ReadError::Unavailable);
            }
            loop {
                if let Some(encoded) = inner.graph.read_for(budget.wait_quantum()?, |graph| {
                    budget.check_catalog(graph)?;
                    if graph.state() != result.metadata.graph {
                        result.terminal =
                            HistoryTerminal::Interrupted(history::HistoryReason::MetadataChanged);
                    }
                    let grid = history::result_grid(&result).map_err(|_| ReadError::Projection)?;
                    let output = if let Some(catalog) = &catalog {
                        output::jeto(grid, catalog, budget)?
                    } else {
                        output::encode(grid, profile, budget)?
                    };
                    Ok(HistoryReadOutput {
                        output,
                        terminal: result.terminal,
                        row_count: result.samples.len(),
                    })
                }) {
                    return encoded;
                }
            }
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;
    use haystack_core::{
        graph::{EntityGraph, SharedGraph},
        kinds::HRef,
    };
    use std::time::Duration;

    async fn setup() -> (ApplicationOwner, ReadService, HistoryService) {
        let graph = SharedGraph::new(EntityGraph::new());
        let mut point = HDict::new();
        point.set("id", Kind::Ref(HRef::from_val("p")));
        point.set("his", Kind::Marker);
        point.set("kind", Kind::Str("Bool".into()));
        point.set("tz", Kind::Str("UTC".into()));
        graph.add(point).unwrap();
        let builder = ApplicationBuilder::new(
            graph,
            Arc::new(AllowAll),
            ReadLimits {
                max_concurrent: 1,
                max_queued: 0,
                ..Default::default()
            },
        )
        .unwrap()
        .shutdown_policy(ShutdownPolicy {
            drain_timeout: Duration::ZERO,
            stop_timeout: Duration::from_secs(2),
        });
        let reads = builder.handle().read_service();
        let store = Arc::new(HisStore::new());
        store
            .write(
                "p",
                vec![HisItem {
                    ts: chrono::DateTime::parse_from_rfc3339("2024-06-01T00:00:00Z").unwrap(),
                    val: Kind::Bool(true),
                }],
            )
            .unwrap();
        let history = HistoryService::new(reads.clone(), store, HistoryLimits::default()).unwrap();
        let owner = builder
            .owned_history(history.clone())
            .unwrap()
            .start(&tokio::runtime::Handle::current())
            .unwrap();
        owner.ready().await.unwrap();
        (owner, reads, history)
    }
    fn request() -> HistoryReadRequest {
        HistoryService::finite_request(
            "p".into(),
            "2024-06-01T00:00:00Z UTC",
            "2024-06-01T00:00:01Z UTC",
        )
        .unwrap()
    }
    fn options(deadline: Instant) -> HistoryReadOptions {
        HistoryReadOptions {
            max_rows: 10,
            max_output_bytes: 256 * 1024,
            deadline,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn joined_history_encoding_stop_retains_original_work_until_actual_exit() {
        for stop in ["cancel", "deadline", "owner"] {
            let (owner, reads, history) = setup().await;
            let cancel = CancellationToken::new();
            let deadline =
                Instant::now() + Duration::from_millis(if stop == "deadline" { 300 } else { 3000 });
            let mut admission = reads
                .begin(ReadContext::new(
                    Principal::Anonymous,
                    deadline,
                    cancel.clone(),
                ))
                .await
                .unwrap();
            let (entered_tx, entered_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let release_rx = std::sync::Mutex::new(release_rx);
            admission.budget_mut().typed_encode_hook = Some(Arc::new(move || {
                entered_tx.send(()).unwrap();
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(3))
                    .unwrap();
            }));
            let observer = history.clone();
            let task = tokio::spawn(admission.history_joined(
                history,
                request(),
                options(deadline),
                OutputProfile::Jeto,
            ));
            entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
            assert_eq!(observer.active_sessions(), 0, "provider has already closed");
            match stop {
                "cancel" => cancel.cancel(),
                "owner" => owner.request_close(),
                _ => tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await,
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
            let held = (
                !task.is_finished(),
                reads.load().admitted,
                owner.handle().outstanding_tasks(),
            );
            release_tx.send(()).unwrap();
            assert!(
                held.0,
                "{stop}: encoding worker was reported joined before release"
            );
            assert_eq!(held.1, 1, "{stop}");
            assert!(held.2 > 0, "{stop}");
            assert_eq!(
                task.await.unwrap().unwrap_err(),
                if stop == "deadline" {
                    ReadError::Deadline
                } else {
                    ReadError::Cancelled
                }
            );
            assert_eq!(reads.load().admitted, 0);
            owner.request_close();
            let report = owner.terminated().await;
            assert!(report.close.is_ok() && report.cleanup_errors.is_empty());
        }
    }

    #[tokio::test]
    async fn joined_history_final_encoder_shares_the_original_cumulative_budget() {
        for kind in [BudgetKind::Retained, BudgetKind::Work] {
            let (owner, reads, history) = setup().await;
            let mut admission = reads
                .begin(ReadContext::with_timeout(
                    Principal::Anonymous,
                    Duration::from_secs(3),
                ))
                .await
                .unwrap();
            let deadline = admission.deadline();
            let original = Arc::new(Mutex::new(admission.budget_mut().clone()));
            // Observe the counters captured before any preparation/collection.
            // Consuming their exact remainder must also exhaust the encoder.
            admission.budget_mut().typed_encode_hook = Some(Arc::new(move || {
                let mut original = original.lock();
                let remaining = if kind == BudgetKind::Retained {
                    original.retained_remaining()
                } else {
                    original.work_remaining()
                };
                original.charge(kind, remaining).unwrap();
            }));
            assert_eq!(
                admission
                    .history_joined(history, request(), options(deadline), OutputProfile::Jeto)
                    .await
                    .unwrap_err(),
                ReadError::Budget(kind)
            );
            assert_eq!(reads.load().admitted, 0);
            owner.close().await.unwrap();
        }
    }
}

//! Application-owned, authorized, demand-driven history sessions.
use crate::{
    BudgetKind, CancellationToken, Principal, ReadAdmission, ReadContext, ReadError, ReadOperation,
    ReadService,
    budget::Budget,
    history_provider::*,
    history_range::{parse_range, point_time, validate_zone},
    history_store::sample_bytes,
};
use chrono::{DateTime, FixedOffset, Utc};
use haystack_core::{
    codecs::history::*,
    graph::GraphState,
    kinds::{Kind, unit_for},
};
use parking_lot::Mutex;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{mpsc, oneshot, watch};

#[derive(Debug, Clone)]
pub struct HistoryLimits {
    pub max_sessions: usize,
    pub max_sessions_per_principal: usize,
    pub batch_rows: usize,
    pub batch_bytes: usize,
    pub total_rows: usize,
    pub total_bytes: usize,
    pub total_work: usize,
}
impl Default for HistoryLimits {
    fn default() -> Self {
        Self {
            max_sessions: 8,
            max_sessions_per_principal: 2,
            batch_rows: 128,
            batch_bytes: 128 * 1024,
            total_rows: 1000,
            total_bytes: 1024 * 1024,
            total_work: 4 * 1024 * 1024,
        }
    }
}
impl HistoryLimits {
    fn validate(&self) -> Result<(), ReadError> {
        if self.max_sessions == 0
            || self.max_sessions > 1024
            || self.max_sessions_per_principal == 0
            || self.max_sessions_per_principal > self.max_sessions
            || self.batch_rows == 0
            || self.batch_rows > 10_000
            || self.batch_bytes == 0
            || self.batch_bytes > 1024 * 1024
            || self.total_rows == 0
            || self.total_rows > 100_000
            || self.total_bytes == 0
            || self.total_bytes > 16 * 1024 * 1024
            || self.total_work == 0
            || self.total_work > 256 * 1024 * 1024
        {
            Err(ReadError::InvalidLimits)
        } else {
            Ok(())
        }
    }
}
pub trait HistoryClock: Send + Sync + 'static {
    fn now(&self) -> DateTime<FixedOffset>;
}
struct SystemClock;
impl HistoryClock for SystemClock {
    fn now(&self) -> DateTime<FixedOffset> {
        Utc::now().fixed_offset()
    }
}
#[derive(Default)]
struct Sessions {
    total: usize,
    principals: HashMap<String, usize>,
}
struct SessionLease {
    sessions: Arc<Mutex<Sessions>>,
    key: String,
}
impl Drop for SessionLease {
    fn drop(&mut self) {
        let mut sessions = self.sessions.lock();
        sessions.total -= 1;
        if let Some(count) = sessions.principals.get_mut(&self.key) {
            *count -= 1;
            if *count == 0 {
                sessions.principals.remove(&self.key);
            }
        }
    }
}
#[derive(Clone)]
pub struct HistoryService {
    reads: ReadService,
    provider: Arc<dyn HistoryProvider>,
    limits: Arc<HistoryLimits>,
    sessions: Arc<Mutex<Sessions>>,
    clock: Arc<dyn HistoryClock>,
}
impl HistoryService {
    pub fn new(
        reads: ReadService,
        provider: Arc<dyn HistoryProvider>,
        limits: HistoryLimits,
    ) -> Result<Self, ReadError> {
        limits.validate()?;
        if reads.limits().max_output_bytes <= 128 * 1024 {
            return Err(ReadError::InvalidLimits);
        }
        Ok(Self {
            reads,
            provider,
            limits: Arc::new(limits),
            sessions: Arc::new(Mutex::new(Sessions::default())),
            clock: Arc::new(SystemClock),
        })
    }
    pub fn with_clock(mut self, clock: Arc<dyn HistoryClock>) -> Self {
        self.clock = clock;
        self
    }
    pub fn read_service(&self) -> &ReadService {
        &self.reads
    }
    pub fn active_sessions(&self) -> usize {
        self.sessions.lock().total
    }
    /// Trusted native backend access, outside the authorized read API.
    pub fn provider(&self) -> Arc<dyn HistoryProvider> {
        self.provider.clone()
    }
    pub async fn open(
        &self,
        context: ReadContext,
        request: HistoryReadRequest,
    ) -> Result<HistoryReadSession, ReadError> {
        self.open_admitted(self.reads.begin(context).await?, request)
            .await
    }
    pub async fn open_admitted(
        &self,
        admission: ReadAdmission,
        request: HistoryReadRequest,
    ) -> Result<HistoryReadSession, ReadError> {
        self.open_configured(admission, request, false).await
    }
    async fn open_configured(
        &self,
        admission: ReadAdmission,
        request: HistoryReadRequest,
        wire: bool,
    ) -> Result<HistoryReadSession, ReadError> {
        if !admission.belongs_to(&self.reads) {
            return Err(ReadError::Forbidden);
        }
        let runtime = admission.runtime();
        let (principal, mut budget) = admission.into_session()?;
        if matches!(&principal, Principal::Authenticated { permissions, .. } if permissions.len() > budget.limits.max_ids)
        {
            return Err(ReadError::Budget(BudgetKind::Input));
        }
        if principal.bytes() > budget.limits.max_input_bytes
            || request.id.len().saturating_add(request.range.len()) > budget.limits.max_input_bytes
            || request.id.len() > 256
            || request.range.len() > 1024
        {
            return Err(ReadError::Budget(BudgetKind::Input));
        }
        budget.charge(
            BudgetKind::Retained,
            principal
                .bytes()
                .saturating_add(request.id.len())
                .saturating_add(request.range.len()),
        )?;
        // Principal capacity keys make two bounded owned copies (table/lease).
        budget.charge(BudgetKind::Work, principal.bytes())?;
        budget.charge(
            BudgetKind::Retained,
            principal.bytes().saturating_mul(2).saturating_add(128),
        )?;
        let key = match &principal {
            Principal::Anonymous => "anonymous".to_owned(),
            Principal::Authenticated { subject, .. } => format!("authenticated:{subject}"),
            Principal::TrustedEmbedding { subject } => format!("embedding:{subject}"),
        };
        let lease = {
            let mut sessions = self.sessions.lock();
            if sessions.total >= self.limits.max_sessions
                || sessions.principals.get(&key).copied().unwrap_or(0)
                    >= self.limits.max_sessions_per_principal
            {
                return Err(ReadError::Capacity);
            }
            sessions.total += 1;
            *sessions.principals.entry(key.clone()).or_default() += 1;
            SessionLease {
                sessions: self.sessions.clone(),
                key,
            }
        };
        let cancel = budget.cancel.clone();
        let mut drop_guard = CancelOpen {
            token: cancel.clone(),
            armed: true,
        };
        let (commands, receiver) = mpsc::channel(1);
        let (terminal, terminal_rx) = watch::channel(None);
        let (done, done_rx) = watch::channel(false);
        let (ready, result) = oneshot::channel();
        let service = self.clone();
        runtime.spawn(async move {
            service
                .coordinate(principal, request, budget, receiver, terminal, ready, wire)
                .await;
            // Only actual provider/session completion releases these leases.
            drop(lease);
            let _ = done.send(true);
        });
        let metadata = result.await.map_err(|_| ReadError::Unavailable)??;
        drop_guard.armed = false;
        Ok(HistoryReadSession {
            metadata,
            commands,
            terminal: terminal_rx,
            done: done_rx,
            cancel,
            pending: None,
        })
    }
    /// One bounded H4 collector. Decode and encoding keep the original work
    /// registration and capacity even when the provider session finishes first.
    pub async fn wire_admitted(
        &self,
        admission: ReadAdmission,
        body: Vec<u8>,
        input: crate::H4Codec,
        output: crate::H4Codec,
    ) -> Result<Vec<u8>, ReadError> {
        self.collect_wire(admission, body, input, output, true)
            .await
    }
    /// Legacy grid callers cannot mistake a partial collection for completeness.
    pub async fn legacy_wire_admitted(
        &self,
        admission: ReadAdmission,
        body: Vec<u8>,
        input: crate::H4Codec,
        output: crate::H4Codec,
    ) -> Result<Vec<u8>, ReadError> {
        self.collect_wire(admission, body, input, output, false)
            .await
    }
    async fn collect_wire(
        &self,
        mut admission: ReadAdmission,
        body: Vec<u8>,
        input: crate::H4Codec,
        output: crate::H4Codec,
        scoped: bool,
    ) -> Result<Vec<u8>, ReadError> {
        use haystack_core::codecs::{codec_for, history};
        if !admission.belongs_to(&self.reads) {
            return Err(ReadError::Forbidden);
        }
        if input == crate::H4Codec::Trio || output == crate::H4Codec::Trio {
            return Err(ReadError::InvalidQuery("unsupported history codec"));
        }
        let _lease = admission.retain_work()?;
        let budget = admission.budget_mut();
        if body.len() > history::MAX_REQUEST_BYTES || body.len() > budget.limits.max_input_bytes {
            return Err(ReadError::Budget(BudgetKind::Input));
        }
        budget.charge(BudgetKind::Work, body.len())?;
        budget.charge(
            BudgetKind::Retained,
            body.len().saturating_mul(16).saturating_add(4096),
        )?;
        let max_output = budget.limits.max_output_bytes;
        let decode = if scoped {
            history::decode_request
        } else {
            history::decode_legacy_request
        };
        let request = decode(
            &body,
            codec_for(input.mime()).ok_or(ReadError::Unavailable)?,
        )
        .map_err(|_| ReadError::InvalidQuery("invalid bounded history request"))?;
        budget.check()?;
        // Keep the same cumulative counters, absolute deadline, cancellation
        // and work lease through every response-construction stage. The
        // coordinator reserves encoder headroom before granting source pulls.
        let final_budget = budget.clone();
        let mut session = self.open_configured(admission, request, true).await?;
        let result = session.collect_borrowed().await?;
        final_budget.check()?;
        if !scoped && result.terminal != HistoryTerminal::Complete {
            return Err(ReadError::UnitTooLarge);
        }
        let mut grid = history::result_grid(&result).map_err(|_| ReadError::Projection)?;
        final_budget.check()?;
        if !scoped {
            grid.meta.remove_tag("history");
        }
        let codec = codec_for(output.mime()).ok_or(ReadError::Unavailable)?;
        let bytes = codec
            .encode_grid(&grid)
            .map_err(|_| ReadError::Projection)?
            .into_bytes();
        final_budget.check()?;
        if bytes.len() > max_output {
            return Err(ReadError::Budget(BudgetKind::Output));
        }
        Ok(bytes)
    }
    pub async fn collect(
        &self,
        context: ReadContext,
        request: HistoryReadRequest,
    ) -> Result<HistoryReadResult, ReadError> {
        self.open(context, request).await?.collect().await
    }
    #[allow(clippy::too_many_arguments)]
    async fn coordinate(
        &self,
        principal: Principal,
        request: HistoryReadRequest,
        mut budget: Budget,
        mut commands: mpsc::Receiver<oneshot::Sender<HistoryBatch>>,
        terminal_tx: watch::Sender<Option<HistoryTerminal>>,
        ready: oneshot::Sender<Result<HistoryMetadata, ReadError>>,
        wire: bool,
    ) {
        let observation = self.observe(&principal, &request.id, &mut budget).await;
        let (schema, graph, scope) = match observation {
            Ok(value) => value,
            Err(error) => {
                let _ = ready.send(Err(error));
                return;
            }
        };
        if wire && let Err(error) = reserve_wire_metadata(&mut budget, &request, &schema) {
            let _ = ready.send(Err(error));
            return;
        }
        let now = self.clock.now();
        let (start, end) = match parse_range(&request.range, &schema.timezone, now) {
            Ok(value) => value,
            Err(error) => {
                let _ = ready.send(Err(error));
                return;
            }
        };
        let pull_budget = self.pull_budget(&budget, 0, 0, 0, wire);
        let mut open = self
            .provider
            .open(request.id.clone(), start.dt, end.dt, pull_budget);
        let result = tokio::select! {
            result = &mut open => result,
            reason = stopped(&budget) => {
                budget.cancel.cancel();
                let _ = ready.send(Err(read_stop(reason)));
                if let Ok(mut session) = open.await { let _ = session.close().await; }
                return;
            }
        };
        let mut session = match result {
            Ok(session) => session,
            Err(_) => {
                let _ = ready.send(Err(ReadError::Unavailable));
                return;
            }
        };
        let metadata = self.metadata(
            &request,
            now,
            schema,
            graph,
            start,
            end,
            session.metadata(),
            &mut budget,
        );
        let metadata = match metadata {
            Ok(value) => value,
            Err(error) => {
                let _ = ready.send(Err(error));
                let _ = session.close().await;
                return;
            }
        };
        if let Err(terminal) = self
            .revalidate(&principal, &metadata, &scope, &mut budget)
            .await
        {
            let _ = ready.send(Err(read_stop(terminal_reason(terminal))));
            let _ = session.close().await;
            return;
        }
        if ready.send(Ok(metadata.clone())).is_err() {
            let _ = session.close().await;
            return;
        }
        let mut rows = 0usize;
        let mut bytes = 0usize;
        let mut work = 0usize;
        let mut first = None;
        let mut last = None;
        loop {
            let reply = tokio::select! {
                command = commands.recv() => match command { Some(reply) => reply, None => { let _ = terminal_tx.send(Some(HistoryTerminal::Interrupted(HistoryReason::Cancelled))); break; } },
                reason = stopped(&budget) => { let _ = terminal_tx.send(Some(HistoryTerminal::Interrupted(reason))); break; }
            };
            let outcome = self
                .revalidate(&principal, &metadata, &scope, &mut budget)
                .await;
            if let Err(terminal) = outcome {
                finish(reply, &terminal_tx, vec![], terminal);
                break;
            }
            let pull_budget = self.pull_budget(&budget, rows, bytes, work, wire);
            let limited = if pull_budget.max_rows == 0 {
                Some(HistoryReason::Rows)
            } else if pull_budget.max_bytes == 0 {
                Some(HistoryReason::Bytes)
            } else if pull_budget.max_work == 0 {
                Some(HistoryReason::Work)
            } else {
                None
            };
            if let Some(reason) = limited {
                finish(
                    reply,
                    &terminal_tx,
                    vec![],
                    HistoryTerminal::Limited(reason),
                );
                break;
            }
            let mut future = session.pull(pull_budget.clone());
            let batch = tokio::select! {
                result = &mut future => result,
                reason = stopped(&budget) => {
                    budget.cancel.cancel();
                    finish(reply, &terminal_tx, vec![], HistoryTerminal::Interrupted(reason));
                    // Keep the future and original WorkLease alive until completion.
                    let _ = future.await;
                    break;
                }
            };
            drop(future);
            let batch = match batch {
                Ok(batch) => batch,
                Err(error) => {
                    let terminal = match error {
                        HistoryProviderError::Changed => {
                            HistoryTerminal::Interrupted(HistoryReason::GenerationChanged)
                        }
                        HistoryProviderError::Stopped => {
                            HistoryTerminal::Interrupted(stop_reason(&budget))
                        }
                        HistoryProviderError::Limit => {
                            HistoryTerminal::Limited(HistoryReason::Work)
                        }
                        _ => HistoryTerminal::Failed(HistoryReason::Provider),
                    };
                    finish(reply, &terminal_tx, vec![], terminal);
                    break;
                }
            };
            if let Err(terminal) = self
                .revalidate(&principal, &metadata, &scope, &mut budget)
                .await
            {
                finish(reply, &terminal_tx, vec![], terminal);
                break;
            }
            // Metadata is immutable for this provider session; a hostile provider
            // cannot silently swap state/schema while reporting ordinary batches.
            let current = session.metadata();
            if current.state != metadata.history
                || current.capabilities != metadata.capabilities
                || current.coverage.retained_count != metadata.coverage.retained_count
                || current.coverage.retained_start
                    != metadata
                        .coverage
                        .retained_start
                        .as_ref()
                        .map(|value| value.dt)
                || current.coverage.retained_end
                    != metadata
                        .coverage
                        .retained_end
                        .as_ref()
                        .map(|value| value.dt)
                || current.coverage.evicted_through
                    != metadata
                        .coverage
                        .evicted_through
                        .as_ref()
                        .map(|value| value.dt)
            {
                finish(
                    reply,
                    &terminal_tx,
                    vec![],
                    HistoryTerminal::Failed(HistoryReason::InvalidProvider),
                );
                break;
            }
            if batch.items.len() > pull_budget.max_rows
                || batch
                    .terminal
                    .is_some_and(|terminal| !valid_terminal(terminal))
                || (batch.items.is_empty() && batch.terminal.is_none())
            {
                finish(
                    reply,
                    &terminal_tx,
                    vec![],
                    HistoryTerminal::Failed(HistoryReason::InvalidProvider),
                );
                break;
            }
            let mut samples = Vec::new();
            let mut batch_bytes = 0usize;
            let mut batch_work = 0usize;
            let mut terminal = batch.terminal;
            for item in batch.items {
                let Some(size) = sample_bytes(&item.val) else {
                    terminal = Some(HistoryTerminal::Failed(HistoryReason::UnsupportedValue));
                    break;
                };
                if size > pull_budget.max_bytes.saturating_sub(batch_bytes)
                    || size > pull_budget.max_work.saturating_sub(batch_work)
                {
                    terminal = Some(HistoryTerminal::Failed(HistoryReason::InvalidProvider));
                    break;
                }
                if item.ts < metadata.start.dt
                    || item.ts >= metadata.end.dt
                    || last.is_some_and(|previous| item.ts <= previous)
                    || metadata
                        .coverage
                        .retained_start
                        .as_ref()
                        .is_none_or(|start| item.ts < start.dt)
                    || metadata
                        .coverage
                        .retained_end
                        .as_ref()
                        .is_none_or(|end| item.ts > end.dt)
                    || (rows + samples.len()) as u64 >= metadata.coverage.retained_count
                {
                    terminal = Some(HistoryTerminal::Failed(HistoryReason::InvalidProvider));
                    break;
                }
                if !valid_value(&item.val, &metadata.schema) {
                    terminal = Some(HistoryTerminal::Failed(HistoryReason::UnsupportedValue));
                    break;
                }
                // The original Kind is validated; there is no Int/Float erasure,
                // reference/display masking, dropped row, or Null substitution.
                if let Err(error) = budget.charge(BudgetKind::Values, 1).and_then(|_| {
                    budget.charge(
                        BudgetKind::Work,
                        size.saturating_mul(if wire { WIRE_SAMPLE_WORK } else { 1 }),
                    )
                }) {
                    terminal = Some(budget_terminal(error, &budget));
                    break;
                }
                if let Err(error) = budget.charge(
                    BudgetKind::Retained,
                    size.saturating_mul(if wire { WIRE_SAMPLE_RETAINED } else { 1 }),
                ) {
                    terminal = Some(budget_terminal(error, &budget));
                    break;
                }
                let ts = match point_time(item.ts, &metadata.schema.timezone) {
                    Ok(ts) => ts,
                    Err(_) => {
                        terminal = Some(HistoryTerminal::Failed(HistoryReason::InvalidProvider));
                        break;
                    }
                };
                first.get_or_insert(item.ts);
                last = Some(item.ts);
                batch_bytes += size;
                batch_work += size;
                samples.push(HistorySample { ts, val: item.val });
            }
            rows += samples.len();
            bytes += batch_bytes;
            work += batch_work;
            if terminal == Some(HistoryTerminal::Complete)
                && let Some((start, end)) = metadata
                    .coverage
                    .retained_start
                    .as_ref()
                    .zip(metadata.coverage.retained_end.as_ref())
            {
                let includes_first = metadata.start.dt <= start.dt && start.dt < metadata.end.dt;
                let includes_last = metadata.start.dt <= end.dt && end.dt < metadata.end.dt;
                if (includes_first && first != Some(start.dt))
                    || (includes_last && last != Some(end.dt))
                    || (includes_first
                        && includes_last
                        && rows as u64 != metadata.coverage.retained_count)
                {
                    terminal = Some(HistoryTerminal::Failed(HistoryReason::InvalidProvider));
                }
            }
            if let Some(terminal) = terminal {
                finish_closed(
                    session.as_mut(),
                    reply,
                    &terminal_tx,
                    samples,
                    terminal,
                    &budget,
                )
                .await;
                return;
            }
            if reply
                .send(HistoryBatch {
                    samples,
                    terminal: None,
                })
                .is_err()
            {
                let _ =
                    terminal_tx.send(Some(HistoryTerminal::Interrupted(HistoryReason::Cancelled)));
                break;
            }
        }
        if session.close().await.is_err() {
            let _ = terminal_tx.send(Some(HistoryTerminal::Failed(HistoryReason::Provider)));
        }
    }
    fn pull_budget(
        &self,
        budget: &Budget,
        rows: usize,
        bytes: usize,
        work: usize,
        wire: bool,
    ) -> HistoryPullBudget {
        HistoryPullBudget {
            max_rows: self.limits.batch_rows.min(
                self.limits
                    .total_rows
                    .min(budget.limits.max_rows)
                    .saturating_sub(rows),
            ),
            max_bytes: self
                .limits
                .batch_bytes
                .min(
                    self.limits
                        .total_bytes
                        .min(budget.limits.max_output_bytes.saturating_sub(128 * 1024) / 6)
                        .saturating_sub(bytes),
                )
                .min(budget.retained_remaining() / if wire { WIRE_SAMPLE_RETAINED } else { 1 }),
            max_work: self
                .limits
                .total_work
                .min(budget.limits.max_work)
                .saturating_sub(work)
                .min(budget.work_remaining() / if wire { WIRE_SAMPLE_WORK } else { 1 }),
            deadline: budget.deadline,
            cancellation: budget.cancel.clone(),
        }
    }
    async fn observe(
        &self,
        principal: &Principal,
        id: &str,
        budget: &mut Budget,
    ) -> Result<(HistorySchema, GraphState, String), ReadError> {
        let graph = self.reads.graph();
        loop {
            budget.check()?;
            let policy = self.reads.policy_snapshot(principal)?;
            if !policy.operation(ReadOperation::History) {
                return Err(ReadError::Forbidden);
            }
            if !policy.entity(id) {
                return Err(ReadError::Unavailable);
            }
            if policy.scope_key().len() > budget.limits.max_input_bytes {
                return Err(ReadError::Budget(BudgetKind::Input));
            }
            for tag in ["id", "kind", "tz", "unit", "his", "val"] {
                if !policy.tag(id, tag) {
                    return Err(ReadError::Unavailable);
                }
            }
            if let Some(result) = graph.read_for(std::time::Duration::ZERO, |graph| {
                let row = graph.get(id).ok_or(ReadError::Unavailable)?;
                if !matches!(row.get("his"), Some(Kind::Marker)) {
                    return Err(ReadError::InvalidQuery("point does not declare history"));
                }
                let kind = match row.get("kind") {
                    Some(Kind::Str(value)) if value == "Bool" => HistoryKind::Bool,
                    Some(Kind::Str(value)) if value == "Number" => HistoryKind::Number,
                    Some(Kind::Str(value)) if value == "Str" => HistoryKind::Str,
                    _ => return Err(ReadError::Projection),
                };
                let Some(Kind::Str(timezone)) = row.get("tz") else {
                    return Err(ReadError::InvalidQuery("point requires history timezone"));
                };
                if timezone.len() > 128 {
                    return Err(ReadError::InvalidQuery("invalid history timezone"));
                }
                validate_zone(timezone)?;
                let unit = match row.get("unit") {
                    None if kind != HistoryKind::Number => None,
                    Some(Kind::Str(unit))
                        if kind == HistoryKind::Number
                            && unit.len() <= 128
                            && unit_for(unit).is_some() =>
                    {
                        Some(budget.copy_string(unit)?)
                    }
                    _ => return Err(ReadError::Projection),
                };
                let schema = HistorySchema {
                    kind,
                    unit,
                    timezone: budget.copy_string(timezone)?,
                };
                Ok((
                    schema,
                    graph.state(),
                    budget.copy_string(policy.scope_key())?,
                ))
            }) {
                return result;
            }
            // Do not block the runtime that delivers owner/caller cancellation.
            tokio::time::sleep(budget.wait_quantum()?).await;
        }
    }
    async fn revalidate(
        &self,
        principal: &Principal,
        metadata: &HistoryMetadata,
        scope: &str,
        budget: &mut Budget,
    ) -> Result<(), HistoryTerminal> {
        if budget.check().is_err() {
            return Err(HistoryTerminal::Interrupted(stop_reason(budget)));
        }
        match self.observe(principal, &metadata.id, budget).await {
            Ok((schema, state, current_scope)) => {
                if current_scope != scope {
                    Err(HistoryTerminal::Interrupted(HistoryReason::PolicyChanged))
                } else if state != metadata.graph || schema != metadata.schema {
                    Err(HistoryTerminal::Interrupted(HistoryReason::MetadataChanged))
                } else {
                    Ok(())
                }
            }
            Err(ReadError::Budget(_)) => Err(HistoryTerminal::Limited(HistoryReason::Work)),
            Err(ReadError::Cancelled | ReadError::Deadline) => {
                Err(HistoryTerminal::Interrupted(stop_reason(budget)))
            }
            Err(_) => Err(HistoryTerminal::Interrupted(HistoryReason::PolicyChanged)),
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn metadata(
        &self,
        request: &HistoryReadRequest,
        now: DateTime<FixedOffset>,
        schema: HistorySchema,
        graph: GraphState,
        start: haystack_core::kinds::HDateTime,
        end: haystack_core::kinds::HDateTime,
        provider: &ProviderHistoryMetadata,
        budget: &mut Budget,
    ) -> Result<HistoryMetadata, ReadError> {
        // Reserve the bounded metadata and ready-handle copy before strings.
        budget.charge(BudgetKind::Retained, 8192)?;
        let c = &provider.coverage;
        if provider.capabilities != HistoryCapabilities::BOUNDED_LIVE
            || (c.retained_count == 0) != (c.retained_start.is_none() && c.retained_end.is_none())
            || c.retained_start.is_some() != c.retained_end.is_some()
            || c.retained_start
                .zip(c.retained_end)
                .is_some_and(|(start, end)| {
                    start > end
                        || (c.retained_count == 1 && start != end)
                        || c.evicted_through.is_some_and(|through| through >= start)
                })
        {
            return Err(ReadError::InvalidQuery("invalid provider history metadata"));
        }
        let convert = |time: Option<DateTime<FixedOffset>>| {
            time.map(|time| point_time(time, &schema.timezone))
                .transpose()
        };
        let coverage = HistoryCoverage {
            retained_start: convert(c.retained_start)?,
            retained_end: convert(c.retained_end)?,
            retained_count: c.retained_count,
            evicted_through: convert(c.evicted_through)?,
        };
        let total_rows = self.limits.total_rows.min(budget.limits.max_rows);
        let total_bytes = self
            .limits
            .total_bytes
            .min(budget.limits.max_output_bytes.saturating_sub(128 * 1024) / 6);
        let bounds = HistoryBounds {
            batch_rows: self.limits.batch_rows.min(total_rows) as u64,
            batch_bytes: self.limits.batch_bytes.min(total_bytes) as u64,
            total_rows: total_rows as u64,
            total_bytes: total_bytes as u64,
            total_work: self.limits.total_work.min(budget.limits.max_work) as u64,
            response_bytes: budget.limits.max_output_bytes as u64,
        };
        Ok(HistoryMetadata {
            id: budget.copy_string(&request.id)?,
            requested_range: budget.copy_string(&request.range)?,
            evaluated_at: point_time(now, &schema.timezone)?,
            start,
            end,
            schema,
            capabilities: provider.capabilities,
            bounds,
            graph,
            policy_observation: rand::random(),
            history: provider.state,
            coverage,
        })
    }
}
// Conservative source-based reservations for the selected scalar-only H4
// codecs, charged before source copies. Per sample: the original and grid
// clone, JSON value tree or Zinc scalar/row strings, worst-case 6x escaping,
// output/string/Vec growth and fixed row nodes all fit within 64 times the
// 512-byte structural allowance plus string/unit bytes. Work reserves 32
// source traversals/escaped-output bytes. These are accounting bounds, not
// measurements of allocator usage. No reservation is refunded between stages.
const WIRE_SAMPLE_RETAINED: usize = 64;
const WIRE_SAMPLE_WORK: usize = 32;
fn reserve_wire_metadata(
    budget: &mut Budget,
    request: &HistoryReadRequest,
    schema: &HistorySchema,
) -> Result<(), ReadError> {
    // Fixed control nodes, containers and numeric/identity fields plus up to
    // eight timezone-bearing metadata timestamps and repeated dynamic text.
    let text = request
        .id
        .len()
        .saturating_add(request.range.len())
        .saturating_add(schema.unit.as_ref().map_or(0, String::len))
        .saturating_add(schema.timezone.len().saturating_mul(8));
    let reserve = (64usize * 1024).saturating_add(text.saturating_mul(128));
    budget.charge(BudgetKind::Retained, reserve)?;
    budget.charge(BudgetKind::Work, reserve)
}
fn valid_value(value: &Kind, schema: &HistorySchema) -> bool {
    match (value, schema.kind) {
        (Kind::NA, _) => true,
        (Kind::Bool(_), HistoryKind::Bool) | (Kind::Str(_), HistoryKind::Str) => true,
        (Kind::Number(number), HistoryKind::Number) => match &number.unit {
            None => !number.val.is_nan() || number.val.to_bits() == f64::NAN.to_bits(),
            Some(unit) => {
                !number.val.is_nan()
                    && schema
                        .unit
                        .as_ref()
                        .and_then(|unit| unit_for(unit))
                        .zip(unit_for(unit))
                        .is_some_and(|(point, sample)| point.name == sample.name)
            }
        },
        _ => false,
    }
}
fn budget_terminal(error: ReadError, budget: &Budget) -> HistoryTerminal {
    match error {
        ReadError::Cancelled | ReadError::Deadline => {
            HistoryTerminal::Interrupted(stop_reason(budget))
        }
        ReadError::Budget(BudgetKind::Retained | BudgetKind::Output) => {
            HistoryTerminal::Limited(HistoryReason::Bytes)
        }
        _ => HistoryTerminal::Limited(HistoryReason::Work),
    }
}
fn stop_reason(budget: &Budget) -> HistoryReason {
    if std::time::Instant::now() >= budget.deadline {
        HistoryReason::Deadline
    } else if budget
        .owner_cancel
        .as_ref()
        .is_some_and(CancellationToken::is_cancelled)
    {
        HistoryReason::OwnerStopped
    } else {
        HistoryReason::Cancelled
    }
}
async fn stopped(budget: &Budget) -> HistoryReason {
    tokio::select! { _ = budget.cancelled() => {}, _ = tokio::time::sleep_until(tokio::time::Instant::from_std(budget.deadline)) => {} }
    stop_reason(budget)
}
fn read_stop(reason: HistoryReason) -> ReadError {
    if reason == HistoryReason::Deadline {
        ReadError::Deadline
    } else {
        ReadError::Cancelled
    }
}
fn terminal_reason(terminal: HistoryTerminal) -> HistoryReason {
    match terminal {
        HistoryTerminal::Limited(reason)
        | HistoryTerminal::Interrupted(reason)
        | HistoryTerminal::Failed(reason) => reason,
        HistoryTerminal::Complete => HistoryReason::Provider,
    }
}
fn finish(
    reply: oneshot::Sender<HistoryBatch>,
    terminal: &watch::Sender<Option<HistoryTerminal>>,
    samples: Vec<HistorySample>,
    result: HistoryTerminal,
) {
    let _ = terminal.send(Some(result));
    let _ = reply.send(HistoryBatch {
        samples,
        terminal: Some(result),
    });
}
async fn finish_closed(
    session: &mut dyn HistorySession,
    reply: oneshot::Sender<HistoryBatch>,
    terminal_tx: &watch::Sender<Option<HistoryTerminal>>,
    samples: Vec<HistorySample>,
    terminal: HistoryTerminal,
    budget: &Budget,
) {
    let mut close = session.close();
    tokio::select! {
        result = &mut close => finish(reply, terminal_tx, samples, if result.is_ok() { terminal } else { HistoryTerminal::Failed(HistoryReason::Provider) }),
        reason = stopped(budget) => {
            finish(reply, terminal_tx, samples, HistoryTerminal::Interrupted(reason));
            let _ = close.await;
        }
    }
}
struct CancelOpen {
    token: CancellationToken,
    armed: bool,
}
impl Drop for CancelOpen {
    fn drop(&mut self) {
        if self.armed {
            self.token.cancel();
        }
    }
}

pub struct HistoryReadSession {
    pub metadata: HistoryMetadata,
    commands: mpsc::Sender<oneshot::Sender<HistoryBatch>>,
    terminal: watch::Receiver<Option<HistoryTerminal>>,
    done: watch::Receiver<bool>,
    cancel: CancellationToken,
    pending: Option<oneshot::Receiver<HistoryBatch>>,
}
impl HistoryReadSession {
    /// Exactly one requested batch, with no speculative fetch. `&mut self`
    /// prevents multiple outstanding pulls for the same session.
    pub async fn next(&mut self) -> HistoryBatch {
        if self.pending.is_none() {
            if let Some(terminal) = *self.terminal.borrow() {
                return HistoryBatch {
                    samples: vec![],
                    terminal: Some(terminal),
                };
            }
            let (reply, result) = oneshot::channel();
            if self.commands.try_send(reply).is_err() {
                return HistoryBatch {
                    samples: vec![],
                    terminal: Some(
                        self.terminal
                            .borrow()
                            .unwrap_or(HistoryTerminal::Failed(HistoryReason::Provider)),
                    ),
                };
            }
            self.pending = Some(result);
        }
        // Cancellation of the waiting `next` future leaves this receiver in the
        // handle, so a later pull observes the same batch without dropping rows.
        let result = self.pending.as_mut().expect("pending pull").await;
        self.pending = None;
        result.unwrap_or_else(|_| HistoryBatch {
            samples: vec![],
            terminal: Some(
                self.terminal
                    .borrow()
                    .unwrap_or(HistoryTerminal::Failed(HistoryReason::Provider)),
            ),
        })
    }
    pub async fn close(&mut self) {
        self.cancel.cancel();
        while !*self.done.borrow() {
            if self.done.changed().await.is_err() {
                break;
            }
        }
    }
    pub async fn collect(mut self) -> Result<HistoryReadResult, ReadError> {
        self.collect_borrowed().await
    }
    async fn collect_borrowed(&mut self) -> Result<HistoryReadResult, ReadError> {
        let mut samples = Vec::new();
        loop {
            let mut batch = self.next().await;
            samples.append(&mut batch.samples);
            if let Some(terminal) = batch.terminal {
                return Ok(HistoryReadResult {
                    metadata: self.metadata.clone(),
                    samples,
                    terminal,
                });
            }
        }
    }
}
impl Drop for HistoryReadSession {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

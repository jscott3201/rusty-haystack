//! Authorized entity commits backed by an explicitly ephemeral receipt store.
//! The provider protocol seam supports failure/uncertainty fixtures; it does not
//! claim database, disk, crash-recovery or multi-process commit qualification.
use crate::{
    BudgetKind, Principal, ReadAdmission, ReadContext, ReadError, ReadService, budget::Budget,
};
use haystack_core::{
    codecs::entity::{
        self, ChangesPage, EntityBatchRequest, EntityReceipt, MutationOutcome, OperationIdentity,
        ReceiptQualification, RejectionReason, UnknownCause,
    },
    graph::{BatchError, BatchLimits, GraphState, PreparedBatch, PreparedChange, SharedGraph},
};
use parking_lot::{Mutex, RwLock};
use std::{collections::HashMap, sync::Arc, time::Instant};
use tokio_util::sync::CancellationToken;

/// Immutable authorization rules, replaced through `MutationService::replace_policy`.
/// Callbacks are trusted, side-effect-free, quick and nonblocking. No callback runs under the graph
/// commit lock. Publication checks and holds the authorized policy generation.
/// Read permission never implies these write decisions.
pub trait MutationPolicy: Send + Sync + 'static {
    fn authorize_intent(&self, principal: &Principal, request: &EntityBatchRequest) -> bool;
    fn authorize_change(&self, principal: &Principal, change: &PreparedChange) -> bool;
    /// Reauthorization is independent of current entity existence. The retained
    /// intent is available even after removal; missing receipts supply `None`.
    fn authorize_reconcile(
        &self,
        principal: &Principal,
        identity: &OperationIdentity,
        original: Option<&EntityBatchRequest>,
    ) -> bool;
}
/// Deliberate opt-in for trusted embedding. This is separate from read AllowAll.
pub struct AllowAllMutations;
impl MutationPolicy for AllowAllMutations {
    fn authorize_intent(&self, _: &Principal, _: &EntityBatchRequest) -> bool {
        true
    }
    fn authorize_change(&self, _: &Principal, _: &PreparedChange) -> bool {
        true
    }
    fn authorize_reconcile(
        &self,
        _: &Principal,
        _: &OperationIdentity,
        _: Option<&EntityBatchRequest>,
    ) -> bool {
        true
    }
}
#[derive(Debug, Clone)]
pub struct MutationLimits {
    pub batch: BatchLimits,
    pub receipt_capacity: usize,
    pub receipt_bytes: usize,
    /// Maximum typed payload bytes for a whole feed page, up to typed-v1's limit.
    pub page_bytes: usize,
}
impl Default for MutationLimits {
    fn default() -> Self {
        Self {
            batch: BatchLimits::default(),
            receipt_capacity: 1024,
            receipt_bytes: 64 * 1024 * 1024,
            page_bytes: entity::MAX_PAYLOAD_BYTES,
        }
    }
}
impl MutationLimits {
    fn validate(&self) -> Result<(), ReadError> {
        if self.batch.max_operations == 0
            || self.batch.max_operations > entity::MAX_OPERATIONS
            || self.batch.max_work == 0
            || self.batch.max_work > 16_000_000
            || self.batch.max_retained_bytes == 0
            || self.batch.max_retained_bytes > 64 * 1024 * 1024
            || self.batch.max_value_depth == 0
            || self.batch.max_value_depth > 48
            || self.receipt_capacity == 0
            || self.receipt_capacity > 100_000
            || self.receipt_bytes == 0
            || self.receipt_bytes > 256 * 1024 * 1024
            || self.page_bytes < 8192
            || self.page_bytes > entity::MAX_PAYLOAD_BYTES
        {
            return Err(ReadError::InvalidLimits);
        }
        Ok(())
    }
}
#[derive(Clone, Hash, PartialEq, Eq)]
enum PrincipalKey {
    Anonymous,
    Authenticated(String),
    Embedding(String),
}
impl From<&Principal> for PrincipalKey {
    fn from(p: &Principal) -> Self {
        match p {
            Principal::Anonymous => Self::Anonymous,
            Principal::Authenticated { subject, .. } => Self::Authenticated(subject.clone()),
            Principal::TrustedEmbedding { subject } => Self::Embedding(subject.clone()),
        }
    }
}
#[derive(Clone, Hash, PartialEq, Eq)]
struct Binding {
    principal: PrincipalKey,
    identity: OperationIdentity,
}
#[derive(Clone)]
struct Record {
    canonical: Arc<[u8]>,
    request: Arc<EntityBatchRequest>,
    outcome: MutationOutcome,
}
#[derive(Default)]
struct Receipts {
    entries: HashMap<Binding, Record>,
    bytes: usize,
}
struct Store {
    graph: SharedGraph,
    dataset: [u8; 16],
    receipts: Mutex<Receipts>,
}
/// Retain this handle when recreating a service around the same in-memory state.
/// Recreating the store creates a new dataset; restarting the graph creates a
/// new incarnation. This handle provides no persistence beyond its process.
#[derive(Clone)]
pub struct EphemeralMutationStore {
    inner: Arc<Store>,
}
impl EphemeralMutationStore {
    pub fn new(graph: SharedGraph) -> Self {
        Self {
            inner: Arc::new(Store {
                graph,
                dataset: rand::random(),
                receipts: Mutex::new(Receipts::default()),
            }),
        }
    }
    pub fn graph(&self) -> SharedGraph {
        self.inner.graph.clone()
    }
    pub fn dataset(&self) -> [u8; 16] {
        self.inner.dataset
    }
    /// Exact retained entity authority, not merely another store on the same graph.
    pub fn same_store(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
    pub fn receipt_count(&self) -> usize {
        self.inner.receipts.lock().entries.len()
    }
}

/// A selected provider receives a fully authorized and capacity-reserved plan.
/// It must call `publish` at most once, or return `reject` before any effect.
/// Dropping a plan leaves a pending identity, requiring reconciliation; it does
/// not permit replay. A lost acknowledgement is `Unknown`, never `Rejected`.
/// This seam only qualifies the in-memory/provider protocol, not durable storage.
pub trait MutationProvider: Send + Sync + 'static {
    fn qualification(&self) -> ReceiptQualification;
    fn commit(&self, prepared: PreparedMutation) -> MutationOutcome;
}
pub struct EphemeralProvider;
impl MutationProvider for EphemeralProvider {
    fn qualification(&self) -> ReceiptQualification {
        ReceiptQualification::EphemeralMemory
    }
    fn commit(&self, prepared: PreparedMutation) -> MutationOutcome {
        prepared.publish()
    }
}

/// Opaque authorized publication. Callbacks may delay or drop this object to
/// exercise failure protocols, but cannot alter its rows, identity or receipt.
/// A retained plan owns admission and lifecycle work until published, rejected
/// or dropped. Publication rejects after policy replacement or owner sealing.
pub struct PreparedMutation {
    store: Arc<Store>,
    binding: Binding,
    plan: PreparedBatch,
    receipt: EntityReceipt,
    deadline: Instant,
    cancel: CancellationToken,
    owner_cancel: Option<CancellationToken>,
    owner_sealed: Option<CancellationToken>,
    policy: Arc<RwLock<PolicyVersion>>,
    generation: Arc<()>,
    _lease: Option<Arc<crate::service::WorkLease>>,
}
impl PreparedMutation {
    pub fn identity(&self) -> &OperationIdentity {
        &self.receipt.identity
    }
    pub fn before(&self) -> GraphState {
        self.plan.state()
    }
    fn stop(&self) -> Option<RejectionReason> {
        if Instant::now() >= self.deadline {
            Some(RejectionReason::Deadline)
        } else if self.cancel.is_cancelled()
            || self
                .owner_cancel
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
            || self
                .owner_sealed
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
        {
            Some(RejectionReason::Cancelled)
        } else {
            None
        }
    }
    /// Guaranteed no effect because this still owns the unconsumed plan.
    pub fn reject(self, reason: RejectionReason) -> MutationOutcome {
        let result = MutationOutcome::Rejected {
            identity: self.receipt.identity.clone(),
            reason,
        };
        // Rejected attempts may be retried with a new operation identity; retain
        // the binding so this identity never changes meaning.
        if let Some(record) = self.store.receipts.lock().entries.get_mut(&self.binding) {
            record.outcome = result.clone();
        }
        result
    }
    pub fn publish(self) -> MutationOutcome {
        if let Some(reason) = self.stop() {
            return self.reject(reason);
        }
        let authority = self.policy.clone();
        let policy = loop {
            if let Some(reason) = self.stop() {
                return self.reject(reason);
            }
            if let Some(policy) = authority.try_read_for(std::time::Duration::from_millis(5)) {
                break policy;
            }
        };
        if !Arc::ptr_eq(&policy.generation, &self.generation) {
            drop(policy);
            return self.reject(RejectionReason::Conflict);
        }
        let mut policy = Some(policy);
        let store = self.store.clone();
        let records = loop {
            if let Some(reason) = self.stop() {
                return self.reject(reason);
            }
            if let Some(records) = store
                .receipts
                .try_lock_for(std::time::Duration::from_millis(5))
            {
                break records;
            }
        };
        let mut records = Some(records);
        let committed = MutationOutcome::Committed(self.receipt.clone());
        let response = committed.clone();
        let identity = self.receipt.identity.clone();
        let mut committed = Some(committed);
        let mut response = Some(response);
        let mut plan = Some(self.plan);
        loop {
            let reason = if Instant::now() >= self.deadline {
                Some(RejectionReason::Deadline)
            } else if self.cancel.is_cancelled()
                || self
                    .owner_cancel
                    .as_ref()
                    .is_some_and(CancellationToken::is_cancelled)
                || self
                    .owner_sealed
                    .as_ref()
                    .is_some_and(CancellationToken::is_cancelled)
            {
                Some(RejectionReason::Cancelled)
            } else {
                None
            };
            if let Some(reason) = reason {
                let rejected = MutationOutcome::Rejected { identity, reason };
                records
                    .as_mut()
                    .expect("receipt publication lock")
                    .entries
                    .get_mut(&self.binding)
                    .expect("reserved receipt")
                    .outcome = rejected.clone();
                return rejected;
            }
            let published = store
                .graph
                .write_for(std::time::Duration::from_millis(5), |graph| {
                    // The final cancellation/deadline check occurs after lock wait.
                    if Instant::now() >= self.deadline {
                        return Err(RejectionReason::Deadline);
                    }
                    if self.cancel.is_cancelled()
                        || self
                            .owner_cancel
                            .as_ref()
                            .is_some_and(CancellationToken::is_cancelled)
                        || self
                            .owner_sealed
                            .as_ref()
                            .is_some_and(CancellationToken::is_cancelled)
                    {
                        return Err(RejectionReason::Cancelled);
                    }
                    graph
                        .apply_prepared(plan.take().expect("one publication"))
                        .map_err(batch_reason)?;
                    // Allocation/reservation and receipt construction preceded effects.
                    // Publication is a move into an existing slot under the SAME lock.
                    records
                        .as_mut()
                        .expect("receipt publication lock")
                        .entries
                        .get_mut(&self.binding)
                        .expect("reserved receipt")
                        .outcome = committed.take().expect("one receipt");
                    // Reentrant notification wakers run after graph unlock;
                    // release receipt and policy ownership before notification.
                    drop(records.take());
                    drop(policy.take());
                    Ok(response.take().expect("one response"))
                });
            match published {
                None => continue,
                Some(Ok(outcome)) => return outcome,
                Some(Err(reason)) => {
                    let outcome = MutationOutcome::Rejected { identity, reason };
                    records
                        .as_mut()
                        .expect("receipt publication lock")
                        .entries
                        .get_mut(&self.binding)
                        .expect("reserved receipt")
                        .outcome = outcome.clone();
                    return outcome;
                }
            }
        }
    }
}

#[derive(Clone)]
pub struct MutationService {
    pub(crate) inner: Arc<MutationInner>,
}
struct PolicyVersion {
    rules: Arc<dyn MutationPolicy>,
    generation: Arc<()>,
}
impl PolicyVersion {
    fn new(rules: Arc<dyn MutationPolicy>) -> Self {
        Self {
            rules,
            generation: Arc::new(()),
        }
    }
}
pub(crate) struct MutationInner {
    pub(crate) reads: ReadService,
    store: EphemeralMutationStore,
    policy: Arc<RwLock<PolicyVersion>>,
    limits: MutationLimits,
    provider: Arc<dyn MutationProvider>,
    pub(crate) cursor_key: [u8; 32],
    pub(crate) started: Instant,
}
impl MutationService {
    pub fn new(
        reads: ReadService,
        store: EphemeralMutationStore,
        policy: Arc<dyn MutationPolicy>,
        limits: MutationLimits,
    ) -> Result<Self, ReadError> {
        Self::with_provider(reads, store, policy, limits, Arc::new(EphemeralProvider))
    }
    pub fn with_provider(
        reads: ReadService,
        store: EphemeralMutationStore,
        policy: Arc<dyn MutationPolicy>,
        limits: MutationLimits,
        provider: Arc<dyn MutationProvider>,
    ) -> Result<Self, ReadError> {
        limits.validate()?;
        if !reads.graph().shares_storage(&store.graph()) {
            return Err(ReadError::Forbidden);
        }
        Ok(Self {
            inner: Arc::new(MutationInner {
                reads,
                store,
                policy: Arc::new(RwLock::new(PolicyVersion::new(policy))),
                limits,
                provider,
                cursor_key: rand::random(),
                started: Instant::now(),
            }),
        })
    }
    pub fn read_service(&self) -> ReadService {
        self.inner.reads.clone()
    }
    pub fn store(&self) -> EphemeralMutationStore {
        self.inner.store.clone()
    }
    pub fn limits(&self) -> &MutationLimits {
        &self.inner.limits
    }
    /// Waits for current authorization/publication decisions, then replaces
    /// rules atomically. Deferred plans from an earlier generation must reject.
    /// Rules must be immutable; do not mutate behind Arc.
    pub fn replace_policy(&self, policy: Arc<dyn MutationPolicy>) {
        *self.inner.policy.write() = PolicyVersion::new(policy);
    }
    pub async fn submit(
        &self,
        context: ReadContext,
        request: EntityBatchRequest,
    ) -> Result<MutationOutcome, ReadError> {
        let admission = self.inner.reads.begin(context).await?;
        self.submit_admitted(admission, request).await
    }
    pub async fn submit_admitted(
        &self,
        admission: ReadAdmission,
        request: EntityBatchRequest,
    ) -> Result<MutationOutcome, ReadError> {
        if !admission.belongs_to(&self.inner.reads) {
            return Err(ReadError::Forbidden);
        }
        entity::encode(&request.identity)
            .map_err(|_| ReadError::InvalidQuery("invalid operation identity"))?;
        let identity = request.identity.clone();
        let inner = self.inner.clone();
        // Once a worker may have started, timeout/cancellation/panic is uncertain
        // to its caller, even though the eventual worker may reject before effect.
        Ok(admission
            .run_task(move |principal, budget| Ok(inner.submit(principal, request, budget)))
            .await
            .unwrap_or(MutationOutcome::Unknown {
                identity,
                cause: UnknownCause::Provider,
            }))
    }
    pub async fn reconcile(
        &self,
        context: ReadContext,
        identity: OperationIdentity,
    ) -> Result<MutationOutcome, ReadError> {
        let admission = self.inner.reads.begin(context).await?;
        self.reconcile_admitted(admission, identity).await
    }
    pub async fn reconcile_admitted(
        &self,
        admission: ReadAdmission,
        identity: OperationIdentity,
    ) -> Result<MutationOutcome, ReadError> {
        if !admission.belongs_to(&self.inner.reads) {
            return Err(ReadError::Forbidden);
        }
        let inner = self.inner.clone();
        admission
            .run_task(move |principal, budget| inner.reconcile(&principal, identity, budget))
            .await
    }
}
impl MutationInner {
    pub(crate) fn submit(
        &self,
        principal: Principal,
        request: EntityBatchRequest,
        budget: &mut Budget,
    ) -> MutationOutcome {
        let identity = request.identity.clone();
        match self.prepare_and_submit(&principal, request, budget) {
            Ok(result) => result,
            Err(reason) => MutationOutcome::Rejected { identity, reason },
        }
    }
    fn prepare_and_submit(
        &self,
        principal: &Principal,
        request: EntityBatchRequest,
        budget: &mut Budget,
    ) -> Result<MutationOutcome, RejectionReason> {
        self.principal_bounds(principal, budget)
            .map_err(read_reason)?;
        let source = request
            .source_bytes()
            .map_err(|_| RejectionReason::Invalid)?;
        budget
            .charge(
                BudgetKind::Retained,
                source.saturating_mul(8).saturating_add(8192),
            )
            .map_err(read_reason)?;
        let canonical = entity::encode(&request).map_err(|_| RejectionReason::Invalid)?;
        budget
            .charge(BudgetKind::Retained, canonical.len().saturating_mul(8))
            .map_err(read_reason)?;
        let policy = loop {
            if let Some(p) = self
                .policy
                .try_read_for(budget.wait_quantum().map_err(read_reason)?)
            {
                break p;
            }
        };
        if !policy.rules.authorize_intent(principal, &request) {
            return Err(RejectionReason::Forbidden);
        }
        budget.check().map_err(read_reason)?;
        if request.identity.dataset != self.store.dataset() {
            return Err(RejectionReason::Conflict);
        }
        let binding = Binding {
            principal: PrincipalKey::from(principal),
            identity: request.identity.clone(),
        };
        // Binding lookup precedes expected-revision/incarnation conflict checks.
        if let Some(record) = self.lookup(&binding, budget).map_err(read_reason)? {
            if !policy.rules.authorize_reconcile(
                principal,
                &request.identity,
                Some(&record.request),
            ) {
                return Err(RejectionReason::Forbidden);
            }
            return if record.canonical.as_ref() == canonical {
                Ok(record.outcome)
            } else {
                Err(RejectionReason::Conflict)
            };
        }
        let graph = self.store.graph();
        let mut batch_limits = self.limits.batch;
        batch_limits.max_retained_bytes = batch_limits
            .max_retained_bytes
            .min(budget.retained_remaining() / 4);
        let plan = loop {
            let prepared = graph.read_for(budget.wait_quantum().map_err(read_reason)?, |g| {
                if g.incarnation() != request.identity.incarnation {
                    return Err(BatchError::Conflict);
                }
                g.prepare_batch(request.expected_revision, &request.operations, batch_limits)
            });
            if let Some(prepared) = prepared {
                break prepared.map_err(batch_reason)?;
            }
        };
        budget.check().map_err(read_reason)?;
        budget
            .charge(BudgetKind::Retained, plan.retained_bytes())
            .map_err(read_reason)?;
        for change in plan.changes() {
            if !policy.rules.authorize_change(principal, change) {
                return Err(RejectionReason::Forbidden);
            }
            budget.check().map_err(read_reason)?;
        }
        self.preflight_feed(&plan, budget).map_err(read_reason)?;
        let receipt = EntityReceipt {
            identity: request.identity.clone(),
            before_revision: plan.state().revision,
            after_revision: plan.after_revision(),
            span: plan.span(),
            qualification: self.provider.qualification(),
        };
        entity::encode(&MutationOutcome::Committed(receipt.clone()))
            .map_err(|_| RejectionReason::Limit)?;
        let bytes = canonical
            .len()
            .saturating_mul(32)
            .saturating_add(principal.bytes().saturating_mul(2))
            .saturating_add(8192);
        let mut table = loop {
            if let Some(t) = self
                .store
                .inner
                .receipts
                .try_lock_for(budget.wait_quantum().map_err(read_reason)?)
            {
                break t;
            }
        };
        // A concurrent identical submission may have bound the identity while
        // this one prepared. Never overwrite or reapply that pending operation.
        if let Some(record) = table.entries.get(&binding).cloned() {
            drop(table);
            if !policy.rules.authorize_reconcile(
                principal,
                &request.identity,
                Some(&record.request),
            ) {
                return Err(RejectionReason::Forbidden);
            }
            return if record.canonical.as_ref() == canonical {
                Ok(record.outcome)
            } else {
                Err(RejectionReason::Conflict)
            };
        }
        if table.entries.len() >= self.limits.receipt_capacity
            || bytes > self.limits.receipt_bytes.saturating_sub(table.bytes)
        {
            return Err(RejectionReason::Capacity);
        }
        table
            .entries
            .try_reserve(1)
            .map_err(|_| RejectionReason::Capacity)?;
        budget.check().map_err(read_reason)?;
        let pending = MutationOutcome::Unknown {
            identity: request.identity.clone(),
            cause: UnknownCause::Pending,
        };
        table.entries.insert(
            binding.clone(),
            Record {
                canonical: canonical.into(),
                request: Arc::new(request),
                outcome: pending,
            },
        );
        table.bytes += bytes;
        drop(table);
        let identity = receipt.identity.clone();
        let prepared = PreparedMutation {
            store: self.store.inner.clone(),
            binding: binding.clone(),
            plan,
            receipt,
            deadline: budget.deadline,
            cancel: budget.cancel.clone(),
            owner_cancel: budget.owner_cancel.clone(),
            owner_sealed: budget.owner_sealed.clone(),
            policy: self.policy.clone(),
            generation: policy.generation.clone(),
            _lease: budget.lease.clone(),
        };
        // Providers may retain the plan after returning. Publication must
        // reacquire and check this generation, and owns the execution lease.
        // Release authorization first to avoid nested reads behind a writer.
        drop(policy);
        let outcome = self.provider.commit(prepared);
        if outcome.identity() != &identity {
            return Ok(MutationOutcome::Unknown {
                identity,
                cause: UnknownCause::InvalidAcknowledgement,
            });
        }
        if !matches!(outcome, MutationOutcome::Unknown { .. }) {
            let stored = self.lookup(&binding, budget).ok().flatten();
            if !stored.is_some_and(|record| record.outcome == outcome) {
                return Ok(MutationOutcome::Unknown {
                    identity,
                    cause: UnknownCause::InvalidAcknowledgement,
                });
            }
        }
        Ok(outcome)
    }
    fn principal_bounds(&self, principal: &Principal, budget: &Budget) -> Result<(), ReadError> {
        budget.check()?;
        if principal.bytes() > budget.limits.max_input_bytes
            || matches!(principal,Principal::Authenticated{permissions,..} if permissions.len()>budget.limits.max_ids)
        {
            return Err(ReadError::Budget(BudgetKind::Input));
        }
        Ok(())
    }
    fn lookup(&self, binding: &Binding, budget: &Budget) -> Result<Option<Record>, ReadError> {
        loop {
            if let Some(table) = self
                .store
                .inner
                .receipts
                .try_lock_for(budget.wait_quantum()?)
            {
                return Ok(table.entries.get(binding).cloned());
            }
        }
    }
    pub(crate) fn reconcile(
        &self,
        principal: &Principal,
        identity: OperationIdentity,
        budget: &mut Budget,
    ) -> Result<MutationOutcome, ReadError> {
        self.principal_bounds(principal, budget)?;
        entity::encode(&identity)
            .map_err(|_| ReadError::InvalidQuery("invalid receipt identity"))?;
        let policy = loop {
            if let Some(p) = self.policy.try_read_for(budget.wait_quantum()?) {
                break p;
            }
        };
        let binding = Binding {
            principal: PrincipalKey::from(principal),
            identity: identity.clone(),
        };
        let record = self.lookup(&binding, budget)?;
        if !policy.rules.authorize_reconcile(
            principal,
            &identity,
            record.as_ref().map(|r| r.request.as_ref()),
        ) {
            return Err(ReadError::Forbidden);
        }
        budget.check()?;
        Ok(record.map_or(
            MutationOutcome::Unknown {
                identity,
                cause: UnknownCause::Missing,
            },
            |r| r.outcome,
        ))
    }
    fn preflight_feed(&self, plan: &PreparedBatch, budget: &mut Budget) -> Result<(), ReadError> {
        if plan.diffs().is_empty() {
            return Ok(());
        }
        let bytes = plan
            .diffs()
            .iter()
            .map(|d| d.retained_bytes())
            .sum::<usize>();
        budget.charge(BudgetKind::Retained, bytes.saturating_mul(4))?;
        let page = ChangesPage {
            dataset: self.store.dataset(),
            incarnation: plan.state().incarnation,
            head: plan.after_revision(),
            floor: plan.state().revision,
            position: plan.after_revision(),
            cursor: "x".repeat(1024),
            complete: true,
            changes: plan.diffs().iter().map(crate::feed::raw_diff).collect(),
        };
        let encoded = entity::encode(&page).map_err(|_| ReadError::Budget(BudgetKind::Output))?;
        if encoded.len() > self.limits.page_bytes {
            return Err(ReadError::Budget(BudgetKind::Output));
        }
        Ok(())
    }
}
fn batch_reason(error: BatchError) -> RejectionReason {
    match error {
        BatchError::Conflict => RejectionReason::Conflict,
        BatchError::Limit => RejectionReason::Limit,
        _ => RejectionReason::Invalid,
    }
}
fn read_reason(error: ReadError) -> RejectionReason {
    match error {
        ReadError::Forbidden => RejectionReason::Forbidden,
        ReadError::Capacity => RejectionReason::Capacity,
        ReadError::Cancelled => RejectionReason::Cancelled,
        ReadError::Deadline => RejectionReason::Deadline,
        _ => RejectionReason::Limit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haystack_core::{
        data::HDict,
        graph::{EntityGraph, EntityOperation},
        kinds::{HRef, Kind},
    };
    use std::{
        future::Future,
        sync::atomic::{AtomicBool, Ordering},
        task::{Context, Poll, Wake, Waker},
        time::Duration,
    };
    struct Observer {
        store: EphemeralMutationStore,
        policy: Arc<RwLock<PolicyVersion>>,
        released: AtomicBool,
    }
    impl Wake for Observer {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            let receipts = self.store.inner.receipts.try_lock();
            let graph = self.store.graph().read_for(Duration::ZERO, |g| g.version());
            let policy = self.policy.try_write();
            self.released.store(
                receipts.is_some() && graph.is_some() && policy.is_some(),
                Ordering::SeqCst,
            );
        }
    }
    #[tokio::test]
    async fn notification_waker_can_observe_graph_receipt_and_policy_without_locks_held() {
        let graph = SharedGraph::new(EntityGraph::new());
        let reads = ReadService::new(
            graph.clone(),
            Arc::new(crate::AllowAll),
            crate::ReadLimits::default(),
        )
        .unwrap();
        let store = EphemeralMutationStore::new(graph.clone());
        let service = MutationService::new(
            reads,
            store.clone(),
            Arc::new(AllowAllMutations),
            MutationLimits::default(),
        )
        .unwrap();
        let observer = Arc::new(Observer {
            store,
            policy: service.inner.policy.clone(),
            released: AtomicBool::new(false),
        });
        let waker = Waker::from(observer.clone());
        let mut context = Context::from_waker(&waker);
        let mut receiver = graph.subscribe();
        let received = receiver.recv();
        tokio::pin!(received);
        assert!(matches!(
            received.as_mut().poll(&mut context),
            Poll::Pending
        ));
        let mut row = HDict::new();
        row.set("id", Kind::Ref(HRef::from_val("a")));
        let request = EntityBatchRequest {
            identity: OperationIdentity {
                operation_id: "waker".into(),
                dataset: service.store().dataset(),
                incarnation: graph.state().incarnation,
            },
            expected_revision: 0,
            operations: vec![EntityOperation::Add(row)],
        };
        assert!(matches!(
            service
                .submit(
                    ReadContext::with_timeout(Principal::Anonymous, Duration::from_secs(2)),
                    request
                )
                .await
                .unwrap(),
            MutationOutcome::Committed(_)
        ));
        assert!(observer.released.load(Ordering::SeqCst));
    }
}

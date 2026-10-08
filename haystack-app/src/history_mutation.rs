//! Authorized one-point history publication and retained operation receipts.
//! The selected authority is ephemeral; provider fixtures cannot claim disk or
//! process-crash durability. Entity and history transaction domains stay separate.
use crate::{
    BudgetKind, CancellationToken, HisItem, HisStore, HistoryService, Principal, ReadAdmission,
    ReadContext, ReadError, ReadService,
    budget::Budget,
    history_store::{HistoryChangeRecord, Series, trim},
};
use haystack_core::{
    codecs::{
        history::{HistorySchema, HistoryState, valid_value},
        history_mutation::*,
    },
    graph::GraphState,
    kinds::offset_at,
};
use parking_lot::RwLock;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

/// Immutable, quick, nonblocking authorization callbacks. No callback executes
/// under graph/point/publication locks. Read permission never implies write
/// permission. Replacement revokes delayed plans from the previous generation.
pub trait HistoryMutationPolicy: Send + Sync + 'static {
    fn authorize_intent(&self, principal: &Principal, request: &HistoryWriteRequest) -> bool;
    fn authorize_schema(
        &self,
        principal: &Principal,
        request: &HistoryWriteRequest,
        schema: &HistorySchema,
    ) -> bool;
    /// Reauthorize original retained intent independently of current point
    /// existence/schema. A missing receipt supplies None and proves nothing about effect.
    fn authorize_reconcile(
        &self,
        principal: &Principal,
        identity: &HistoryOperationIdentity,
        original: Option<&HistoryWriteRequest>,
    ) -> bool;
}
/// Deliberate trusted embedding opt-in, separate from all read/entity policies.
pub struct AllowAllHistoryMutations;
impl HistoryMutationPolicy for AllowAllHistoryMutations {
    fn authorize_intent(&self, _: &Principal, _: &HistoryWriteRequest) -> bool {
        true
    }
    fn authorize_schema(&self, _: &Principal, _: &HistoryWriteRequest, _: &HistorySchema) -> bool {
        true
    }
    fn authorize_reconcile(
        &self,
        _: &Principal,
        _: &HistoryOperationIdentity,
        _: Option<&HistoryWriteRequest>,
    ) -> bool {
        true
    }
}
#[derive(Clone)]
pub struct HistoryWriteCapability {
    pub store: HisStore,
    pub qualification: HistoryReceiptQualification,
}
#[derive(Debug, Clone)]
pub struct HistoryMutationLimits {
    pub max_samples: usize,
    pub max_source_bytes: usize,
    /// Bound existing-series traversal as well as incoming sorting/merging.
    pub max_series_samples: usize,
    pub max_prepared_bytes: usize,
}
impl Default for HistoryMutationLimits {
    fn default() -> Self {
        Self {
            max_samples: 128,
            max_source_bytes: 65_536,
            max_series_samples: 100_000,
            max_prepared_bytes: 8 * 1024 * 1024,
        }
    }
}
impl HistoryMutationLimits {
    fn validate(&self) -> Result<(), ReadError> {
        if self.max_samples == 0
            || self.max_samples > MAX_SAMPLES
            || self.max_source_bytes == 0
            || self.max_source_bytes > MAX_SOURCE_BYTES
            || self.max_series_samples == 0
            || self.max_series_samples > 1_000_000
            || self.max_prepared_bytes == 0
            || self.max_prepared_bytes > 64 * 1024 * 1024
        {
            Err(ReadError::InvalidLimits)
        } else {
            Ok(())
        }
    }
}
#[derive(Clone, Hash, PartialEq, Eq)]
enum PrincipalKey {
    Anonymous,
    Authenticated(String),
    Embedding(String),
}
impl From<&Principal> for PrincipalKey {
    fn from(principal: &Principal) -> Self {
        match principal {
            Principal::Anonymous => Self::Anonymous,
            Principal::Authenticated { subject, .. } => Self::Authenticated(subject.clone()),
            Principal::TrustedEmbedding { subject } => Self::Embedding(subject.clone()),
        }
    }
}
#[derive(Clone, Hash, PartialEq, Eq)]
pub(crate) struct HistoryBinding {
    principal: PrincipalKey,
    identity: HistoryOperationIdentity,
}
#[derive(Clone)]
pub(crate) struct HistoryRecord {
    canonical: Arc<[u8]>,
    request: Arc<HistoryWriteRequest>,
    pub(crate) outcome: HistoryWriteOutcome,
}
struct PolicyVersion {
    rules: Arc<dyn HistoryMutationPolicy>,
    generation: Arc<()>,
}
impl PolicyVersion {
    fn new(rules: Arc<dyn HistoryMutationPolicy>) -> Self {
        Self {
            rules,
            generation: Arc::new(()),
        }
    }
}
#[derive(Clone)]
pub struct HistoryMutationService {
    pub(crate) inner: Arc<HistoryMutationInner>,
}
pub(crate) struct HistoryMutationInner {
    pub(crate) history: HistoryService,
    store: HisStore,
    qualification: HistoryReceiptQualification,
    policy: Arc<RwLock<PolicyVersion>>,
    limits: HistoryMutationLimits,
}
impl HistoryMutationService {
    /// Select the exact history provider/authority already used by this read
    /// service. Read-only providers cannot opt in through an unrelated store.
    pub fn new(
        history: HistoryService,
        policy: Arc<dyn HistoryMutationPolicy>,
        limits: HistoryMutationLimits,
    ) -> Result<Self, ReadError> {
        limits.validate()?;
        if history.read_service().limits().max_output_bytes < MAX_RECEIPT_BYTES {
            return Err(ReadError::InvalidLimits);
        }
        let capability = history
            .provider()
            .history_write_capability()
            .ok_or(ReadError::Unavailable)?;
        Ok(Self {
            inner: Arc::new(HistoryMutationInner {
                history,
                store: capability.store,
                qualification: capability.qualification,
                policy: Arc::new(RwLock::new(PolicyVersion::new(policy))),
                limits,
            }),
        })
    }
    pub fn history_service(&self) -> &HistoryService {
        &self.inner.history
    }
    pub fn read_service(&self) -> &ReadService {
        self.inner.history.read_service()
    }
    pub fn store(&self) -> HisStore {
        self.inner.store.clone()
    }
    pub fn limits(&self) -> &HistoryMutationLimits {
        &self.inner.limits
    }
    pub fn replace_policy(&self, policy: Arc<dyn HistoryMutationPolicy>) {
        *self.inner.policy.write() = PolicyVersion::new(policy);
    }
    pub async fn submit(
        &self,
        context: ReadContext,
        request: HistoryWriteRequest,
    ) -> Result<HistoryWriteOutcome, ReadError> {
        self.submit_admitted(self.read_service().begin(context).await?, request)
            .await
    }
    pub async fn submit_admitted(
        &self,
        admission: ReadAdmission,
        request: HistoryWriteRequest,
    ) -> Result<HistoryWriteOutcome, ReadError> {
        if !admission.belongs_to(self.read_service()) {
            return Err(ReadError::Forbidden);
        }
        validate_identity(&request.identity)
            .map_err(|_| ReadError::InvalidQuery("invalid history operation identity"))?;
        let identity = request.identity.clone();
        let service = self.inner.clone();
        Ok(admission
            .run_task(move |principal, budget| Ok(service.submit(principal, request, budget)))
            .await
            .unwrap_or(HistoryWriteOutcome::Unknown {
                identity,
                cause: HistoryWriteUnknown::Provider,
            }))
    }
    pub async fn reconcile(
        &self,
        context: ReadContext,
        identity: HistoryOperationIdentity,
    ) -> Result<HistoryWriteOutcome, ReadError> {
        self.reconcile_admitted(self.read_service().begin(context).await?, identity)
            .await
    }
    pub async fn reconcile_admitted(
        &self,
        admission: ReadAdmission,
        identity: HistoryOperationIdentity,
    ) -> Result<HistoryWriteOutcome, ReadError> {
        if !admission.belongs_to(self.read_service()) {
            return Err(ReadError::Forbidden);
        }
        let service = self.inner.clone();
        admission
            .run_task(move |principal, budget| service.reconcile(&principal, identity, budget))
            .await
    }
}
struct PreparedSeries {
    point: Arc<RwLock<Series>>,
    graph: GraphState,
    next: Series,
    receipt: HistoryWriteReceipt,
    change: HistoryChangeRecord,
}
impl HistoryMutationInner {
    pub(crate) fn submit(
        &self,
        principal: Principal,
        request: HistoryWriteRequest,
        budget: &mut Budget,
    ) -> HistoryWriteOutcome {
        let identity = request.identity.clone();
        match self.prepare_submit(&principal, request, budget) {
            Ok(outcome) => outcome,
            Err(reason) => HistoryWriteOutcome::Rejected { identity, reason },
        }
    }
    fn principal_bounds(&self, principal: &Principal, budget: &Budget) -> Result<(), ReadError> {
        budget.check()?;
        if principal.bytes() > budget.limits.max_input_bytes
            || matches!(principal, Principal::Authenticated { permissions, .. } if permissions.len() > budget.limits.max_ids)
        {
            Err(ReadError::Budget(BudgetKind::Input))
        } else {
            Ok(())
        }
    }
    fn lookup(
        &self,
        binding: &HistoryBinding,
        budget: &Budget,
    ) -> Result<Option<HistoryRecord>, ReadError> {
        loop {
            if let Some(state) = self.store.inner.state.try_lock_for(budget.wait_quantum()?) {
                return Ok(state.receipts.get(binding).cloned());
            }
        }
    }
    fn recognized(
        &self,
        policy: &PolicyVersion,
        principal: &Principal,
        record: HistoryRecord,
        canonical: &[u8],
    ) -> Result<HistoryWriteOutcome, HistoryWriteRejection> {
        if !policy.rules.authorize_reconcile(
            principal,
            &record.request.identity,
            Some(&record.request),
        ) {
            return Err(HistoryWriteRejection::Forbidden);
        }
        if record.canonical.as_ref() != canonical {
            return Err(HistoryWriteRejection::Conflict);
        }
        Ok(record.outcome)
    }
    fn prepare_submit(
        &self,
        principal: &Principal,
        request: HistoryWriteRequest,
        budget: &mut Budget,
    ) -> Result<HistoryWriteOutcome, HistoryWriteRejection> {
        self.principal_bounds(principal, budget)
            .map_err(read_reason)?;
        let source = request
            .source_bytes()
            .map_err(|_| HistoryWriteRejection::Limit)?;
        if request.samples.len() > self.limits.max_samples
            || source
                > self
                    .limits
                    .max_source_bytes
                    .min(budget.limits.max_input_bytes)
        {
            return Err(HistoryWriteRejection::Limit);
        }
        budget
            .charge(
                BudgetKind::Retained,
                source.saturating_mul(8).saturating_add(8192),
            )
            .map_err(read_reason)?;
        budget
            .charge(BudgetKind::Work, source)
            .map_err(read_reason)?;
        let canonical = canonical_request(&request).map_err(|_| HistoryWriteRejection::Invalid)?;
        budget
            .charge(BudgetKind::Retained, canonical.len().saturating_mul(8))
            .map_err(read_reason)?;
        let binding = HistoryBinding {
            principal: PrincipalKey::from(principal),
            identity: request.identity.clone(),
        };
        let policy = loop {
            if let Some(policy) = self
                .policy
                .try_read_for(budget.wait_quantum().map_err(read_reason)?)
            {
                break policy;
            }
        };
        // Old recognized intent is authorized before inspecting current graph,
        // point incarnation, schema, expected generation or provider capability.
        if let Some(record) = self.lookup(&binding, budget).map_err(read_reason)? {
            return self.recognized(&policy, principal, record, &canonical);
        }
        if request.identity.authority != self.store.authority() {
            if !policy
                .rules
                .authorize_reconcile(principal, &request.identity, None)
            {
                return Err(HistoryWriteRejection::Forbidden);
            }
            return Ok(HistoryWriteOutcome::Unknown {
                identity: request.identity,
                cause: HistoryWriteUnknown::Missing,
            });
        }
        let provider = self.history.provider();
        if !provider
            .history_write_capability()
            .is_some_and(|capability| {
                capability.store.shares_authority(&self.store)
                    && capability.qualification == self.qualification
            })
        {
            return Err(HistoryWriteRejection::Unsupported);
        }
        if !policy.rules.authorize_intent(principal, &request) {
            return Err(HistoryWriteRejection::Forbidden);
        }
        let plan = match self.prepare(principal, &policy, &request, source, budget) {
            Ok(plan) => plan,
            Err(reason) => {
                // Another identical submission may have bound/committed while
                // this one waited for graph/point admission. Observe it first.
                if let Some(record) = self.lookup(&binding, budget).map_err(read_reason)? {
                    return self.recognized(&policy, principal, record, &canonical);
                }
                return Err(reason);
            }
        };
        let bytes = source
            .saturating_add(canonical.len().saturating_mul(8))
            .saturating_add(principal.bytes().saturating_mul(2))
            .saturating_add(4096);
        let mut state = loop {
            if let Some(state) = self
                .store
                .inner
                .state
                .try_lock_for(budget.wait_quantum().map_err(read_reason)?)
            {
                break state;
            }
        };
        if let Some(record) = state.receipts.get(&binding).cloned() {
            drop(state);
            return self.recognized(&policy, principal, record, &canonical);
        }
        if state.receipts.len() >= self.store.inner.limits.receipt_capacity
            || bytes
                > self
                    .store
                    .inner
                    .limits
                    .receipt_bytes
                    .saturating_sub(state.receipt_bytes)
        {
            return Err(HistoryWriteRejection::Capacity);
        }
        state
            .receipts
            .try_reserve(1)
            .map_err(|_| HistoryWriteRejection::Capacity)?;
        budget.check().map_err(read_reason)?;
        let outcome = if plan.is_none() {
            HistoryWriteOutcome::Rejected {
                identity: request.identity.clone(),
                reason: HistoryWriteRejection::EmptyBatch,
            }
        } else {
            HistoryWriteOutcome::Unknown {
                identity: request.identity.clone(),
                cause: HistoryWriteUnknown::Pending,
            }
        };
        state.receipts.insert(
            binding.clone(),
            HistoryRecord {
                canonical: canonical.into(),
                request: Arc::new(request),
                outcome: outcome.clone(),
            },
        );
        state.receipt_bytes += bytes;
        drop(state);
        let Some(plan) = plan else {
            return Ok(outcome);
        };
        let identity = plan.receipt.identity.clone();
        let prepared = PreparedHistoryMutation {
            store: self.store.clone(),
            graph: self.history.read_service().graph(),
            binding: binding.clone(),
            plan: Some(plan),
            deadline: budget.deadline,
            cancel: budget.cancel.clone(),
            owner_cancel: budget.owner_cancel.clone(),
            owner_sealed: budget.owner_sealed.clone(),
            policy: self.policy.clone(),
            generation: policy.generation.clone(),
            _lease: budget.lease.clone(),
        };
        drop(policy);
        let outcome = provider.commit_history(prepared);
        if outcome.identity() != &identity {
            return Ok(HistoryWriteOutcome::Unknown {
                identity,
                cause: HistoryWriteUnknown::InvalidAcknowledgement,
            });
        }
        if !matches!(outcome, HistoryWriteOutcome::Unknown { .. })
            && !self
                .lookup(&binding, budget)
                .ok()
                .flatten()
                .is_some_and(|record| record.outcome == outcome)
        {
            return Ok(HistoryWriteOutcome::Unknown {
                identity,
                cause: HistoryWriteUnknown::InvalidAcknowledgement,
            });
        }
        Ok(outcome)
    }
    fn prepare(
        &self,
        principal: &Principal,
        policy: &PolicyVersion,
        request: &HistoryWriteRequest,
        source: usize,
        budget: &mut Budget,
    ) -> Result<Option<PreparedSeries>, HistoryWriteRejection> {
        let graph = self.history.read_service().graph();
        let (schema, observed) = loop {
            if let Some(result) =
                graph.read_for(budget.wait_quantum().map_err(read_reason)?, |graph| {
                    let row = graph
                        .get(&request.identity.point)
                        .ok_or(HistoryWriteRejection::Invalid)?;
                    Ok((
                        crate::history_admission::schema(row, budget).map_err(read_reason)?,
                        graph.state(),
                    ))
                })
            {
                break result?;
            }
        };
        if !policy.rules.authorize_schema(principal, request, &schema) {
            return Err(HistoryWriteRejection::Forbidden);
        }
        for sample in &request.samples {
            budget.check().map_err(read_reason)?;
            if !valid_value(&sample.val, &schema) {
                return Err(HistoryWriteRejection::Unsupported);
            }
            if sample.ts.tz_name != schema.timezone
                || offset_at(&sample.ts.tz_name, sample.ts.dt) != Some(*sample.ts.dt.offset())
            {
                return Err(HistoryWriteRejection::Invalid);
            }
        }
        let point = loop {
            if let Some(mut state) = self
                .store
                .inner
                .state
                .try_lock_for(budget.wait_quantum().map_err(read_reason)?)
            {
                break self
                    .store
                    .point_in(&mut state, &request.identity.point)
                    .map_err(provider_reason)?;
            }
        };
        let series = loop {
            if let Some(series) = point.try_read_for(budget.wait_quantum().map_err(read_reason)?) {
                break series;
            }
        };
        if series.incarnation != request.identity.incarnation
            || series.generation != request.expected_generation
        {
            return Err(HistoryWriteRejection::Conflict);
        }
        if request.samples.is_empty() {
            return Ok(None);
        }
        let generation = series
            .generation
            .checked_add(1)
            .ok_or(HistoryWriteRejection::Limit)?;
        if series.items.len() > self.limits.max_series_samples {
            return Err(HistoryWriteRejection::Limit);
        }
        if source > self.limits.max_prepared_bytes {
            return Err(HistoryWriteRejection::Limit);
        }
        let mut bytes = 0usize;
        for item in &series.items {
            budget.check().map_err(read_reason)?;
            let size = crate::history_store::sample_bytes(&item.val)
                .ok_or(HistoryWriteRejection::Unsupported)?;
            if !valid_value(&item.val, &schema) {
                return Err(HistoryWriteRejection::Unsupported);
            }
            bytes = bytes.saturating_add(size);
            if bytes.saturating_add(source) > self.limits.max_prepared_bytes {
                return Err(HistoryWriteRejection::Limit);
            }
        }
        // Source-sized bounds precede the first existing-series clone. Original
        // request accounting already reserved incoming values and sorting space.
        budget
            .charge(
                BudgetKind::Retained,
                bytes.saturating_mul(3).saturating_add(4096),
            )
            .map_err(read_reason)?;
        budget
            .charge(BudgetKind::Work, bytes.saturating_mul(2))
            .map_err(read_reason)?;
        let mut next = Series {
            incarnation: series.incarnation,
            generation,
            items: series.items.clone(),
            evicted: series.evicted,
        };
        let before = self.store.series_state(&series);
        drop(series);
        let n = request.samples.len();
        budget
            .charge(
                BudgetKind::Work,
                n.saturating_mul((n.max(1).ilog2() as usize + 1).saturating_mul(64)),
            )
            .map_err(read_reason)?;
        let mut input: Vec<HisItem> = request
            .samples
            .iter()
            .map(|sample| HisItem {
                ts: sample.ts.dt,
                val: sample.val.clone(),
            })
            .collect();
        // Stable sorting preserves original order among equal instants.
        input.sort_by_key(|item| item.ts);
        let mut unique: Vec<HisItem> = Vec::with_capacity(input.len());
        for item in input {
            if unique.last().is_some_and(|last| last.ts == item.ts) {
                *unique.last_mut().expect("last item") = item;
            } else {
                unique.push(item);
            }
        }
        let unique_samples = unique.len() as u64;
        let capacity = next
            .items
            .len()
            .checked_add(unique.len())
            .ok_or(HistoryWriteRejection::Limit)?;
        let mut merged = Vec::new();
        merged
            .try_reserve_exact(capacity)
            .map_err(|_| HistoryWriteRejection::Capacity)?;
        let mut old = std::mem::take(&mut next.items).into_iter().peekable();
        let mut incoming = unique.into_iter().peekable();
        while old.peek().is_some() || incoming.peek().is_some() {
            budget.check().map_err(read_reason)?;
            match (old.peek(), incoming.peek()) {
                (Some(a), Some(b)) if a.ts < b.ts => merged.push(old.next().expect("old sample")),
                (Some(a), Some(b)) if a.ts == b.ts => {
                    old.next();
                    merged.push(incoming.next().expect("incoming sample"));
                }
                (_, Some(_)) => merged.push(incoming.next().expect("incoming sample")),
                (Some(_), None) => merged.push(old.next().expect("old sample")),
                _ => break,
            }
        }
        next.items = merged;
        let evicted_samples = trim(&mut next, self.store.inner.limits.max_items_per_point) as u64;
        let receipt = HistoryWriteReceipt {
            identity: request.identity.clone(),
            before_generation: before.generation,
            after_generation: generation,
            change_sequence: 0,
            submitted_samples: request.samples.len() as u64,
            unique_samples,
            retained_samples: next.items.len() as u64,
            evicted_samples,
            qualification: self.qualification,
        };
        // All receipt allocations/codec representability checks precede effects.
        let mut check = receipt.clone();
        check.change_sequence = u64::MAX;
        outcome_grid(&HistoryWriteOutcome::Committed(check))
            .map_err(|_| HistoryWriteRejection::Limit)?;
        let change = HistoryChangeRecord {
            sequence: 0,
            point: request.identity.point.clone(),
            before,
            after: self.store.series_state(&next),
            submitted_samples: receipt.submitted_samples,
            unique_samples,
            retained_samples: receipt.retained_samples,
            evicted_samples,
            evicted_through: next.evicted,
            operation: Some(request.identity.clone()),
        };
        Ok(Some(PreparedSeries {
            point,
            graph: observed,
            next,
            receipt,
            change,
        }))
    }
    pub(crate) fn reconcile(
        &self,
        principal: &Principal,
        identity: HistoryOperationIdentity,
        budget: &mut Budget,
    ) -> Result<HistoryWriteOutcome, ReadError> {
        self.principal_bounds(principal, budget)?;
        validate_identity(&identity)
            .map_err(|_| ReadError::InvalidQuery("invalid history operation identity"))?;
        let policy = loop {
            if let Some(policy) = self.policy.try_read_for(budget.wait_quantum()?) {
                break policy;
            }
        };
        let binding = HistoryBinding {
            principal: PrincipalKey::from(principal),
            identity: identity.clone(),
        };
        let record = self.lookup(&binding, budget)?;
        if !policy.rules.authorize_reconcile(
            principal,
            &identity,
            record.as_ref().map(|record| record.request.as_ref()),
        ) {
            return Err(ReadError::Forbidden);
        }
        budget.check()?;
        Ok(record.map_or(
            HistoryWriteOutcome::Unknown {
                identity,
                cause: HistoryWriteUnknown::Missing,
            },
            |record| record.outcome,
        ))
    }
}
/// Opaque reserved publication. Retaining it retains the original admission and
/// lifecycle lease. Publish/reject consumes it; dropping it leaves Unknown/Pending.
pub struct PreparedHistoryMutation {
    store: HisStore,
    graph: haystack_core::graph::SharedGraph,
    binding: HistoryBinding,
    plan: Option<PreparedSeries>,
    deadline: Instant,
    cancel: CancellationToken,
    owner_cancel: Option<CancellationToken>,
    owner_sealed: Option<CancellationToken>,
    policy: Arc<RwLock<PolicyVersion>>,
    generation: Arc<()>,
    _lease: Option<Arc<crate::service::WorkLease>>,
}
impl PreparedHistoryMutation {
    pub fn identity(&self) -> &HistoryOperationIdentity {
        &self.binding.identity
    }
    pub fn before(&self) -> HistoryState {
        self.plan.as_ref().expect("unconsumed plan").change.before
    }
    fn stopped(&self) -> Option<HistoryWriteRejection> {
        if Instant::now() >= self.deadline {
            Some(HistoryWriteRejection::Deadline)
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
            Some(HistoryWriteRejection::Cancelled)
        } else {
            None
        }
    }
    pub fn reject(self, reason: HistoryWriteRejection) -> HistoryWriteOutcome {
        let outcome = HistoryWriteOutcome::Rejected {
            identity: self.binding.identity.clone(),
            reason,
        };
        if let Some(record) = self
            .store
            .inner
            .state
            .lock()
            .receipts
            .get_mut(&self.binding)
        {
            record.outcome = outcome.clone();
        }
        outcome
    }
    pub fn publish(mut self) -> HistoryWriteOutcome {
        let policy_owner = self.policy.clone();
        let store = self.store.clone();
        let graph = self.graph.clone();
        let receipt = &self.plan.as_ref().expect("unconsumed plan").receipt;
        let mut committed = Some(HistoryWriteOutcome::Committed(receipt.clone()));
        let mut response = Some(HistoryWriteOutcome::Committed(receipt.clone()));
        loop {
            if let Some(reason) = self.stopped() {
                return self.reject(reason);
            }
            let Some(policy) = policy_owner.try_read_for(Duration::from_millis(5)) else {
                continue;
            };
            if !Arc::ptr_eq(&policy.generation, &self.generation) {
                drop(policy);
                return self.reject(HistoryWriteRejection::Conflict);
            }
            // Never wait for authority/point locks while holding graph ownership.
            // A failed try releases every publication lock before the next wait.
            let result = graph
                .read_for(Duration::ZERO, |current_graph| {
                    if let Some(reason) = self.stopped() {
                        return Some(Err(reason));
                    }
                    if current_graph.state() != self.plan.as_ref().expect("plan").graph {
                        return Some(Err(HistoryWriteRejection::Conflict));
                    }
                    let mut state = store.inner.state.try_lock()?;
                    let point = self.plan.as_ref().expect("plan").point.clone();
                    let mut series = point.try_write()?;
                    let before = self.plan.as_ref().expect("plan").change.before;
                    if series.incarnation != before.incarnation
                        || series.generation != before.generation
                    {
                        return Some(Err(HistoryWriteRejection::Conflict));
                    }
                    let Some(sequence) = state.sequence.checked_add(1) else {
                        return Some(Err(HistoryWriteRejection::Limit));
                    };
                    if !state.receipts.get(&self.binding).is_some_and(|record| {
                        matches!(
                            record.outcome,
                            HistoryWriteOutcome::Unknown {
                                cause: HistoryWriteUnknown::Pending,
                                ..
                            }
                        )
                    }) {
                        return Some(Err(HistoryWriteRejection::Conflict));
                    }
                    if let Some(reason) = self.stopped() {
                        return Some(Err(reason));
                    }
                    let mut plan = self.plan.take().expect("one publication");
                    plan.change.sequence = sequence;
                    for outcome in [&mut committed, &mut response] {
                        let Some(HistoryWriteOutcome::Committed(receipt)) = outcome else {
                            unreachable!("prepared committed receipt")
                        };
                        receipt.change_sequence = sequence;
                    }
                    // Every allocation, validation and callback preceded this point.
                    // Point readers, receipt lookup and ledger observers all exclude
                    // this publication until every part has become authoritative.
                    *series = plan.next;
                    store.append_change(&mut state, plan.change);
                    state
                        .receipts
                        .get_mut(&self.binding)
                        .expect("reserved receipt")
                        .outcome = committed.take().expect("one receipt");
                    Some(Ok(()))
                })
                .flatten();
            drop(policy);
            match result {
                Some(Ok(())) => return response.take().expect("one response"),
                Some(Err(reason)) => return self.reject(reason),
                None => std::thread::sleep(Duration::from_millis(1)),
            }
        }
    }
}
fn read_reason(error: ReadError) -> HistoryWriteRejection {
    match error {
        ReadError::Forbidden => HistoryWriteRejection::Forbidden,
        ReadError::Capacity => HistoryWriteRejection::Capacity,
        ReadError::Cancelled => HistoryWriteRejection::Cancelled,
        ReadError::Deadline => HistoryWriteRejection::Deadline,
        ReadError::Projection => HistoryWriteRejection::Unsupported,
        ReadError::InvalidQuery(_) => HistoryWriteRejection::Invalid,
        _ => HistoryWriteRejection::Limit,
    }
}
fn provider_reason(error: crate::HistoryProviderError) -> HistoryWriteRejection {
    match error {
        crate::HistoryProviderError::Changed => HistoryWriteRejection::Conflict,
        crate::HistoryProviderError::Stopped => HistoryWriteRejection::Cancelled,
        crate::HistoryProviderError::Unsupported => HistoryWriteRejection::Unsupported,
        crate::HistoryProviderError::Limit | crate::HistoryProviderError::Exhausted => {
            HistoryWriteRejection::Limit
        }
        crate::HistoryProviderError::Failed => HistoryWriteRejection::Provider,
    }
}

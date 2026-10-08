//! One application owner for bounded state subscriptions and legacy attachments.
//! Records never retain ordinary read permits. Every active operation captures
//! its authorized graph view under one graph guard and publishes by record CAS.
mod adapters;
use crate::{
    ApplicationResource, BudgetKind, EphemeralMutationStore, PolicySnapshot, Principal,
    ReadAdmission, ReadContext, ReadError, ReadOperation, ReadService, ReadyInfo, ResourceContext,
    ResourceFuture, SubscriptionSession, budget::Budget, sanitize,
};
pub use adapters::{LegacySubscriptionRequest, SubscriptionWireRequest};
use haystack_core::{
    codecs::{subscription as wire, typed},
    data::HDict,
    graph::GraphState,
    kinds::Kind,
};
use parking_lot::Mutex;
use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
pub use wire::{
    SubscriptionCreate, SubscriptionDelivery, SubscriptionId, SubscriptionOutcome,
    SubscriptionRejection, SubscriptionRequest, SubscriptionResync,
};

#[derive(Debug, Clone)]
pub struct SubscriptionLimits {
    pub max_watches: usize,
    pub max_watches_per_session: usize,
    pub max_bindings: usize,
    pub max_bindings_per_session: usize,
    pub max_ids: usize,
    pub max_view_bytes: usize,
    pub max_delivery_bytes: usize,
    pub max_retained_bytes: usize,
    pub max_change_diffs: usize,
    pub max_lease: Duration,
    pub maintenance_interval: Duration,
}
impl Default for SubscriptionLimits {
    fn default() -> Self {
        Self {
            max_watches: 128,
            max_watches_per_session: 32,
            max_bindings: 1024,
            max_bindings_per_session: 128,
            max_ids: 512,
            max_view_bytes: 65_536,
            max_delivery_bytes: 262_144,
            max_retained_bytes: 128 * 1024 * 1024,
            max_change_diffs: 10_000,
            max_lease: Duration::from_secs(300),
            maintenance_interval: Duration::from_millis(100),
        }
    }
}
impl SubscriptionLimits {
    fn validate(&self) -> Result<(), ReadError> {
        if self.max_watches == 0
            || self.max_watches > 4096
            || self.max_watches_per_session == 0
            || self.max_watches_per_session > self.max_watches
            || self.max_bindings < self.max_watches
            || self.max_bindings > 16_384
            || self.max_bindings_per_session < self.max_watches_per_session
            || self.max_bindings_per_session > self.max_bindings
            || self.max_ids == 0
            || self.max_ids > wire::MAX_IDS
            || self.max_view_bytes == 0
            || self.max_view_bytes > wire::MAX_PAYLOAD_BYTES
            || self.max_delivery_bytes == 0
            || self.max_delivery_bytes > wire::MAX_PAYLOAD_BYTES
            || self.max_retained_bytes > 512 * 1024 * 1024
            || self.max_change_diffs == 0
            || self.max_change_diffs > 100_000
            || self.max_lease.is_zero()
            || self.max_lease > Duration::from_millis(wire::MAX_LEASE_MS)
            || self.maintenance_interval.is_zero()
            || self.maintenance_interval > Duration::from_secs(10)
        {
            return Err(ReadError::InvalidLimits);
        }
        Ok(())
    }
}
#[derive(Clone)]
pub struct StateSubscriptionService {
    inner: Arc<SubscriptionInner>,
}
struct SubscriptionInner {
    reads: ReadService,
    store: EphemeralMutationStore,
    authority: [u8; 16],
    limits: SubscriptionLimits,
    records: Mutex<Registry>,
    stopped: AtomicBool,
    anonymous: SubscriptionSession,
}
#[derive(Default)]
struct Registry {
    entries: HashMap<[u8; 16], Arc<Entry>>,
    bindings: HashMap<([u8; 16], String), [u8; 16]>,
    reserved_bytes: usize,
}
struct Entry {
    identity: SubscriptionId,
    session: SubscriptionSession,
    canonical: Arc<[u8]>,
    base_reservation: usize,
    reservation: AtomicUsize,
    legacy: bool,
    connection: Option<[u8; 16]>,
    active: AtomicBool,
    state: Mutex<Record>,
}
#[derive(Clone)]
struct Record {
    revision: u64,
    status: Status,
}
#[derive(Clone)]
enum Status {
    Preparing,
    Live(Live),
    Closed,
    Resync(SubscriptionResync),
}
#[derive(Clone)]
struct Live {
    ids: Arc<[String]>,
    graph: GraphState,
    policy: Arc<str>,
    scope: u64,
    acknowledged: u64,
    projection: Arc<Projection>,
    prepared: Option<Prepared>,
    last_ack: Option<Ack>,
    lease_until: Instant,
}
#[derive(Clone, Copy)]
struct Ack {
    scope: u64,
    token: [u8; 16],
    through: u64,
}
#[derive(Clone)]
struct Prepared {
    delivery: Arc<SubscriptionDelivery>,
    projection: Arc<Projection>,
}
#[derive(Clone)]
struct ProjectedRow {
    row: Arc<HDict>,
    canonical: Arc<[u8]>,
}
type Projection = BTreeMap<String, ProjectedRow>;
struct Capture {
    graph: GraphState,
    policy: Arc<str>,
    projection: Arc<Projection>,
}
enum CaptureError {
    Read(ReadError),
    Resync(SubscriptionResync),
}
impl From<ReadError> for CaptureError {
    fn from(value: ReadError) -> Self {
        Self::Read(value)
    }
}
struct Reservation {
    owner: Arc<SubscriptionInner>,
    entry: Arc<Entry>,
    published: bool,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.published {
            self.owner.remove(&self.entry);
        }
    }
}
enum Publication {
    Done,
    Retry,
    Capacity,
    Stopped(SubscriptionResync),
}

impl StateSubscriptionService {
    pub fn new(
        reads: ReadService,
        store: EphemeralMutationStore,
        limits: SubscriptionLimits,
    ) -> Result<Self, ReadError> {
        limits.validate()?;
        if !reads.graph().shares_storage(&store.graph()) {
            return Err(ReadError::Forbidden);
        }
        Ok(Self {
            inner: Arc::new(SubscriptionInner {
                reads,
                store,
                authority: rand::random(),
                limits,
                records: Mutex::new(Registry::default()),
                stopped: AtomicBool::new(false),
                anonymous: SubscriptionSession::legacy_anonymous(),
            }),
        })
    }
    pub fn read_service(&self) -> &ReadService {
        &self.inner.reads
    }
    pub fn entity_store(&self) -> EphemeralMutationStore {
        self.inner.store.clone()
    }
    pub fn authority(&self) -> [u8; 16] {
        self.inner.authority
    }
    pub fn limits(&self) -> &SubscriptionLimits {
        &self.inner.limits
    }
    pub fn active_watches(&self) -> usize {
        self.inner
            .records
            .lock()
            .entries
            .values()
            .filter(|entry| entry.active.load(Ordering::Acquire))
            .count()
    }
    pub fn binding_count(&self) -> usize {
        self.inner.records.lock().entries.len()
    }
    pub fn retained_reservations(&self) -> usize {
        self.inner.records.lock().reserved_bytes
    }
    pub fn anonymous_legacy_session(&self) -> SubscriptionSession {
        self.inner.anonymous.clone()
    }
    pub async fn execute(
        &self,
        context: ReadContext,
        session: SubscriptionSession,
        request: SubscriptionRequest,
    ) -> Result<SubscriptionOutcome, ReadError> {
        let admission = self.inner.reads.begin(context).await?;
        self.execute_admitted(admission, session, request).await
    }
    pub async fn execute_admitted(
        &self,
        admission: ReadAdmission,
        session: SubscriptionSession,
        request: SubscriptionRequest,
    ) -> Result<SubscriptionOutcome, ReadError> {
        if !admission.belongs_to(&self.inner.reads) {
            return Err(ReadError::Forbidden);
        }
        let owner = self.inner.clone();
        admission
            .run_task(move |principal, budget| {
                owner.execute(&principal, &session, request, false, None, budget)
            })
            .await
    }
    /// Recheck a queued delivery immediately before encoding/emission. This does
    /// not acknowledge, renew or replace it. A superseded token is never emitted.
    pub async fn revalidate_delivery(
        &self,
        admission: ReadAdmission,
        session: SubscriptionSession,
        delivery: Arc<SubscriptionDelivery>,
    ) -> Result<SubscriptionOutcome, ReadError> {
        if !admission.belongs_to(&self.inner.reads) {
            return Err(ReadError::Forbidden);
        }
        let owner = self.inner.clone();
        admission
            .run_task(move |principal, budget| {
                let outcome = owner.execute(
                    &principal,
                    &session,
                    SubscriptionRequest::Poll {
                        watch: delivery.watch.clone(),
                    },
                    false,
                    None,
                    budget,
                )?;
                Ok(match &outcome {
                    SubscriptionOutcome::Delivery(current)
                        if current.token == delivery.token
                            && current.scope_generation == delivery.scope_generation =>
                    {
                        outcome
                    }
                    SubscriptionOutcome::Resync(_)
                    | SubscriptionOutcome::Rejected(_)
                    | SubscriptionOutcome::Closed { .. } => outcome,
                    _ => SubscriptionOutcome::Resync(SubscriptionResync::ScopeChanged),
                })
            })
            .await
    }
    pub(crate) fn resource(&self) -> SubscriptionResource {
        SubscriptionResource(self.clone())
    }
    fn shutdown(&self) {
        self.inner.stopped.store(true, Ordering::Release);
        self.inner.anonymous.close();
        let mut table = self.inner.records.lock();
        table.entries.clear();
        table.bindings.clear();
        table.reserved_bytes = 0;
    }
}
impl SubscriptionInner {
    fn stopped(&self, budget: &Budget) -> bool {
        self.stopped.load(Ordering::Acquire)
            || budget
                .owner_sealed
                .as_ref()
                .is_some_and(crate::CancellationToken::is_cancelled)
    }
    fn admission(
        &self,
        principal: &Principal,
        session: &SubscriptionSession,
        request: &SubscriptionRequest,
        legacy: bool,
        budget: &mut Budget,
    ) -> Result<Option<SubscriptionOutcome>, ReadError> {
        if let Principal::Authenticated { permissions, .. } = principal
            && permissions.len() > 1024
        {
            return Err(ReadError::Budget(BudgetKind::Input));
        }
        let bytes = principal.bytes().saturating_add(
            request
                .source_bytes()
                .map_err(|_| ReadError::InvalidQuery("invalid subscription request"))?,
        );
        if bytes > budget.limits.max_input_bytes {
            return Err(ReadError::Budget(BudgetKind::Input));
        }
        budget.charge(BudgetKind::Work, bytes.saturating_mul(16))?;
        budget.charge(BudgetKind::Retained, bytes.saturating_mul(8))?;
        if !session.matches(principal) || (!legacy && !session.scoped()) {
            return Ok(Some(SubscriptionOutcome::Rejected(
                SubscriptionRejection::Forbidden,
            )));
        }
        if self.stopped(budget) {
            return Ok(Some(SubscriptionOutcome::Resync(
                SubscriptionResync::Shutdown,
            )));
        }
        if let Some(reason) = session.reason() {
            return Ok(Some(SubscriptionOutcome::Resync(reason)));
        }
        Ok(None)
    }
    fn remove(&self, entry: &Arc<Entry>) {
        let mut table = self.records.lock();
        if table
            .entries
            .get(&entry.identity.watch)
            .is_some_and(|current| Arc::ptr_eq(current, entry))
        {
            table.entries.remove(&entry.identity.watch);
            table
                .bindings
                .remove(&(entry.session.id(), entry.identity.key.clone()));
            table.reserved_bytes = table
                .reserved_bytes
                .saturating_sub(entry.reservation.load(Ordering::Relaxed));
        }
    }
    fn registry<'a>(
        &'a self,
        budget: &Budget,
    ) -> Result<parking_lot::MutexGuard<'a, Registry>, ReadError> {
        loop {
            if let Some(table) = self.records.try_lock_for(budget.wait_quantum()?) {
                return Ok(table);
            }
        }
    }
    fn record(entry: &Entry, budget: &Budget) -> Result<Record, ReadError> {
        loop {
            if let Some(record) = entry.state.try_lock_for(budget.wait_quantum()?) {
                return Ok(record.clone());
            }
        }
    }
    fn execute(
        self: &Arc<Self>,
        principal: &Principal,
        session: &SubscriptionSession,
        request: SubscriptionRequest,
        legacy: bool,
        connection: Option<[u8; 16]>,
        budget: &mut Budget,
    ) -> Result<SubscriptionOutcome, ReadError> {
        if let Some(outcome) = self.admission(principal, session, &request, legacy, budget)? {
            return Ok(outcome);
        }
        if matches!(request, SubscriptionRequest::Describe) {
            let policy = self.reads.policy_snapshot(principal)?;
            if !policy.operation(ReadOperation::Subscriptions) {
                return Ok(SubscriptionOutcome::Rejected(
                    SubscriptionRejection::Forbidden,
                ));
            }
            return Ok(SubscriptionOutcome::Authority {
                authority: self.authority,
                dataset: self.store.dataset(),
            });
        }
        let mut reservation = None;
        let entry = if let SubscriptionRequest::Create(create) = &request {
            if create.authority != self.authority {
                return Ok(SubscriptionOutcome::Resync(SubscriptionResync::Authority));
            }
            if create.ids.len() > self.limits.max_ids
                || Duration::from_millis(create.lease_ms) > self.limits.max_lease
            {
                return Ok(SubscriptionOutcome::Rejected(SubscriptionRejection::Limit));
            }
            let canonical = wire::encode(&request)
                .map_err(|_| ReadError::InvalidQuery("invalid subscription creation"))?;
            budget.charge(BudgetKind::Work, canonical.len().saturating_mul(2))?;
            budget.charge(BudgetKind::Retained, canonical.len().saturating_mul(3))?;
            let mut table = self.registry(budget)?;
            if let Some(id) = table.bindings.get(&(session.id(), create.key.clone())) {
                let entry = table.entries.get(id).expect("binding has entry").clone();
                if !entry.session.same_session(session)
                    || entry.canonical.as_ref() != canonical.as_slice()
                    || entry.legacy != legacy
                {
                    return Ok(SubscriptionOutcome::Rejected(
                        SubscriptionRejection::Conflict,
                    ));
                }
                entry
            } else {
                budget.charge(BudgetKind::Work, table.entries.len().saturating_add(1))?;
                let active = table
                    .entries
                    .values()
                    .filter(|entry| entry.active.load(Ordering::Acquire))
                    .count();
                let (own, own_active) = table
                    .entries
                    .values()
                    .filter(|entry| entry.session.same_session(session))
                    .fold((0usize, 0usize), |(count, active), entry| {
                        (
                            count + 1,
                            active + usize::from(entry.active.load(Ordering::Acquire)),
                        )
                    });
                let base_reservation = self
                    .limits
                    .max_view_bytes
                    .saturating_mul(12)
                    .saturating_add(self.limits.max_delivery_bytes.saturating_mul(2))
                    .saturating_add(canonical.len().saturating_mul(4))
                    .saturating_add(principal.bytes())
                    .saturating_add(4096);
                let reserve = base_reservation.saturating_add(membership_bytes(&create.ids));
                if active >= self.limits.max_watches
                    || own_active >= self.limits.max_watches_per_session
                    || table.entries.len() >= self.limits.max_bindings
                    || own >= self.limits.max_bindings_per_session
                    || reserve
                        > self
                            .limits
                            .max_retained_bytes
                            .saturating_sub(table.reserved_bytes)
                {
                    return Ok(SubscriptionOutcome::Rejected(
                        SubscriptionRejection::Capacity,
                    ));
                }
                let entry = Arc::new(Entry {
                    identity: SubscriptionId {
                        authority: self.authority,
                        watch: rand::random(),
                        key: create.key.clone(),
                    },
                    session: session.clone(),
                    canonical: canonical.into(),
                    base_reservation,
                    reservation: AtomicUsize::new(reserve),
                    legacy,
                    connection,
                    active: AtomicBool::new(true),
                    state: Mutex::new(Record {
                        revision: 0,
                        status: Status::Preparing,
                    }),
                });
                table.reserved_bytes += reserve;
                table
                    .bindings
                    .insert((session.id(), create.key.clone()), entry.identity.watch);
                table.entries.insert(entry.identity.watch, entry.clone());
                reservation = Some(Reservation {
                    owner: self.clone(),
                    entry: entry.clone(),
                    published: false,
                });
                entry
            }
        } else {
            let watch = request.watch().expect("non-create request identity");
            if watch.authority != self.authority {
                return Ok(SubscriptionOutcome::Resync(SubscriptionResync::Authority));
            }
            let table = self.registry(budget)?;
            let Some(entry) = table.entries.get(&watch.watch) else {
                return Ok(SubscriptionOutcome::Resync(SubscriptionResync::Unknown));
            };
            if entry.identity != *watch
                || !entry.session.same_session(session)
                || entry.legacy != legacy
            {
                return Ok(SubscriptionOutcome::Rejected(
                    SubscriptionRejection::Forbidden,
                ));
            }
            entry.clone()
        };
        // Subscribe before the initial graph read. Wake loss never controls
        // correctness: every operation traverses retained units immediately.
        let _wake = reservation
            .as_ref()
            .map(|_| self.reads.graph().subscribe_wakes());
        loop {
            if self.stopped(budget) {
                return Ok(SubscriptionOutcome::Resync(SubscriptionResync::Shutdown));
            }
            if let Some(reason) = session.reason() {
                return Ok(SubscriptionOutcome::Resync(reason));
            }
            let record = Self::record(&entry, budget)?;
            let live = match &record.status {
                Status::Closed => {
                    return Ok(SubscriptionOutcome::Closed {
                        watch: entry.identity.clone(),
                    });
                }
                Status::Resync(reason) => return Ok(SubscriptionOutcome::Resync(*reason)),
                Status::Preparing if reservation.is_none() => {
                    return Ok(SubscriptionOutcome::Rejected(
                        SubscriptionRejection::Pending,
                    ));
                }
                Status::Preparing => None,
                Status::Live(live) => Some(live),
            };
            if let Err(error) = budget.charge(BudgetKind::Work, 1) {
                if live.is_some() && execution_overflow(&error) {
                    if self.terminal(
                        &entry,
                        record.revision,
                        SubscriptionResync::Overflow,
                        budget,
                    )? {
                        return Ok(SubscriptionOutcome::Resync(SubscriptionResync::Overflow));
                    }
                    continue;
                }
                return Err(error);
            }
            if let Some(live) = live
                && Instant::now() >= live.lease_until
            {
                if self.terminal(
                    &entry,
                    record.revision,
                    SubscriptionResync::LeaseExpired,
                    budget,
                )? {
                    return Ok(SubscriptionOutcome::Resync(
                        SubscriptionResync::LeaseExpired,
                    ));
                }
                continue;
            }
            let ids = match &request {
                SubscriptionRequest::Create(create) if live.is_none() => {
                    Arc::<[String]>::from(normalize(&create.ids))
                }
                SubscriptionRequest::Replace {
                    ids,
                    expected_scope_generation,
                    ..
                } => {
                    let current = live.expect("existing record");
                    if *expected_scope_generation != current.scope {
                        return Ok(SubscriptionOutcome::Rejected(
                            SubscriptionRejection::Conflict,
                        ));
                    }
                    if ids.len() > self.limits.max_ids {
                        return Ok(SubscriptionOutcome::Rejected(SubscriptionRejection::Limit));
                    }
                    Arc::<[String]>::from(normalize(ids))
                }
                _ => live.expect("existing record").ids.clone(),
            };
            let captured = match self.capture(principal, &ids, live, budget) {
                Ok(captured) => captured,
                Err(CaptureError::Read(error)) if live.is_some() && execution_overflow(&error) => {
                    if self.terminal(
                        &entry,
                        record.revision,
                        SubscriptionResync::Overflow,
                        budget,
                    )? {
                        return Ok(SubscriptionOutcome::Resync(SubscriptionResync::Overflow));
                    }
                    continue;
                }
                Err(CaptureError::Read(error)) => return Err(error),
                Err(CaptureError::Resync(reason)) if live.is_none() => {
                    return Ok(SubscriptionOutcome::Rejected(
                        if reason == SubscriptionResync::Policy {
                            SubscriptionRejection::Forbidden
                        } else {
                            SubscriptionRejection::Limit
                        },
                    ));
                }
                Err(CaptureError::Resync(reason)) => {
                    if self.terminal(&entry, record.revision, reason, budget)? {
                        return Ok(SubscriptionOutcome::Resync(reason));
                    }
                    continue;
                }
            };
            let observed = captured.graph;
            let observed_policy = captured.policy.clone();
            let (next, outcome) =
                match self.prepare(&entry, &request, &record, ids, captured, budget) {
                    Ok(prepared) => prepared,
                    Err(ReadError::Budget(BudgetKind::Output)) if live.is_none() => {
                        return Ok(SubscriptionOutcome::Rejected(SubscriptionRejection::Limit));
                    }
                    Err(error) if live.is_some() && execution_overflow(&error) => {
                        if self.terminal(
                            &entry,
                            record.revision,
                            SubscriptionResync::Overflow,
                            budget,
                        )? {
                            return Ok(SubscriptionOutcome::Resync(SubscriptionResync::Overflow));
                        }
                        continue;
                    }
                    Err(error) => return Err(error),
                };
            let policy = self.reads.policy_snapshot(principal)?;
            if policy.scope_key() != observed_policy.as_ref()
                || !policy.operation(ReadOperation::Subscriptions)
                || !policy.operation(ReadOperation::Read)
            {
                if self.terminal(&entry, record.revision, SubscriptionResync::Policy, budget)? {
                    return Ok(SubscriptionOutcome::Resync(SubscriptionResync::Policy));
                }
                continue;
            }
            match self.publish(&entry, record.revision, observed, next, session, budget)? {
                Publication::Stopped(reason) => return Ok(SubscriptionOutcome::Resync(reason)),
                Publication::Retry => continue,
                Publication::Capacity => {
                    return Ok(SubscriptionOutcome::Rejected(
                        SubscriptionRejection::Capacity,
                    ));
                }
                Publication::Done => {
                    if let Some(reserved) = &mut reservation {
                        reserved.published = true;
                    }
                    return Ok(outcome);
                }
            }
        }
    }
    fn capture(
        &self,
        principal: &Principal,
        ids: &[String],
        previous: Option<&Live>,
        budget: &mut Budget,
    ) -> Result<Capture, CaptureError> {
        let policy = self.reads.policy_snapshot(principal)?;
        if !policy.operation(ReadOperation::Subscriptions) || !policy.operation(ReadOperation::Read)
        {
            return Err(CaptureError::Resync(SubscriptionResync::Policy));
        }
        if policy.scope_key().len() > 1024 {
            return Err(ReadError::Budget(BudgetKind::Input).into());
        }
        let policy_key: Arc<str> = budget.copy_string(policy.scope_key())?.into();
        if previous.is_some_and(|live| live.policy.as_ref() != policy.scope_key()) {
            return Err(CaptureError::Resync(SubscriptionResync::Policy));
        }
        let graph = self.reads.graph();
        loop {
            let result = graph.read_for(
                budget.wait_quantum()?,
                |graph| -> Result<Capture, CaptureError> {
                    let observed = graph.state();
                    if let Some(previous) = previous {
                        if observed.incarnation != previous.graph.incarnation {
                            return Err(CaptureError::Resync(SubscriptionResync::Incarnation));
                        }
                        if observed.catalog_generation != previous.graph.catalog_generation {
                            return Err(CaptureError::Resync(SubscriptionResync::Catalog));
                        }
                        let from = previous
                            .prepared
                            .as_ref()
                            .map_or(previous.acknowledged, |prepared| prepared.delivery.through);
                        let units = graph
                            .change_units_since(from)
                            .map_err(|_| CaptureError::Resync(SubscriptionResync::Gap))?;
                        let mut diffs = 0usize;
                        for unit in units {
                            if unit.len() > self.limits.max_change_diffs.saturating_sub(diffs) {
                                return Err(CaptureError::Resync(SubscriptionResync::Overflow));
                            }
                            diffs += unit.len();
                            budget.charge(BudgetKind::Work, unit.len().saturating_add(1))?;
                        }
                        for id in previous.ids.iter() {
                            budget.charge(BudgetKind::Work, id.len() + 1)?;
                            if !policy.entity(id) || !policy.tag(id, "id") {
                                return Err(CaptureError::Resync(SubscriptionResync::Policy));
                            }
                        }
                        self.reauthorize_projection(&previous.projection, policy.as_ref(), budget)?;
                        if let Some(prepared) = &previous.prepared {
                            self.reauthorize_projection(
                                &prepared.projection,
                                policy.as_ref(),
                                budget,
                            )?;
                        }
                    }
                    let mut projection = Projection::new();
                    let mut bytes = 0usize;
                    for id in ids {
                        budget.charge(BudgetKind::Work, id.len() + 1)?;
                        if !policy.entity(id) || !policy.tag(id, "id") {
                            return Err(CaptureError::Resync(SubscriptionResync::Policy));
                        }
                        let before = budget.retained_remaining();
                        let row = sanitize::View {
                            graph,
                            policy: policy.as_ref(),
                            budget,
                        }
                        .entity(id)?;
                        let Some(row) = row else { continue };
                        let retained = before.saturating_sub(budget.retained_remaining());
                        budget.charge(BudgetKind::Work, retained.saturating_mul(2))?;
                        budget.charge(BudgetKind::Retained, retained.saturating_mul(8))?;
                        let canonical = typed::encode(&Kind::Dict(Box::new((*row).clone())))
                            .map_err(|_| CaptureError::Resync(SubscriptionResync::Overflow))?;
                        bytes = bytes
                            .saturating_add(canonical.len())
                            .saturating_add(id.len())
                            .saturating_add(512);
                        if bytes > self.limits.max_view_bytes {
                            return Err(CaptureError::Resync(SubscriptionResync::Overflow));
                        }
                        projection.insert(
                            budget.copy_string(id)?,
                            ProjectedRow {
                                row,
                                canonical: canonical.into(),
                            },
                        );
                    }
                    Ok(Capture {
                        graph: observed,
                        policy: policy_key.clone(),
                        projection: Arc::new(projection),
                    })
                },
            );
            if let Some(result) = result {
                return result;
            }
        }
    }
    fn reauthorize_projection(
        &self,
        projection: &Projection,
        policy: &dyn PolicySnapshot,
        budget: &mut Budget,
    ) -> Result<(), CaptureError> {
        for (id, stored) in projection {
            budget.charge(BudgetKind::Work, stored.canonical.len().saturating_mul(4))?;
            budget.charge(
                BudgetKind::Retained,
                stored.canonical.len().saturating_mul(8),
            )?;
            let Some(row) = sanitize::record(id, &stored.row, policy, budget)? else {
                return Err(CaptureError::Resync(SubscriptionResync::Policy));
            };
            let bytes = typed::encode(&Kind::Dict(Box::new(row)))
                .map_err(|_| CaptureError::Resync(SubscriptionResync::Policy))?;
            if stored.canonical.as_ref() != bytes.as_slice() {
                return Err(CaptureError::Resync(SubscriptionResync::Policy));
            }
        }
        Ok(())
    }
    fn delivery(
        &self,
        entry: &Entry,
        live: &Live,
        capture: &Capture,
        initial: bool,
        budget: &mut Budget,
    ) -> Result<Prepared, ReadError> {
        let encoded_bytes: usize = capture
            .projection
            .values()
            .map(|row| row.canonical.len().saturating_add(512))
            .sum();
        budget.charge(BudgetKind::Work, encoded_bytes.saturating_mul(4))?;
        budget.charge(
            BudgetKind::Retained,
            encoded_bytes.saturating_mul(12).saturating_add(8192),
        )?;
        let rows = capture
            .projection
            .iter()
            .filter(|(id, row)| {
                initial
                    || live
                        .projection
                        .get(*id)
                        .is_none_or(|old| old.canonical != row.canonical)
            })
            .map(|(_, row)| (*row.row).clone())
            .collect();
        let removed = if initial {
            vec![]
        } else {
            live.projection
                .keys()
                .filter(|id| !capture.projection.contains_key(*id))
                .cloned()
                .collect()
        };
        let delivery = Arc::new(SubscriptionDelivery {
            watch: entry.identity.clone(),
            dataset: self.store.dataset(),
            incarnation: capture.graph.incarnation,
            catalog_generation: capture.graph.catalog_generation,
            scope_generation: live.scope,
            token: rand::random(),
            from: if initial {
                capture.graph.revision
            } else {
                live.acknowledged
            },
            through: capture.graph.revision,
            initial,
            rows,
            removed,
        });
        let bytes = wire::encode(&SubscriptionOutcome::Delivery(delivery.clone()))
            .map_err(|_| ReadError::Budget(BudgetKind::Output))?;
        if bytes.len() > self.limits.max_delivery_bytes {
            return Err(ReadError::Budget(BudgetKind::Output));
        }
        Ok(Prepared {
            delivery,
            projection: capture.projection.clone(),
        })
    }
    fn prepare(
        &self,
        entry: &Entry,
        request: &SubscriptionRequest,
        record: &Record,
        ids: Arc<[String]>,
        capture: Capture,
        budget: &mut Budget,
    ) -> Result<(Record, SubscriptionOutcome), ReadError> {
        let revision = record
            .revision
            .checked_add(1)
            .ok_or(ReadError::Unavailable)?;
        let mut live = match &record.status {
            Status::Preparing => {
                let SubscriptionRequest::Create(create) = request else {
                    unreachable!()
                };
                Live {
                    ids: ids.clone(),
                    graph: capture.graph,
                    policy: capture.policy.clone(),
                    scope: 1,
                    acknowledged: capture.graph.revision,
                    projection: Arc::new(Projection::new()),
                    prepared: None,
                    last_ack: None,
                    lease_until: Instant::now() + Duration::from_millis(create.lease_ms),
                }
            }
            Status::Live(live) => live.clone(),
            _ => unreachable!(),
        };
        let initial = matches!(record.status, Status::Preparing);
        live.graph = capture.graph;
        let outcome = match request {
            SubscriptionRequest::Describe => unreachable!(),
            SubscriptionRequest::Create(_) if initial => {
                let prepared = self.delivery(entry, &live, &capture, true, budget)?;
                let outcome = SubscriptionOutcome::Delivery(prepared.delivery.clone());
                live.prepared = Some(prepared);
                outcome
            }
            SubscriptionRequest::Replace { .. } => {
                live.ids = ids;
                live.scope = live.scope.checked_add(1).ok_or(ReadError::Unavailable)?;
                live.acknowledged = capture.graph.revision;
                live.projection = Arc::new(Projection::new());
                live.last_ack = None;
                let prepared = self.delivery(entry, &live, &capture, true, budget)?;
                let outcome = SubscriptionOutcome::Delivery(prepared.delivery.clone());
                live.prepared = Some(prepared);
                outcome
            }
            SubscriptionRequest::Create(_)
            | SubscriptionRequest::Poll { .. }
            | SubscriptionRequest::Resume { .. } => {
                if let SubscriptionRequest::Resume {
                    scope_generation,
                    acknowledged,
                    ..
                } = request
                    && (*scope_generation != live.scope || *acknowledged != live.acknowledged)
                {
                    SubscriptionOutcome::Resync(SubscriptionResync::ScopeChanged)
                } else if let Some(prepared) = &live.prepared {
                    SubscriptionOutcome::Delivery(prepared.delivery.clone())
                } else if capture.graph.revision == live.acknowledged
                    && same_projection(&live.projection, &capture.projection)
                {
                    SubscriptionOutcome::Idle {
                        watch: entry.identity.clone(),
                        scope_generation: live.scope,
                        acknowledged: live.acknowledged,
                    }
                } else {
                    let prepared = self.delivery(entry, &live, &capture, false, budget)?;
                    let outcome = SubscriptionOutcome::Delivery(prepared.delivery.clone());
                    live.prepared = Some(prepared);
                    outcome
                }
            }
            SubscriptionRequest::Acknowledge {
                scope_generation,
                token,
                through,
                ..
            } => {
                let matches = |ack: Ack| {
                    ack.scope == *scope_generation && ack.token == *token && ack.through == *through
                };
                if *scope_generation != live.scope {
                    SubscriptionOutcome::Rejected(SubscriptionRejection::Conflict)
                } else if live.last_ack.is_some_and(matches) {
                    SubscriptionOutcome::Acknowledged {
                        watch: entry.identity.clone(),
                        scope_generation: *scope_generation,
                        token: *token,
                        through: *through,
                    }
                } else if live.prepared.as_ref().is_some_and(|prepared| {
                    matches(Ack {
                        scope: prepared.delivery.scope_generation,
                        token: prepared.delivery.token,
                        through: prepared.delivery.through,
                    })
                }) {
                    let prepared = live.prepared.take().expect("matching prepared delivery");
                    live.acknowledged = *through;
                    live.projection = prepared.projection;
                    live.last_ack = Some(Ack {
                        scope: *scope_generation,
                        token: *token,
                        through: *through,
                    });
                    SubscriptionOutcome::Acknowledged {
                        watch: entry.identity.clone(),
                        scope_generation: *scope_generation,
                        token: *token,
                        through: *through,
                    }
                } else {
                    SubscriptionOutcome::Rejected(SubscriptionRejection::Conflict)
                }
            }
            SubscriptionRequest::Renew { lease_ms, .. } => {
                if Duration::from_millis(*lease_ms) > self.limits.max_lease {
                    SubscriptionOutcome::Rejected(SubscriptionRejection::Limit)
                } else {
                    live.lease_until = Instant::now() + Duration::from_millis(*lease_ms);
                    SubscriptionOutcome::Renewed {
                        watch: entry.identity.clone(),
                        lease_ms: *lease_ms,
                    }
                }
            }
            SubscriptionRequest::Unsubscribe { .. } => {
                return Ok((
                    Record {
                        revision,
                        status: Status::Closed,
                    },
                    SubscriptionOutcome::Closed {
                        watch: entry.identity.clone(),
                    },
                ));
            }
        };
        Ok((
            Record {
                revision,
                status: Status::Live(live),
            },
            outcome,
        ))
    }
    fn terminal(
        &self,
        entry: &Entry,
        revision: u64,
        reason: SubscriptionResync,
        budget: &Budget,
    ) -> Result<bool, ReadError> {
        loop {
            if let Some(mut record) = entry.state.try_lock_for(budget.wait_quantum()?) {
                if record.revision != revision {
                    return Ok(false);
                }
                record.revision = record
                    .revision
                    .checked_add(1)
                    .ok_or(ReadError::Unavailable)?;
                record.status = Status::Resync(reason);
                entry.active.store(false, Ordering::Release);
                return Ok(true);
            }
        }
    }
    fn publish(
        &self,
        entry: &Entry,
        revision: u64,
        observed: GraphState,
        next: Record,
        session: &SubscriptionSession,
        budget: &Budget,
    ) -> Result<Publication, ReadError> {
        let graph = self.reads.graph();
        graph
            .read_for(Duration::ZERO, |graph| {
                if graph.state() != observed {
                    return Ok(Publication::Retry);
                }
                // Registry reservation and record publication share one short
                // critical section. No replacement can install IDs before its
                // additional permanent storage is admitted.
                let Some(mut table) = self.records.try_lock() else {
                    return Ok(Publication::Retry);
                };
                if !table
                    .entries
                    .get(&entry.identity.watch)
                    .is_some_and(|current| std::ptr::eq(Arc::as_ptr(current), entry))
                {
                    return Ok(Publication::Stopped(SubscriptionResync::Unknown));
                }
                let Some(mut record) = entry.state.try_lock() else {
                    return Ok(Publication::Retry);
                };
                if record.revision != revision {
                    return Ok(Publication::Retry);
                }
                budget.check()?;
                if self.stopped(budget) {
                    return Ok(Publication::Stopped(SubscriptionResync::Shutdown));
                }
                if let Some(reason) = session.reason() {
                    return Ok(Publication::Stopped(reason));
                }
                if let Status::Live(live) = &record.status
                    && Instant::now() >= live.lease_until
                {
                    return Ok(Publication::Retry);
                }
                let reserved = entry.reservation.load(Ordering::Relaxed);
                let required = match &next.status {
                    Status::Live(live) => entry
                        .base_reservation
                        .saturating_add(membership_bytes(&live.ids)),
                    _ => reserved,
                };
                let growth = required.saturating_sub(reserved);
                if growth
                    > self
                        .limits
                        .max_retained_bytes
                        .saturating_sub(table.reserved_bytes)
                {
                    return Ok(Publication::Capacity);
                }
                table.reserved_bytes += growth;
                entry
                    .reservation
                    .store(reserved + growth, Ordering::Relaxed);
                entry
                    .active
                    .store(matches!(next.status, Status::Live(_)), Ordering::Release);
                *record = next;
                Ok(Publication::Done)
            })
            .unwrap_or(Ok(Publication::Retry))
    }
    fn maintain(&self) {
        // The registry has an admitted finite entry ceiling. Maintenance never
        // waits for an entry lock or acquires graph/read permits.
        let Some(mut table) = self.records.try_lock() else {
            return;
        };
        let mut remove = Vec::new();
        for (id, entry) in &table.entries {
            if !entry.session.is_active() {
                remove.push(*id);
                continue;
            }
            let Some(mut record) = entry.state.try_lock() else {
                continue;
            };
            if let Status::Live(live) = &record.status
                && Instant::now() >= live.lease_until
            {
                record.status = Status::Resync(SubscriptionResync::LeaseExpired);
                record.revision = record.revision.saturating_add(1);
                entry.active.store(false, Ordering::Release);
            }
            if entry.legacy && !entry.active.load(Ordering::Acquire) {
                remove.push(*id);
            }
        }
        for id in remove {
            if let Some(entry) = table.entries.remove(&id) {
                table
                    .bindings
                    .remove(&(entry.session.id(), entry.identity.key.clone()));
                table.reserved_bytes = table
                    .reserved_bytes
                    .saturating_sub(entry.reservation.load(Ordering::Relaxed));
            }
        }
    }
}
// The original canonical creation remains reserved for the session lifetime;
// current membership additionally owns one string allocation per retained ID.
fn membership_bytes(ids: &[String]) -> usize {
    ids.iter().fold(64usize, |total, id| {
        total
            .saturating_add(std::mem::size_of::<String>())
            .saturating_add(id.len())
    })
}
fn execution_overflow(error: &ReadError) -> bool {
    matches!(
        error,
        ReadError::Budget(
            BudgetKind::Work
                | BudgetKind::Retained
                | BudgetKind::Values
                | BudgetKind::Depth
                | BudgetKind::Rows
                | BudgetKind::Output
                | BudgetKind::Candidates
                | BudgetKind::Forward
                | BudgetKind::Inverse
        )
    )
}
fn normalize(ids: &[String]) -> Vec<String> {
    let mut ids = ids.to_vec();
    ids.sort();
    ids.dedup();
    ids
}
fn same_projection(left: &Projection, right: &Projection) -> bool {
    left.len() == right.len()
        && left.iter().all(|(id, row)| {
            right
                .get(id)
                .is_some_and(|other| row.canonical == other.canonical)
        })
}
pub(crate) struct SubscriptionResource(StateSubscriptionService);
impl ApplicationResource for SubscriptionResource {
    fn name(&self) -> &str {
        "state subscriptions"
    }
    fn initialize(&mut self, context: ResourceContext) -> ResourceFuture<'_, ReadyInfo> {
        let service = self.0.clone();
        Box::pin(async move {
            let closing = context.cancellation();
            context.spawn("subscription maintenance",async move{
            let mut tick=tokio::time::interval(service.inner.limits.maintenance_interval);
            loop{tokio::select!{biased;_=closing.cancelled()=>{service.shutdown();return Ok(())},_=tick.tick()=>service.inner.maintain()}}
        })?;
            Ok(ReadyInfo::default())
        })
    }
    fn rollback_start(&mut self) -> ResourceFuture<'_> {
        self.0.shutdown();
        Box::pin(async { Ok(()) })
    }
    fn close(&mut self) -> ResourceFuture<'_> {
        self.0.shutdown();
        Box::pin(async { Ok(()) })
    }
}

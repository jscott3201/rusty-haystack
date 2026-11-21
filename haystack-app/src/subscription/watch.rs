//! Exact-session typed compatibility watches. Their implicit poll fence is
//! separate from the native explicit-ACK protocol and legacy attachments.
use super::*;
use crate::{ApiError, registry::Handler};
use haystack_core::{
    data::{HCol, HGrid},
    graph::{DiffOp, GraphDiff},
    kinds::Number,
};
use std::collections::BTreeSet;

pub(crate) enum Request {
    Sub {
        watch: Option<String>,
        ids: Vec<String>,
        lease: Option<Duration>,
    },
    Unsub {
        watch: String,
        ids: Vec<String>,
        close: bool,
    },
    Poll {
        watch: String,
        refresh: bool,
        current: bool,
    },
}
impl Request {
    pub(crate) fn decode(
        handler: Handler,
        args: &HDict,
        budget: &mut Budget,
    ) -> Result<(&'static str, Self), ApiError> {
        if handler == Handler::WatchPoll {
            let watch = string(args.get("watchId"))?.ok_or(ApiError::InvalidArgs)?;
            return Ok((
                "ph.api::watchPoll",
                Self::Poll {
                    watch,
                    refresh: marker(args.get("refresh"))?,
                    current: marker(args.get("curValSub"))?,
                },
            ));
        }
        let Some(Kind::Grid(grid)) = args.get("req") else {
            return Err(ApiError::InvalidArgs);
        };
        let subscribe = handler == Handler::WatchSub;
        let allowed = if subscribe {
            &["watchId", "watchDis", "lease"][..]
        } else {
            &["watchId", "close"][..]
        };
        if grid.meta.tag_names().any(|name| !allowed.contains(&name))
            || grid.cols.len() > 1
            || grid.cols.iter().any(|col| {
                col.name != "id"
                    || col.meta.tag_names().any(|name| name != "of")
                    || col
                        .meta
                        .get("of")
                        .is_some_and(|of| !matches!(of,Kind::Ref(id) if id.val == "sys::Ref"))
            })
            || (!grid.rows.is_empty() && grid.cols.is_empty())
            || grid.rows.len() > wire::MAX_IDS
        {
            return Err(ApiError::InvalidArgs);
        }
        let mut ids = Vec::new();
        budget.charge(BudgetKind::Retained, grid.rows.len().saturating_mul(128))?;
        for row in &grid.rows {
            if row.tag_names().any(|name| name != "id") {
                return Err(ApiError::InvalidArgs);
            }
            let Some(Kind::Ref(id)) = row.get("id") else {
                return Err(ApiError::InvalidArgs);
            };
            if id.val.is_empty() || id.val.len() > 1024 || id.val.chars().any(char::is_control) {
                return Err(ApiError::InvalidArgs);
            }
            ids.push(budget.copy_string(&id.val)?);
        }
        let watch = string(grid.meta.get("watchId"))?;
        if subscribe {
            let display = string(grid.meta.get("watchDis"))?;
            if watch.is_none() && display.is_none() {
                return Err(ApiError::InvalidArgs);
            }
            let lease = match grid.meta.get("lease") {
                None => None,
                Some(Kind::Number(number)) => {
                    let seconds = number
                        .convert_to("s")
                        .map_err(|_| ApiError::InvalidArgs)?
                        .val;
                    if !seconds.is_finite() || seconds <= 0.0 {
                        return Err(ApiError::InvalidArgs);
                    }
                    let duration =
                        Duration::try_from_secs_f64(seconds).map_err(|_| ApiError::InvalidArgs)?;
                    if duration.is_zero() {
                        return Err(ApiError::InvalidArgs);
                    }
                    Some(duration)
                }
                _ => return Err(ApiError::InvalidArgs),
            };
            Ok(("ph.api::watchSub", Self::Sub { watch, ids, lease }))
        } else {
            Ok((
                "ph.api::watchUnsub",
                Self::Unsub {
                    watch: watch.ok_or(ApiError::InvalidArgs)?,
                    ids,
                    close: marker(grid.meta.get("close"))?,
                },
            ))
        }
    }
    fn watch(&self) -> Option<&str> {
        match self {
            Self::Sub { watch, .. } => watch.as_deref(),
            Self::Unsub { watch, .. } | Self::Poll { watch, .. } => Some(watch),
        }
    }
    fn cleanup(&self) -> bool {
        matches!(self, Self::Unsub { .. })
    }
    fn ids(&self) -> &[String] {
        match self {
            Self::Sub { ids, .. } | Self::Unsub { ids, .. } => ids,
            Self::Poll { .. } => &[],
        }
    }
}
fn string(value: Option<&Kind>) -> Result<Option<String>, ApiError> {
    match value {
        None => Ok(None),
        Some(Kind::Str(text)) if text.len() <= 1024 => Ok(Some(text.clone())),
        _ => Err(ApiError::InvalidArgs),
    }
}
fn marker(value: Option<&Kind>) -> Result<bool, ApiError> {
    match value {
        None | Some(Kind::Null) => Ok(false),
        Some(Kind::Marker) => Ok(true),
        _ => Err(ApiError::InvalidArgs),
    }
}

#[derive(Clone)]
struct Member {
    active: bool,
    enrolled: u64,
}
#[derive(Clone)]
pub(super) struct Live {
    members: Arc<BTreeMap<String, Member>>,
    observed: GraphState,
    policy: Arc<str>,
    projection: Arc<Projection>,
    poll_revision: u64,
    first_poll: bool,
    lease: Duration,
    pub(super) lease_until: Instant,
}
impl Live {
    pub(super) fn renew_at_publication(&mut self, session: &SubscriptionSession) {
        let until = Instant::now() + self.lease;
        self.lease_until = session.expires_at().map_or(until, |end| until.min(end));
    }
    pub(super) fn membership_bytes(&self) -> usize {
        self.members
            .keys()
            .fold(64, |n, id| n.saturating_add(id.len()).saturating_add(256))
    }
    fn previous(&self) -> super::Live {
        super::Live {
            ids: self.members.keys().cloned().collect::<Vec<_>>().into(),
            graph: self.observed,
            policy: self.policy.clone(),
            scope: 1,
            acknowledged: self.poll_revision,
            projection: self.projection.clone(),
            prepared: None,
            last_ack: None,
            lease_until: self.lease_until,
        }
    }
}
pub(super) struct CaptureContext<'a> {
    previous: Option<&'a Live>,
    ids: &'a [String],
    policy_scope: &'a str,
}
#[derive(Default)]
pub(super) struct Captured {
    pub(super) authorized: BTreeSet<String>,
    changed: BTreeSet<String>,
    deleted: bool,
}
impl CaptureContext<'_> {
    pub(super) fn matches_policy(&self, policy: &dyn PolicySnapshot) -> bool {
        policy.scope_key() == self.policy_scope
    }
    pub(super) fn existing(&self, id: &str) -> bool {
        self.previous
            .is_some_and(|live| live.members.contains_key(id))
    }
    pub(super) fn observe(
        &self,
        diff: &GraphDiff,
        capture: &mut Captured,
        budget: &mut Budget,
    ) -> Result<(), ReadError> {
        budget.charge(BudgetKind::Work, diff.ref_val.len().saturating_add(1))?;
        // Enrollment guards deletion history, but deliberately not update
        // history: a newly added member may have changed after the last poll.
        if diff.op == DiffOp::Remove
            && self
                .previous
                .and_then(|live| live.members.get(&diff.ref_val))
                .is_some_and(|member| member.active && diff.version > member.enrolled)
        {
            capture.deleted = true;
        }
        if self.ids.binary_search(&diff.ref_val).is_ok() && !capture.changed.contains(&diff.ref_val)
        {
            budget.charge(BudgetKind::Retained, diff.ref_val.len().saturating_add(64))?;
            capture.changed.insert(budget.copy_string(&diff.ref_val)?);
        }
        Ok(())
    }
}
fn id_bytes(watch: &str) -> Result<[u8; 16], ApiError> {
    if watch.len() != 32
        || !watch
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(ApiError::InvalidArgs);
    }
    let mut id = [0; 16];
    for (i, byte) in id.iter_mut().enumerate() {
        *byte =
            u8::from_str_radix(&watch[i * 2..i * 2 + 2], 16).map_err(|_| ApiError::InvalidArgs)?;
    }
    Ok(id)
}
fn id_text(id: [u8; 16]) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

impl StateSubscriptionService {
    pub(crate) fn typed_watch<T>(
        &self,
        principal: &Principal,
        session: &SubscriptionSession,
        request: Request,
        policy_scope: &str,
        budget: &mut Budget,
        mut encode: impl FnMut(HGrid, &mut Budget) -> Result<T, ApiError>,
    ) -> Result<T, ApiError> {
        self.inner.watch(
            principal,
            session,
            request,
            policy_scope,
            budget,
            &mut encode,
        )
    }
}
impl SubscriptionInner {
    fn check_watch_authority(
        &self,
        principal: &Principal,
        session: &SubscriptionSession,
        policy_scope: &str,
        budget: &Budget,
    ) -> Result<(), ApiError> {
        budget.check()?;
        if self.stopped(budget) {
            return Err(ApiError::Unavailable);
        }
        if !session.scoped() || !session.matches(principal) || !session.is_active() {
            return Err(ApiError::Permission);
        }
        let policy = self.reads.policy_snapshot(principal)?;
        if policy.scope_key() != policy_scope
            || !policy.operation(ReadOperation::Subscriptions)
            || !policy.operation(ReadOperation::Read)
        {
            return Err(ApiError::Permission);
        }
        Ok(())
    }
    fn missing_cleanup(
        &self,
        entry: &Entry,
        principal: &Principal,
        session: &SubscriptionSession,
        policy_scope: &str,
        budget: &Budget,
    ) -> Result<(), ApiError> {
        // Unknown also covers a different entry occupying the registry slot.
        // Only absence of the already-authorized identity is idempotent cleanup.
        if self
            .registry(budget)?
            .entries
            .contains_key(&entry.identity.watch)
        {
            return Err(ApiError::Unavailable);
        }
        self.check_watch_authority(principal, session, policy_scope, budget)
    }
    fn watch_reserve(
        self: &Arc<Self>,
        principal: &Principal,
        session: &SubscriptionSession,
        request: &Request,
        budget: &mut Budget,
    ) -> Result<(Arc<Entry>, Reservation), ApiError> {
        let mut table = self.registry(budget)?;
        budget.charge(BudgetKind::Work, table.entries.len().saturating_add(1))?;
        let active = table
            .entries
            .values()
            .filter(|entry| entry.active.load(Ordering::Acquire))
            .count();
        let own = table
            .entries
            .values()
            .filter(|entry| entry.session.same_session(session))
            .count();
        let own_active = table
            .entries
            .values()
            .filter(|entry| {
                entry.session.same_session(session) && entry.active.load(Ordering::Acquire)
            })
            .count();
        let base = self
            .limits
            .max_view_bytes
            .saturating_mul(12)
            .saturating_add(self.limits.max_delivery_bytes.saturating_mul(2))
            .saturating_add(principal.bytes())
            .saturating_add(8192);
        let reserved = base.saturating_add(request.ids().iter().fold(64usize, |n, id| {
            n.saturating_add(id.len()).saturating_add(256)
        }));
        if active >= self.limits.max_watches
            || own_active >= self.limits.max_watches_per_session
            || table.entries.len() >= self.limits.max_bindings
            || own >= self.limits.max_bindings_per_session
            || reserved
                > self
                    .limits
                    .max_retained_bytes
                    .saturating_sub(table.reserved_bytes)
        {
            return Err(ApiError::Unavailable);
        }
        budget.charge(BudgetKind::Retained, 4096)?;
        let watch = rand::random();
        let key = id_text(watch);
        let entry = Arc::new(Entry {
            identity: SubscriptionId {
                authority: self.authority,
                watch,
                key: key.clone(),
            },
            session: session.clone(),
            canonical: Arc::from([]),
            base_reservation: base,
            reservation: AtomicUsize::new(reserved),
            legacy: false,
            typed_watch: true,
            connection: None,
            active: AtomicBool::new(true),
            state: Mutex::new(Record {
                revision: 0,
                status: Status::Preparing,
            }),
        });
        table.entries.insert(watch, entry.clone());
        table.bindings.insert((session.id(), key), watch);
        table.reserved_bytes += reserved;
        let reservation = Reservation {
            owner: self.clone(),
            entry: entry.clone(),
            published: false,
        };
        Ok((entry, reservation))
    }
    fn watch<T>(
        self: &Arc<Self>,
        principal: &Principal,
        session: &SubscriptionSession,
        request: Request,
        policy_scope: &str,
        budget: &mut Budget,
        encode: &mut impl FnMut(HGrid, &mut Budget) -> Result<T, ApiError>,
    ) -> Result<T, ApiError> {
        budget.check()?;
        if !session.scoped() || !session.matches(principal) || !session.is_active() {
            return Err(ApiError::Permission);
        }
        if request.ids().len() > self.limits.max_ids {
            return Err(ApiError::InvalidArgs);
        }
        let mut reservation = None;
        let entry = if let Some(watch) = request.watch() {
            let id = id_bytes(watch)?;
            let table = self.registry(budget)?;
            let entry = table.entries.get(&id).cloned();
            drop(table);
            match entry {
                Some(entry) if !entry.typed_watch || !entry.session.same_session(session) => {
                    return Err(ApiError::Permission);
                }
                Some(entry) => entry,
                None if request.cleanup() => {
                    let encoded = encode(HGrid::new(), budget)?;
                    self.check_watch_authority(principal, session, policy_scope, budget)?;
                    return Ok(encoded);
                }
                None => return Err(ApiError::Unavailable),
            }
        } else {
            let (entry, guard) = self.watch_reserve(principal, session, &request, budget)?;
            reservation = Some(guard);
            entry
        };
        loop {
            budget.check()?;
            if self.stopped(budget) {
                return Err(ApiError::Unavailable);
            }
            if !session.is_active() {
                return Err(ApiError::Permission);
            }
            let record = Self::record(&entry, budget)?;
            if matches!(&record.status, Status::Watch(live) if Instant::now() >= live.lease_until) {
                if !self.terminal(
                    &entry,
                    record.revision,
                    SubscriptionResync::LeaseExpired,
                    budget,
                )? {
                    continue;
                }
                if request.cleanup() {
                    let encoded = encode(HGrid::new(), budget)?;
                    self.check_watch_authority(principal, session, policy_scope, budget)?;
                    return Ok(encoded);
                }
                return Err(ApiError::Unavailable);
            }
            let previous = match &record.status {
                Status::Watch(live) if Instant::now() < live.lease_until => Some(live),
                Status::Preparing if reservation.is_some() => None,
                _ if request.cleanup() => {
                    let encoded = encode(HGrid::new(), budget)?;
                    self.check_watch_authority(principal, session, policy_scope, budget)?;
                    return Ok(encoded);
                }
                _ => return Err(ApiError::Unavailable),
            };
            if let (Some(_), Request::Unsub { close: true, .. }) = (previous, &request) {
                let encoded = encode(HGrid::new(), budget)?;
                self.check_watch_authority(principal, session, policy_scope, budget)?;
                // Cleanup does not require the old graph projection to remain
                // valid. Observe the current graph while retaining catalog pin.
                let graph = self.reads.graph();
                let current = loop {
                    if let Some(state) = graph.read_for(budget.wait_quantum()?, |g| {
                        budget.check_catalog(g).map(|_| g.state())
                    }) {
                        break state?;
                    }
                };
                let next = Record {
                    revision: record
                        .revision
                        .checked_add(1)
                        .ok_or(ApiError::Unavailable)?,
                    status: Status::Closed,
                };
                match self.publish(&entry, record.revision, current, next, session, budget)? {
                    Publication::Done => return Ok(encoded),
                    Publication::Retry => continue,
                    Publication::Stopped(SubscriptionResync::Unknown) => {
                        self.missing_cleanup(&entry, principal, session, policy_scope, budget)?;
                        return Ok(encoded);
                    }
                    _ => return Err(ApiError::Unavailable),
                }
            }
            // Reserve membership cloning/normalization and tree nodes before
            // copying any retained ID. Missing inactive IDs count as members.
            let membership_source = previous.map_or(0, Live::membership_bytes).saturating_add(
                request
                    .ids()
                    .iter()
                    .map(|id| id.len().saturating_add(256))
                    .sum::<usize>(),
            );
            budget.charge(BudgetKind::Work, membership_source)?;
            budget.charge(
                BudgetKind::Retained,
                membership_source.saturating_mul(4).saturating_add(2048),
            )?;
            let mut all: BTreeSet<String> =
                previous.map_or_else(BTreeSet::new, |live| live.members.keys().cloned().collect());
            if let Request::Sub { ids, .. } = &request {
                all.extend(ids.iter().cloned());
            }
            if all.len() > self.limits.max_ids {
                return Err(ApiError::InvalidArgs);
            }
            let ids: Vec<_> = all.into_iter().collect();
            budget.charge(BudgetKind::Work, ids.iter().map(|id| id.len() + 1).sum())?;
            budget.charge(
                BudgetKind::Retained,
                ids.iter().map(|id| id.len() + 256).sum(),
            )?;
            let native_previous = previous.map(Live::previous);
            let context = CaptureContext {
                previous,
                ids: &ids,
                policy_scope,
            };
            let captured = match self.capture_watch(
                principal,
                &ids,
                native_previous.as_ref(),
                Some(&context),
                budget,
            ) {
                Ok(capture) => capture,
                Err(CaptureError::Resync(reason)) => {
                    if previous.is_some()
                        && !self.terminal(&entry, record.revision, reason, budget)?
                    {
                        continue;
                    }
                    return Err(if reason == SubscriptionResync::Policy {
                        ApiError::Permission
                    } else {
                        ApiError::Unavailable
                    });
                }
                Err(CaptureError::Read(error)) => {
                    if previous.is_some()
                        && execution_overflow(&error)
                        && !self.terminal(
                            &entry,
                            record.revision,
                            SubscriptionResync::Overflow,
                            budget,
                        )?
                    {
                        continue;
                    }
                    return Err(error.into());
                }
            };
            let details = captured.watch.as_ref().expect("watch capture");
            if details.deleted {
                if !self.terminal(
                    &entry,
                    record.revision,
                    SubscriptionResync::ScopeChanged,
                    budget,
                )? {
                    continue;
                }
                return Err(ApiError::Unavailable);
            }
            let lease = match &request {
                Request::Sub { lease, .. } => {
                    lease.unwrap_or(previous.map_or(Duration::from_secs(60), |live| live.lease))
                }
                _ => previous.expect("existing watch").lease,
            }
            .min(self.limits.max_lease);
            let lease = lease.min(
                session
                    .expires_at()
                    .map_or(lease, |end| end.saturating_duration_since(Instant::now())),
            );
            if lease.is_zero() {
                return Err(ApiError::Permission);
            }
            let mut live = previous.cloned().unwrap_or_else(|| Live {
                members: Arc::new(BTreeMap::new()),
                observed: captured.graph,
                policy: captured.policy.clone(),
                projection: Arc::new(Projection::new()),
                poll_revision: captured.graph.revision,
                first_poll: true,
                lease,
                lease_until: Instant::now() + lease,
            });
            let mut members = (*live.members).clone();
            let watch = id_text(entry.identity.watch);
            let result = match &request {
                Request::Sub { ids, .. } => {
                    for id in ids {
                        if details.authorized.contains(id) && !members.contains_key(id) {
                            members.insert(
                                id.clone(),
                                Member {
                                    active: captured.projection.contains_key(id),
                                    enrolled: captured.graph.revision,
                                },
                            );
                        }
                    }
                    let rows = ids
                        .iter()
                        .map(|id| copy_row(captured.projection.get(id), budget))
                        .collect::<Result<Vec<_>, _>>()?;
                    let mut grid = watch_grid(rows, Some(&watch), false, budget)?;
                    grid.meta.set(
                        "lease",
                        Kind::Number(Number::new(lease.as_secs_f64(), Some("s".into()))),
                    );
                    grid
                }
                Request::Unsub { ids, .. } => {
                    for id in ids {
                        members.remove(id);
                    }
                    HGrid::new()
                }
                Request::Poll {
                    refresh, current, ..
                } => {
                    let rows = members
                        .iter()
                        .filter(|(id, member)| {
                            member.active
                                && (*refresh || live.first_poll || details.changed.contains(*id))
                        })
                        .filter_map(|(id, _)| captured.projection.get(id))
                        .map(|row| copy_row(Some(row), budget))
                        .collect::<Result<Vec<_>, _>>()?;
                    live.poll_revision = captured.graph.revision;
                    live.first_poll = false;
                    watch_grid(rows, Some(&watch), *current, budget)?
                }
            };
            live.members = Arc::new(members);
            live.observed = captured.graph;
            live.policy = captured.policy.clone();
            // Removed members must not remain in later reauthorization state.
            live.projection = Arc::new(
                captured
                    .projection
                    .iter()
                    .filter(|(id, _)| live.members.contains_key(*id))
                    .map(|(id, row)| (id.clone(), row.clone()))
                    .collect(),
            );
            live.lease = lease;
            live.lease_until = Instant::now() + lease;
            let next = Record {
                revision: record
                    .revision
                    .checked_add(1)
                    .ok_or(ApiError::Unavailable)?,
                status: Status::Watch(live),
            };
            let encoded = encode(result, budget)?;
            let policy = self.reads.policy_snapshot(principal)?;
            if policy.scope_key() != captured.policy.as_ref()
                || !policy.operation(ReadOperation::Subscriptions)
                || !policy.operation(ReadOperation::Read)
            {
                if !self.terminal(&entry, record.revision, SubscriptionResync::Policy, budget)? {
                    continue;
                }
                return Err(ApiError::Permission);
            }
            match self.publish(
                &entry,
                record.revision,
                captured.graph,
                next,
                session,
                budget,
            )? {
                Publication::Done => {
                    if let Some(guard) = &mut reservation {
                        guard.published = true;
                    }
                    return Ok(encoded);
                }
                Publication::Retry => continue,
                Publication::Stopped(SubscriptionResync::Unknown) if request.cleanup() => {
                    self.missing_cleanup(&entry, principal, session, policy_scope, budget)?;
                    return Ok(encoded);
                }
                Publication::Capacity | Publication::Stopped(_) => {
                    return Err(ApiError::Unavailable);
                }
            }
        }
    }
}
fn copy_row(row: Option<&ProjectedRow>, budget: &mut Budget) -> Result<HDict, ApiError> {
    let bytes = row.map_or(64, |row| row.canonical.len().saturating_add(512));
    budget.charge(BudgetKind::Work, bytes.saturating_mul(2))?;
    budget.charge(BudgetKind::Retained, bytes.saturating_mul(8))?;
    Ok(row.map_or_else(HDict::new, |row| (*row.row).clone()))
}
fn watch_grid(
    mut rows: Vec<HDict>,
    watch: Option<&str>,
    current: bool,
    budget: &mut Budget,
) -> Result<HGrid, ApiError> {
    if rows.len() > budget.limits.max_rows {
        return Err(ApiError::InvalidArgs);
    }
    budget.charge(BudgetKind::Work, rows.len().saturating_mul(16))?;
    budget.charge(
        BudgetKind::Retained,
        rows.len().saturating_mul(1024).saturating_add(1024),
    )?;
    let cols = if current {
        for row in &mut rows {
            let mut selected = HDict::new();
            for key in ["id", "curVal", "curStatus"] {
                if let Some(value) = row.get(key) {
                    selected.set(key, value.clone());
                }
            }
            *row = selected;
        }
        ["id", "curVal", "curStatus"]
            .into_iter()
            .map(HCol::new)
            .collect()
    } else {
        let mut names = BTreeSet::from(["id".to_owned()]);
        for row in &rows {
            names.extend(row.tag_names().map(str::to_owned));
        }
        names.into_iter().map(HCol::new).collect()
    };
    let mut meta = HDict::new();
    if let Some(id) = watch {
        meta.set("watchId", Kind::Str(id.into()));
    }
    Ok(HGrid::from_parts(meta, cols, rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AllowAll, ReadLimits, ReadPolicy};
    use haystack_core::{
        graph::{EntityGraph, SharedGraph},
        kinds::HRef,
        ontology::DefNamespace,
    };
    struct Fixture {
        service: StateSubscriptionService,
        session: SubscriptionSession,
        graph: SharedGraph,
    }
    impl Fixture {
        fn new(policy: Arc<dyn ReadPolicy>, limits: SubscriptionLimits, capacity: usize) -> Self {
            let graph = SharedGraph::new(EntityGraph::with_changelog_capacity(capacity));
            for id in ["a", "b"] {
                graph.add(row(id, 1.0)).unwrap();
            }
            let reads = ReadService::new(graph.clone(), policy, ReadLimits::default()).unwrap();
            Self {
                service: StateSubscriptionService::new(
                    reads,
                    EntityMutationStore::new(graph.clone()),
                    limits,
                )
                .unwrap(),
                session: SubscriptionSession::trusted("owner", Duration::from_secs(30)).unwrap(),
                graph,
            }
        }
        async fn run(&self, request: Request) -> Result<HGrid, ApiError> {
            self.run_with(request, |grid, _| Ok(grid)).await
        }
        async fn run_with(
            &self,
            request: Request,
            encode: impl FnMut(HGrid, &mut Budget) -> Result<HGrid, ApiError> + Send + 'static,
        ) -> Result<HGrid, ApiError> {
            let mut admission = self
                .service
                .read_service()
                .begin(ReadContext::with_timeout(
                    self.session.principal().clone(),
                    Duration::from_secs(3),
                ))
                .await
                .unwrap();
            admission
                .bind_wire_session(self.session.principal().clone(), self.session.clone())
                .unwrap();
            let service = self.service.clone();
            let session = self.session.clone();
            let policy_scope = service
                .read_service()
                .policy_snapshot(session.principal())
                .unwrap()
                .scope_key()
                .to_owned();
            admission
                .run_task(move |principal, budget| {
                    Ok(service.typed_watch(
                        &principal,
                        &session,
                        request,
                        &policy_scope,
                        budget,
                        encode,
                    ))
                })
                .await
                .map_err(ApiError::from)?
        }
        fn entry(&self, watch: &str) -> Arc<Entry> {
            self.service
                .inner
                .records
                .lock()
                .entries
                .get(&id_bytes(watch).unwrap())
                .unwrap()
                .clone()
        }
    }
    fn row(id: &str, value: f64) -> HDict {
        let mut r = HDict::new();
        r.set("id", Kind::Ref(HRef::from_val(id)));
        r.set("curVal", Kind::Number(Number::unitless(value)));
        r.set("curStatus", Kind::Str("ok".into()));
        r.set("secret", Kind::Str("hidden".into()));
        r
    }
    fn sub(watch: Option<&str>, ids: &[&str]) -> Request {
        Request::Sub {
            watch: watch.map(str::to_owned),
            ids: ids.iter().map(|id| (*id).into()).collect(),
            lease: None,
        }
    }
    fn poll(watch: &str) -> Request {
        Request::Poll {
            watch: watch.into(),
            refresh: false,
            current: false,
        }
    }
    fn id(grid: &HGrid) -> String {
        match grid.meta.get("watchId").unwrap() {
            Kind::Str(id) => id.clone(),
            _ => panic!(),
        }
    }
    fn state(entry: &Entry) -> Live {
        match &entry.state.lock().status {
            Status::Watch(live) => live.clone(),
            _ => panic!("live watch expected"),
        }
    }

    #[tokio::test]
    async fn inactive_missing_members_consume_capacity_until_explicitly_removed() {
        let f = Fixture::new(
            Arc::new(AllowAll),
            SubscriptionLimits {
                max_ids: 1,
                ..SubscriptionLimits::default()
            },
            20,
        );
        let watch = id(&f.run(sub(None, &["missing"])).await.unwrap());
        assert!(matches!(
            f.run(sub(Some(&watch), &["a"])).await,
            Err(ApiError::InvalidArgs)
        ));
        assert!(f.run(poll(&watch)).await.unwrap().rows.is_empty());
        f.run(Request::Unsub {
            watch: watch.clone(),
            ids: vec!["missing".into()],
            close: false,
        })
        .await
        .unwrap();
        assert_eq!(
            f.run(sub(Some(&watch), &["a"])).await.unwrap().rows.len(),
            1
        );
        assert_eq!(
            f.run(Request::Poll {
                watch,
                refresh: true,
                current: false,
            })
            .await
            .unwrap()
            .rows
            .len(),
            1
        );
    }

    #[tokio::test]
    async fn assigned_lease_caps_and_successful_poll_add_empty_remove_renew_without_fence_reset() {
        let fixture = Fixture::new(
            Arc::new(AllowAll),
            SubscriptionLimits {
                max_lease: Duration::from_secs(2),
                ..SubscriptionLimits::default()
            },
            32,
        );
        let created = fixture
            .run(Request::Sub {
                watch: None,
                ids: vec!["a".into()],
                lease: Some(Duration::from_secs(600)),
            })
            .await
            .unwrap();
        assert_eq!(
            created.meta.get("lease"),
            Some(&Kind::Number(Number::new(2.0, Some("s".into()))))
        );
        let watch = id(&created);
        let entry = fixture.entry(&watch);
        fixture.run(poll(&watch)).await.unwrap();
        let fence = state(&entry).poll_revision;
        for request in [
            sub(Some(&watch), &["b"]),
            Request::Unsub {
                watch: watch.clone(),
                ids: vec![],
                close: false,
            },
            poll(&watch),
        ] {
            if let Status::Watch(live) = &mut entry.state.lock().status {
                live.lease_until = Instant::now() + Duration::from_millis(100);
            }
            fixture.run(request).await.unwrap();
            let live = state(&entry);
            assert!(live.lease_until > Instant::now() + Duration::from_secs(1));
            assert_eq!(live.poll_revision, fence);
        }
        if let Status::Watch(live) = &mut entry.state.lock().status {
            live.lease_until = Instant::now() - Duration::from_millis(1);
        }
        assert!(matches!(
            fixture.run(poll(&watch)).await,
            Err(ApiError::Unavailable)
        ));
        assert_eq!(fixture.service.active_watches(), 0);
        assert!(
            fixture
                .run(Request::Unsub {
                    watch,
                    ids: vec![],
                    close: false
                })
                .await
                .unwrap()
                .rows
                .is_empty()
        );
    }

    #[tokio::test]
    async fn prepublication_output_failure_preserves_create_membership_poll_and_lease_state() {
        let fixture = Fixture::new(Arc::new(AllowAll), SubscriptionLimits::default(), 32);
        assert!(matches!(
            fixture
                .run_with(sub(None, &["a"]), |_, _| Err(ApiError::NotAcceptable))
                .await,
            Err(ApiError::NotAcceptable)
        ));
        assert_eq!(fixture.service.active_watches(), 0);
        assert_eq!(fixture.service.binding_count(), 0);
        let watch = id(&fixture.run(sub(None, &["a"])).await.unwrap());
        let entry = fixture.entry(&watch);
        fixture.graph.update("a", row("a", 2.0)).unwrap();
        for request in [
            poll(&watch),
            sub(Some(&watch), &["b"]),
            Request::Unsub {
                watch: watch.clone(),
                ids: vec!["a".into()],
                close: false,
            },
            Request::Unsub {
                watch: watch.clone(),
                ids: vec![],
                close: true,
            },
        ] {
            let before = SelfRecord::from(&entry);
            assert!(matches!(
                fixture
                    .run_with(request, |_, _| Err(ApiError::InvalidArgs))
                    .await,
                Err(ApiError::InvalidArgs)
            ));
            assert_eq!(SelfRecord::from(&entry), before);
        }
        let result = fixture.run(poll(&watch)).await.unwrap();
        assert_eq!(result.rows.len(), 1);
        assert_eq!(
            result.rows[0].get("curVal"),
            Some(&Kind::Number(Number::unitless(2.0)))
        );
    }
    #[tokio::test]
    async fn graph_retry_recaptures_pending_changes_and_expiry_or_shutdown_cannot_publish() {
        let f = Fixture::new(Arc::new(AllowAll), SubscriptionLimits::default(), 32);
        let watch = id(&f.run(sub(None, &["a"])).await.unwrap());
        f.run(poll(&watch)).await.unwrap();
        let graph = f.graph.clone();
        let mut changed = false;
        let grid = f
            .run_with(poll(&watch), move |grid, _| {
                if !changed {
                    changed = true;
                    graph.update("a", row("a", 9.0)).unwrap();
                }
                Ok(grid)
            })
            .await
            .unwrap();
        assert_eq!(
            grid.rows[0].get("curVal"),
            Some(&Kind::Number(Number::unitless(9.0)))
        );
        assert!(f.run(poll(&watch)).await.unwrap().rows.is_empty());
        let entry = f.entry(&watch);
        assert!(matches!(
            f.run_with(poll(&watch), move |grid, _| {
                let mut record = entry.state.lock();
                let Status::Watch(live) = &mut record.status else {
                    panic!()
                };
                live.lease_until = Instant::now() - Duration::from_secs(1);
                Ok(grid)
            })
            .await,
            Err(ApiError::Unavailable)
        ));
        assert_eq!(f.service.active_watches(), 0);

        let f = Fixture::new(Arc::new(AllowAll), SubscriptionLimits::default(), 32);
        let service = f.service.clone();
        assert!(matches!(
            f.run_with(sub(None, &["a"]), move |grid, _| {
                service.shutdown();
                Ok(grid)
            })
            .await,
            Err(ApiError::Unavailable)
        ));
        assert_eq!(f.service.active_watches(), 0);
        assert_eq!(f.service.binding_count(), 0);
    }

    async fn malformed_poll_preserves_fence_and_lease(tail: &str) {
        let f = Fixture::new(
            Arc::new(AllowAll),
            SubscriptionLimits {
                max_lease: Duration::from_secs(2),
                ..SubscriptionLimits::default()
            },
            32,
        );
        let watch = id(&f.run(sub(None, &["a"])).await.unwrap());
        f.run(poll(&watch)).await.unwrap();
        f.graph.update("a", row("a", 5.0)).unwrap();
        let entry = f.entry(&watch);
        let before = SelfRecord::from(&entry);
        let mut admission = f
            .service
            .read_service()
            .begin(ReadContext::with_timeout(
                f.session.principal().clone(),
                Duration::from_secs(3),
            ))
            .await
            .unwrap();
        admission
            .bind_wire_session(f.session.principal().clone(), f.session.clone())
            .unwrap();
        let result = admission
            .invoke_wire_with_services(
                crate::TypedInvocationInput {
                    operation: "ph.api::watchPoll".into(),
                    post: true,
                    versions: vec!["5".into()],
                    content_types: vec!["text/zinc".into()],
                    accepts: vec!["application/json".into()],
                    body: format!("ver:\"3.0\"\nwatchId\n\"{watch}\"{tail}\n").into_bytes(),
                    ..Default::default()
                },
                None,
                None,
                Some(f.service.clone()),
            )
            .await;
        assert_eq!(
            SelfRecord::from(&entry),
            before,
            "malformed poll must not consume pending state or renew lease"
        );
        assert!(matches!(result, Err(ApiError::InvalidArgs)));
        let pending = f.run(poll(&watch)).await.unwrap();
        assert_eq!(
            pending.rows[0].get("curVal"),
            Some(&Kind::Number(Number::unitless(5.0)))
        );
    }
    #[tokio::test]
    async fn zinc_poll_surplus_cell_preserves_fence_and_lease() {
        malformed_poll_preserves_fence_and_lease(",999").await;
    }
    #[tokio::test]
    async fn zinc_poll_scalar_tail_preserves_fence_and_lease() {
        malformed_poll_preserves_fence_and_lease(" garbage").await;
    }

    async fn cleanup_racing_close_and_maintenance(close: bool) {
        let f = Arc::new(Fixture::new(
            Arc::new(AllowAll),
            SubscriptionLimits::default(),
            32,
        ));
        let watch = id(&f.run(sub(None, &["a"])).await.unwrap());
        let request = Request::Unsub {
            watch: watch.clone(),
            ids: vec!["a".into()],
            close,
        };
        let (entered, waiting) = tokio::sync::oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        let task_fixture = f.clone();
        let task = tokio::spawn(async move {
            let mut entered = Some(entered);
            task_fixture
                .run_with(request, move |grid, _| {
                    if let Some(entered) = entered.take() {
                        let _ = entered.send(());
                        // Dropping the sender releases this worker on assertion failure.
                        let _ = released.recv();
                    }
                    Ok(grid)
                })
                .await
        });
        tokio::time::timeout(Duration::from_secs(3), waiting)
            .await
            .unwrap()
            .unwrap();
        f.run(Request::Unsub {
            watch,
            ids: vec![],
            close: true,
        })
        .await
        .unwrap();
        f.service.inner.maintain();
        assert_eq!(f.service.active_watches(), 0);
        assert_eq!(f.service.binding_count(), 0);
        release.send(()).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap();
        assert!(
            result.is_ok(),
            "authorized cleanup after maintenance must be idempotent: {result:?}"
        );
        assert!(result.unwrap().rows.is_empty());
        assert_eq!(f.service.binding_count(), 0);
    }
    #[tokio::test]
    async fn close_racing_close_and_maintenance_is_idempotent() {
        cleanup_racing_close_and_maintenance(true).await;
    }
    #[tokio::test]
    async fn remove_racing_close_and_maintenance_is_idempotent() {
        cleanup_racing_close_and_maintenance(false).await;
    }

    #[tokio::test]
    async fn missing_cleanup_preserves_policy_session_budget_and_owner_fences() {
        for close in [true, false] {
            for failure in ["policy", "session", "cancel", "sealed", "shutdown"] {
                let changed = Arc::new(AtomicBool::new(false));
                let f = Fixture::new(
                    Arc::new(Policy(changed.clone())),
                    SubscriptionLimits::default(),
                    32,
                );
                let watch = id(&f.run(sub(None, &["a"])).await.unwrap());
                let entry = f.entry(&watch);
                let service = f.service.clone();
                let session = f.session.clone();
                let result = f
                    .run_with(
                        Request::Unsub {
                            watch,
                            ids: vec!["a".into()],
                            close,
                        },
                        move |grid, budget| {
                            // Simulate the already-tested concurrent closer's completed
                            // transition, then maintenance, at this deterministic seam.
                            {
                                let mut record = entry.state.lock();
                                record.revision += 1;
                                record.status = Status::Closed;
                                entry.active.store(false, Ordering::Release);
                            }
                            service.inner.maintain();
                            match failure {
                                "policy" => changed.store(true, Ordering::Release),
                                "session" => session.close(),
                                "cancel" => budget.cancel.cancel(),
                                "sealed" => {
                                    let sealed = crate::CancellationToken::new();
                                    sealed.cancel();
                                    budget.owner_sealed = Some(sealed);
                                }
                                "shutdown" => service.shutdown(),
                                _ => unreachable!(),
                            }
                            Ok(grid)
                        },
                    )
                    .await;
                assert!(result.is_err(), "close={close}, failure={failure}");
                // Cancellation may return before its blocking worker. Observe
                // actual admission release before inspecting retained state.
                tokio::time::timeout(Duration::from_secs(3), async {
                    while f.service.read_service().load().admitted != 0 {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
                assert_eq!(f.service.active_watches(), 0);
                assert_eq!(f.service.binding_count(), 0);
            }
        }
    }
    #[derive(Debug, PartialEq)]
    struct SelfRecord {
        revision: u64,
        ids: Vec<String>,
        poll: u64,
        first: bool,
        lease: Instant,
    }
    impl From<&Arc<Entry>> for SelfRecord {
        fn from(entry: &Arc<Entry>) -> Self {
            let record = entry.state.lock();
            let Status::Watch(live) = &record.status else {
                panic!()
            };
            Self {
                revision: record.revision,
                ids: live.members.keys().cloned().collect(),
                poll: live.poll_revision,
                first: live.first_poll,
                lease: live.lease_until,
            }
        }
    }

    struct Policy(Arc<AtomicBool>);
    struct Snapshot(bool);
    impl ReadPolicy for Policy {
        fn snapshot(&self, _: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
            Ok(Arc::new(Snapshot(self.0.load(Ordering::Acquire))))
        }
    }
    impl PolicySnapshot for Snapshot {
        fn scope_key(&self) -> &str {
            if self.0 { "revoke-a" } else { "deny-b" }
        }
        fn function(&self, _: &crate::FunctionIdentity) -> bool {
            true
        }
        fn operation(&self, _: ReadOperation) -> bool {
            true
        }
        fn entity(&self, id: &str) -> bool {
            if self.0 { id != "a" } else { id != "b" }
        }
        fn tag(&self, _: &str, tag: &str) -> bool {
            !matches!(tag, "secret" | "curStatus")
        }
        fn reference(&self, _: &str) -> bool {
            true
        }
        fn reference_display(&self, _: &str) -> bool {
            false
        }
        fn catalog(&self, _: crate::CatalogKind, _: &str) -> bool {
            true
        }
        fn nominal_provenance(&self, _: &haystack_core::kinds::NominalScalar) -> bool {
            true
        }
    }
    #[tokio::test]
    async fn newly_denied_ids_are_null_and_not_members_but_existing_authority_loss_is_terminal() {
        let revoke = Arc::new(AtomicBool::new(false));
        let fixture = Fixture::new(
            Arc::new(Policy(revoke.clone())),
            SubscriptionLimits::default(),
            32,
        );
        let created = fixture.run(sub(None, &["a", "b", "b"])).await.unwrap();
        assert_eq!(created.rows.len(), 3);
        assert!(created.rows[1].is_empty() && created.rows[2].is_empty());
        assert!(!created.rows[0].has("secret"));
        let watch = id(&created);
        let entry = fixture.entry(&watch);
        assert_eq!(state(&entry).members.len(), 1);
        let projected = fixture
            .run(Request::Poll {
                watch: watch.clone(),
                refresh: true,
                current: true,
            })
            .await
            .unwrap();
        assert_eq!(
            projected
                .cols
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["id", "curVal", "curStatus"]
        );
        assert!(!projected.rows[0].has("curStatus"));
        revoke.store(true, Ordering::Release);
        assert!(matches!(
            fixture.run(poll(&watch)).await,
            Err(ApiError::Permission)
        ));
        assert_eq!(fixture.service.active_watches(), 0);
        revoke.store(false, Ordering::Release);
        assert!(matches!(
            fixture
                .run(Request::Poll {
                    watch,
                    refresh: true,
                    current: false
                })
                .await,
            Err(ApiError::Unavailable)
        ));
    }

    #[tokio::test]
    async fn gaps_overflow_catalog_incarnation_and_deletion_require_recreation() {
        for cause in ["gap", "overflow", "catalog", "incarnation", "deletion"] {
            let fixture = Fixture::new(
                Arc::new(AllowAll),
                SubscriptionLimits {
                    max_change_diffs: if cause == "overflow" { 1 } else { 100 },
                    ..SubscriptionLimits::default()
                },
                if cause == "gap" { 2 } else { 32 },
            );
            let watch = id(&fixture.run(sub(None, &["a"])).await.unwrap());
            fixture.run(poll(&watch)).await.unwrap();
            match cause {
                "gap" => {
                    for n in 0..4 {
                        fixture.graph.update("b", row("b", f64::from(n))).unwrap();
                    }
                }
                "overflow" => {
                    for n in 0..2 {
                        fixture.graph.update("b", row("b", f64::from(n))).unwrap();
                    }
                }
                "catalog" => fixture.graph.set_namespace(DefNamespace::new()),
                "incarnation" => fixture.graph.write(|graph| {
                    *graph = EntityGraph::new();
                }),
                "deletion" => {
                    fixture.graph.remove("a").unwrap();
                    fixture.graph.add(row("a", 2.0)).unwrap();
                }
                _ => unreachable!(),
            }
            assert!(
                matches!(fixture.run(poll(&watch)).await, Err(ApiError::Unavailable)),
                "{cause}"
            );
            assert_eq!(fixture.service.active_watches(), 0, "{cause}");
            assert!(
                matches!(
                    fixture
                        .run(Request::Poll {
                            watch,
                            refresh: true,
                            current: false
                        })
                        .await,
                    Err(ApiError::Unavailable)
                ),
                "{cause}"
            );
        }
    }
}

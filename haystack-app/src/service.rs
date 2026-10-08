use crate::{
    budget::Budget,
    lifecycle::{ApplicationError, Lifecycle, WorkGuard},
    output,
    policy::{PolicySnapshot, ReadPolicy},
    sanitize::{self, View},
    types::*,
    wire,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use haystack_core::{
    data::HDict,
    filter::{self, CatalogKind, FilterError, FilterNode, FilterParseLimits},
    graph::SharedGraph,
    kinds::{Kind, Symbol},
    ontology::DefNamespace,
    xeto::Spec,
};
use hmac::{Hmac, KeyInit, Mac};
use parking_lot::{Mutex, MutexGuard};
use sha2::Sha256;
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Instant,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadLoad {
    pub admitted: usize,
    pub waiting: usize,
}

#[derive(Clone)]
pub struct ReadService {
    inner: Arc<Inner>,
}
struct Inner {
    lifecycle: Option<Arc<Lifecycle>>,
    graph: SharedGraph,
    dataset: [u8; 16],
    policy: Arc<dyn ReadPolicy>,
    limits: Arc<ReadLimits>,
    active: Arc<Semaphore>,
    waiting: Arc<Semaphore>,
    cursor_key: [u8; 32],
    cursors: Mutex<CursorTable>,
}
#[derive(Default)]
struct CursorTable {
    entries: HashMap<[u8; 16], CursorRecord>,
    bytes: usize,
}
struct CursorRecord {
    identity: Arc<RequestIdentity>,
    stamp: Stamp,
    after: String,
    expires: Instant,
    bytes: usize,
}
struct Prepared {
    identity: Arc<RequestIdentity>,
    cursor: Option<String>,
    retained_estimate: usize,
}
enum Input {
    Request(ReadRequest),
    Wire {
        operation: ReadOperation,
        body: Vec<u8>,
        input: H4Codec,
        output: H4Codec,
    },
}
impl Input {
    fn operation(&self) -> ReadOperation {
        match self {
            Self::Request(r) => r.query.operation(),
            Self::Wire { operation, .. } => *operation,
        }
    }
}
struct CancelOnDrop {
    token: CancellationToken,
    armed: bool,
}
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.token.cancel();
        }
    }
}

impl ReadService {
    /// Shares a graph and borrows the executing Tokio runtime. Creating another
    /// service creates another dataset/cursor authority; clones share authority.
    pub fn new(
        graph: SharedGraph,
        policy: Arc<dyn ReadPolicy>,
        limits: ReadLimits,
    ) -> Result<Self, ReadError> {
        limits.validate()?;
        Ok(Self {
            inner: Arc::new(Inner {
                lifecycle: None,
                graph,
                dataset: rand::random(),
                policy,
                active: Arc::new(Semaphore::new(limits.max_concurrent)),
                waiting: Arc::new(Semaphore::new(limits.max_queued)),
                limits: Arc::new(limits),
                cursor_key: rand::random(),
                cursors: Mutex::new(CursorTable::default()),
            }),
        })
    }
    pub(crate) fn managed(
        graph: SharedGraph,
        policy: Arc<dyn ReadPolicy>,
        limits: ReadLimits,
        lifecycle: Arc<Lifecycle>,
    ) -> Result<Self, ReadError> {
        let mut service = Self::new(graph, policy, limits)?;
        Arc::get_mut(&mut service.inner)
            .expect("new service")
            .lifecycle = Some(lifecycle);
        Ok(service)
    }
    /// Whether both handles share this exact service, including policy, cursor
    /// authority and admission. Independently constructed services differ even
    /// when they refer to the same graph.
    pub fn same_service(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
    /// Administrative graph handle for the owning application, outside the
    /// resource-policy API. Native callers already holding it remain trusted.
    pub fn graph(&self) -> SharedGraph {
        self.inner.graph.clone()
    }
    pub(crate) fn policy_snapshot(
        &self,
        principal: &Principal,
    ) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
        self.inner.policy.snapshot(principal)
    }
    pub fn limits(&self) -> &ReadLimits {
        &self.inner.limits
    }
    pub fn load(&self) -> ReadLoad {
        ReadLoad {
            admitted: self.inner.limits.max_concurrent - self.inner.active.available_permits(),
            waiting: self.inner.limits.max_queued - self.inner.waiting.available_permits(),
        }
    }
    pub async fn read(
        &self,
        context: ReadContext,
        request: ReadRequest,
    ) -> Result<ReadPage, ReadError> {
        self.run(context, Input::Request(request)).await
    }
    /// Decode under the same bounded admission/worker ownership as evaluation.
    /// Transport adapters must bound body collection before calling this method.
    pub async fn read_wire(
        &self,
        context: ReadContext,
        operation: ReadOperation,
        body: Vec<u8>,
        input: H4Codec,
        output: H4Codec,
    ) -> Result<ReadPage, ReadError> {
        self.run(
            context,
            Input::Wire {
                operation,
                body,
                input,
                output,
            },
        )
        .await
    }
    /// Register a logical request before body collection. Move this admission
    /// into `read_wire` after bounded collection; no second slot is acquired.
    pub async fn begin(&self, context: ReadContext) -> Result<ReadAdmission, ReadError> {
        let guard = self
            .inner
            .lifecycle
            .as_ref()
            .map(|life| life.admit().map_err(application_read_error))
            .transpose()?;
        self.begin_with_guard(context, guard).await
    }
    /// Continue an operation already admitted by this application's transport.
    /// The guard must belong to the exact application; no fresh admission is
    /// made during drain, and the registration moves into body/worker lifetime.
    pub async fn begin_admitted(
        &self,
        context: ReadContext,
        guard: WorkGuard,
    ) -> Result<ReadAdmission, ReadError> {
        if !self
            .inner
            .lifecycle
            .as_ref()
            .is_some_and(|life| Arc::ptr_eq(life, &guard.lifecycle))
        {
            return Err(ReadError::Forbidden);
        }
        self.begin_with_guard(context, Some(guard)).await
    }
    async fn begin_with_guard(
        &self,
        context: ReadContext,
        guard: Option<WorkGuard>,
    ) -> Result<ReadAdmission, ReadError> {
        let now = Instant::now();
        let deadline = context.deadline.min(now + self.inner.limits.max_duration);
        let cancel = context.cancellation.child_token();
        let mut budget = Budget::new(self.inner.limits.clone(), deadline, cancel);
        budget.owner_cancel = guard.as_ref().map(WorkGuard::cancellation);
        budget.owner_sealed = guard.as_ref().map(WorkGuard::closing);
        budget.check()?;
        let permit = self.admit(&budget).await?;
        Ok(ReadAdmission {
            inner: self.inner.clone(),
            principal: Some(context.principal),
            budget: Some(budget),
            permit: Some(permit),
            guard,
        })
    }
    async fn run(&self, context: ReadContext, input: Input) -> Result<ReadPage, ReadError> {
        self.begin(context).await?.run(input).await
    }
    async fn admit(&self, budget: &Budget) -> Result<OwnedSemaphorePermit, ReadError> {
        budget.check()?;
        if let Ok(permit) = self.inner.active.clone().try_acquire_owned() {
            return Ok(permit);
        }
        let _queued = self
            .inner
            .waiting
            .clone()
            .try_acquire_owned()
            .map_err(|_| ReadError::Capacity)?;
        tokio::select! {
            permit = self.inner.active.clone().acquire_owned() => { budget.check()?; permit.map_err(|_| ReadError::Unavailable) },
            _ = budget.cancelled() => Err(ReadError::Cancelled),
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(budget.deadline)) => Err(ReadError::Deadline),
        }
    }
}
/// Owns one logical request slot through body collection and worker execution.
/// Dropping before execution cancels collection and releases its slot; after the
/// handoff the worker and any deferred mutation plan share ownership until both
/// actually release it.
pub struct ReadAdmission {
    inner: Arc<Inner>,
    principal: Option<Principal>,
    budget: Option<Budget>,
    permit: Option<OwnedSemaphorePermit>,
    guard: Option<WorkGuard>,
}
/// Shared execution ownership, also retained by a provider-held mutation plan.
/// The original registration transfers here without an untracked handoff gap.
pub(crate) struct WorkLease {
    _permit: OwnedSemaphorePermit,
    _guard: Option<WorkGuard>,
}
impl Drop for ReadAdmission {
    fn drop(&mut self) {
        if let Some(budget) = &self.budget {
            budget.cancel.cancel();
        }
    }
}
impl ReadAdmission {
    pub fn deadline(&self) -> Instant {
        self.budget.as_ref().expect("live admission").deadline
    }
    pub fn cancellation(&self) -> CancellationToken {
        self.budget.as_ref().expect("live admission").cancel.clone()
    }
    /// Wait for either caller cancellation or the application cooperative stop.
    pub async fn cancelled(&self) {
        self.budget
            .as_ref()
            .expect("live admission")
            .cancelled()
            .await;
    }
    pub async fn read(self, request: ReadRequest) -> Result<ReadPage, ReadError> {
        self.run(Input::Request(request)).await
    }
    pub async fn read_wire(
        self,
        operation: ReadOperation,
        body: Vec<u8>,
        input: H4Codec,
        output: H4Codec,
    ) -> Result<ReadPage, ReadError> {
        self.run(Input::Wire {
            operation,
            body,
            input,
            output,
        })
        .await
    }
    async fn run(self, input: Input) -> Result<ReadPage, ReadError> {
        let inner = self.inner.clone();
        self.run_task(move |principal, budget| inner.execute(principal, input, budget))
            .await
    }
    pub(crate) fn belongs_to(&self, service: &ReadService) -> bool {
        Arc::ptr_eq(&self.inner, &service.inner)
    }
    pub(crate) async fn run_task<T: Send + 'static>(
        mut self,
        task: impl FnOnce(Principal, &mut Budget) -> Result<T, ReadError> + Send + 'static,
    ) -> Result<T, ReadError> {
        let mut budget = self.budget.take().expect("live admission");
        let principal = self.principal.take().expect("live admission");
        let permit = self.permit.take().expect("live admission");
        let cancel = budget.cancel.clone();
        let deadline = budget.deadline;
        let owner_cancel = budget.owner_cancel.clone();
        let mut drop_guard = CancelOnDrop {
            token: cancel.clone(),
            armed: true,
        };
        budget.check()?;
        let inner = self.inner.clone();
        budget.lease = Some(Arc::new(WorkLease {
            _permit: permit,
            _guard: self.guard.take(),
        }));
        let mut job = spawn_worker(inner, principal, task, budget);
        let result = tokio::select! {
            biased;
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                cancel.cancel();
                job.abort();
                Err(ReadError::Deadline)
            },
            _ = cancel.cancelled() => {
                job.abort();
                Err(ReadError::Cancelled)
            },
            _ = async {
                match owner_cancel { Some(token) => token.cancelled().await, None => std::future::pending::<()>().await }
            } => {
                cancel.cancel();
                job.abort();
                Err(ReadError::Cancelled)
            },
            result = &mut job => result.map_err(|_| ReadError::Unavailable)?,
        };
        // A stop is a prompt request outcome, not proof that the worker exited.
        // Awaiting an aborted spawn_blocking job can wait indefinitely behind
        // unrelated runtime work. Its capture retains admission until release.
        drop_guard.armed = false;
        result
    }
}

/// Managed workers carry a tracker registration through the runtime's actual
/// closure lifetime; unmanaged workers keep their caller-runtime contract.
/// Neither abort nor a returned request error is a join receipt. Tokio may keep
/// an aborted closure in its blocking queue, so admission and registration
/// belong to the closure capture rather than the awaiting caller.
fn spawn_worker<T: Send + 'static>(
    inner: Arc<Inner>,
    principal: Principal,
    task: impl FnOnce(Principal, &mut Budget) -> Result<T, ReadError> + Send + 'static,
    mut budget: Budget,
) -> tokio::task::JoinHandle<Result<T, ReadError>> {
    let lifecycle = inner.lifecycle.clone();
    let work = move || {
        budget.check()?;
        task(principal, &mut budget)
    };
    match lifecycle {
        Some(life) => life.runtime().spawn_blocking(work),
        None => tokio::task::spawn_blocking(work),
    }
}

impl Inner {
    fn execute(
        &self,
        principal: Principal,
        input: Input,
        budget: &mut Budget,
    ) -> Result<ReadPage, ReadError> {
        if let Principal::Authenticated { permissions, .. } = &principal
            && permissions.len() > budget.limits.max_ids
        {
            return Err(ReadError::Budget(BudgetKind::Input));
        }
        if principal.bytes() > budget.limits.max_input_bytes {
            return Err(ReadError::Budget(BudgetKind::Input));
        }
        budget.check()?;
        let policy = self.policy.snapshot(&principal)?;
        budget.check()?;
        if !policy.operation(input.operation()) {
            return Err(ReadError::Forbidden);
        }
        if policy.scope_key().len() > budget.limits.max_input_bytes {
            return Err(ReadError::Budget(BudgetKind::Input));
        }
        let request = match input {
            Input::Request(request) => request,
            Input::Wire {
                operation,
                body,
                input,
                output,
            } => wire::decode(operation, &body, input, output, budget)?,
        };
        let prepared = normalize(request, principal, policy.as_ref(), budget)?;
        let prior = prepared
            .cursor
            .as_deref()
            .map(|c| self.lookup(c, budget))
            .transpose()?;
        if prior
            .as_ref()
            .is_some_and(|(identity, _, _)| identity.as_ref() != prepared.identity.as_ref())
        {
            return Err(ReadError::StaleCursor);
        }
        loop {
            let wait = budget.wait_quantum()?;
            if let Some(result) = self.graph.read_for(wait, |graph| {
                let stamp = Stamp {
                    dataset: self.dataset,
                    incarnation: graph.incarnation(),
                    entities: graph.version(),
                    catalog: graph.catalog_generation(),
                };
                if prior.as_ref().is_some_and(|(_, old, _)| *old != stamp) {
                    return Err(ReadError::StaleCursor);
                }
                let after = prior.as_ref().map(|(_, _, id)| id.as_str());
                let mut view = View {
                    graph,
                    policy: policy.as_ref(),
                    budget,
                };
                validate_query(&prepared.identity.query, &mut view)?;
                let (rows, complete, last) = produce(&prepared.identity, after, &mut view)?;
                let count = rows.len();
                let pending = if complete {
                    None
                } else {
                    Some(self.token(view.budget)?)
                };
                let cursor = pending.as_ref().map(|(_, token)| token.as_str());
                let grid = output::grid(rows, complete, cursor, view.budget)?;
                let output = output::encode(grid, prepared.identity.profile, view.budget)?;
                if let Some((nonce, _)) = &pending {
                    let after = last.ok_or(ReadError::Unavailable)?;
                    self.insert(
                        *nonce,
                        prepared.identity.clone(),
                        stamp,
                        after,
                        prepared.retained_estimate,
                        view.budget,
                    )?;
                }
                Ok(ReadPage {
                    output,
                    complete,
                    cursor: pending.map(|(_, token)| token),
                    row_count: count,
                })
            }) {
                return result;
            }
        }
    }
    fn lock_cursors<'a>(
        &'a self,
        budget: &Budget,
    ) -> Result<MutexGuard<'a, CursorTable>, ReadError> {
        loop {
            if let Some(guard) = self.cursors.try_lock_for(budget.wait_quantum()?) {
                return Ok(guard);
            }
        }
    }
    fn token(&self, budget: &mut Budget) -> Result<([u8; 16], String), ReadError> {
        budget.charge(BudgetKind::Retained, 256)?;
        let nonce: [u8; 16] = rand::random();
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&self.cursor_key).map_err(|_| ReadError::Unavailable)?;
        mac.update(&nonce);
        let mut bytes = Vec::with_capacity(48);
        bytes.extend_from_slice(&nonce);
        bytes.extend_from_slice(&mac.finalize().into_bytes());
        Ok((nonce, URL_SAFE_NO_PAD.encode(bytes)))
    }
    fn lookup(
        &self,
        token: &str,
        budget: &mut Budget,
    ) -> Result<(Arc<RequestIdentity>, Stamp, String), ReadError> {
        if token.len() != 64 {
            return Err(ReadError::StaleCursor);
        }
        budget.charge(BudgetKind::Retained, 256)?;
        let bytes = URL_SAFE_NO_PAD
            .decode(token)
            .map_err(|_| ReadError::StaleCursor)?;
        if bytes.len() != 48 {
            return Err(ReadError::StaleCursor);
        }
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&self.cursor_key).map_err(|_| ReadError::Unavailable)?;
        mac.update(&bytes[..16]);
        mac.verify_slice(&bytes[16..])
            .map_err(|_| ReadError::StaleCursor)?;
        let nonce: [u8; 16] = bytes[..16].try_into().map_err(|_| ReadError::StaleCursor)?;
        let table = self.lock_cursors(budget)?;
        let record = table.entries.get(&nonce).ok_or(ReadError::StaleCursor)?;
        if Instant::now() >= record.expires {
            return Err(ReadError::StaleCursor);
        }
        Ok((
            record.identity.clone(),
            record.stamp.clone(),
            budget.copy_string(&record.after)?,
        ))
    }
    fn insert(
        &self,
        nonce: [u8; 16],
        identity: Arc<RequestIdentity>,
        stamp: Stamp,
        after: String,
        estimate: usize,
        budget: &mut Budget,
    ) -> Result<(), ReadError> {
        let bytes = estimate.saturating_add(after.len()).saturating_add(512);
        let mut table = self.lock_cursors(budget)?;
        budget.charge(BudgetKind::Work, table.entries.len())?;
        let now = Instant::now();
        table.entries.retain(|_, record| record.expires > now);
        table.bytes = table.entries.values().map(|r| r.bytes).sum();
        if table.entries.len() >= self.limits.cursor_capacity
            || bytes > self.limits.max_cursor_bytes.saturating_sub(table.bytes)
        {
            return Err(ReadError::Capacity);
        }
        budget.check()?;
        table.bytes += bytes;
        table.entries.insert(
            nonce,
            CursorRecord {
                identity,
                stamp,
                after,
                expires: now + self.limits.cursor_ttl,
                bytes,
            },
        );
        Ok(())
    }
}

fn request_bytes(request: &ReadRequest) -> usize {
    let query = match &request.query {
        ReadQuery::Ids(ids) => ids.iter().fold(0usize, |n, id| {
            n.saturating_add(id.len()).saturating_add(32)
        }),
        ReadQuery::Filter(s) | ReadQuery::Spec(s) => s.len(),
        ReadQuery::Nav(s)
        | ReadQuery::Definitions { filter: s }
        | ReadQuery::Specs { library: s } => s.as_ref().map_or(0, String::len),
        ReadQuery::Libraries => 0,
    };
    request.projection.iter().fold(
        query.saturating_add(request.cursor.as_ref().map_or(0, String::len)),
        |n, s| n.saturating_add(s.len()).saturating_add(32),
    )
}
fn normalize(
    mut request: ReadRequest,
    principal: Principal,
    policy: &dyn PolicySnapshot,
    budget: &mut Budget,
) -> Result<Prepared, ReadError> {
    if request.profile == OutputProfile::H4(H4Codec::Trio) {
        return Err(ReadError::InvalidQuery("codec cannot carry page metadata"));
    }
    if request.page_size == 0 || request.page_size > budget.limits.max_rows {
        return Err(ReadError::Budget(BudgetKind::Rows));
    }
    if request.projection.len() > budget.limits.max_ids {
        return Err(ReadError::Budget(BudgetKind::Input));
    }
    if let ReadQuery::Ids(ids) = &request.query
        && ids.len() > budget.limits.max_ids
    {
        return Err(ReadError::Budget(BudgetKind::Ids));
    }
    let bytes = request_bytes(&request)
        .saturating_add(principal.bytes())
        .saturating_add(policy.scope_key().len());
    if bytes > budget.limits.max_input_bytes {
        return Err(ReadError::Budget(BudgetKind::Input));
    }
    // Covers parser scalar/container expansion and retained normalized identity.
    let estimate = bytes.saturating_mul(512).saturating_add(1024);
    budget.charge(BudgetKind::Retained, estimate)?;
    budget.charge(BudgetKind::Work, bytes.saturating_add(1))?;
    request.projection.sort();
    request.projection.dedup();
    let query = match request.query {
        ReadQuery::Ids(mut ids) => {
            ids.sort();
            ids.dedup();
            NormalizedQuery::Ids(ids)
        }
        ReadQuery::Filter(filter) => {
            let filter = filter.trim();
            if filter == "*" {
                NormalizedQuery::Filter(None)
            } else {
                let limits = FilterParseLimits {
                    max_bytes: budget.limits.max_input_bytes,
                    max_nodes: budget.limits.max_ast_nodes,
                    max_depth: budget.limits.max_ast_depth,
                };
                let ast = filter::parse_filter_controlled(filter, limits, &mut || {
                    budget.check().map_err(|_| FilterError::Interrupted)
                })
                .map_err(|error| match error {
                    FilterError::Limit => ReadError::Budget(BudgetKind::Ast),
                    FilterError::Interrupted => {
                        budget.check().err().unwrap_or(ReadError::Cancelled)
                    }
                    FilterError::Parse { .. } => ReadError::InvalidQuery("invalid filter"),
                })?;
                NormalizedQuery::Filter(Some(ast))
            }
        }
        ReadQuery::Nav(s) => NormalizedQuery::Nav(s),
        ReadQuery::Definitions { filter } => NormalizedQuery::Definitions(filter),
        ReadQuery::Libraries => NormalizedQuery::Libraries,
        ReadQuery::Specs { library } => NormalizedQuery::Specs(library),
        ReadQuery::Spec(s) => NormalizedQuery::Spec(s),
    };
    let scope: Arc<str> = Arc::from(budget.copy_string(policy.scope_key())?);
    let identity = Arc::new(RequestIdentity {
        query,
        projection: request.projection,
        page_size: request.page_size,
        profile: request.profile,
        principal,
        scope,
    });
    Ok(Prepared {
        identity,
        cursor: request.cursor,
        retained_estimate: estimate,
    })
}
fn validate_query(query: &NormalizedQuery, view: &mut View<'_>) -> Result<(), ReadError> {
    fn ast(node: &FilterNode, view: &mut View<'_>, depth: usize) -> Result<(), ReadError> {
        view.budget.depth(depth)?;
        view.budget.charge(BudgetKind::Work, 1)?;
        match node {
            FilterNode::SpecMatch(term) => {
                if !filter::catalog_term_available(view.graph.namespace(), term, view)? {
                    return Err(ReadError::InvalidQuery("catalog term is unavailable"));
                }
            }
            FilterNode::And(l, r) | FilterNode::Or(l, r) => {
                ast(l, view, depth + 1)?;
                ast(r, view, depth + 1)?;
            }
            FilterNode::Cmp { val, .. } => sanitize::measure(val, view.budget, 1)?,
            _ => {}
        }
        Ok(())
    }
    if let NormalizedQuery::Filter(Some(node)) = query {
        ast(node, view, 1)?;
    }
    Ok(())
}
fn project(mut row: HDict, projection: &[String], budget: &mut Budget) -> Result<HDict, ReadError> {
    if !projection.is_empty() {
        let mut out = HDict::new();
        for tag in projection {
            budget.charge(BudgetKind::Work, 1)?;
            if let Some(value) = row.remove_tag(tag) {
                budget.charge(BudgetKind::Retained, 512)?;
                out.set(budget.copy_string(tag)?, value);
            }
        }
        row = out;
    }
    Ok(row)
}
fn nav_row(mut row: HDict, budget: &mut Budget) -> Result<HDict, ReadError> {
    let mut out = HDict::new();
    if let Some(Kind::Ref(id)) = row.remove_tag("id") {
        out.set("navId", Kind::Str(budget.copy_string(&id.val)?));
        out.set("id", Kind::Ref(id));
    }
    if let Some(dis) = row.remove_tag("dis") {
        out.set("dis", dis);
    }
    budget.charge(BudgetKind::Retained, 1536)?;
    Ok(out)
}
fn produce(
    identity: &RequestIdentity,
    after: Option<&str>,
    view: &mut View<'_>,
) -> Result<(Vec<HDict>, bool, Option<String>), ReadError> {
    match &identity.query {
        NormalizedQuery::Definitions(_)
        | NormalizedQuery::Libraries
        | NormalizedQuery::Specs(_)
        | NormalizedQuery::Spec(_) => return catalog_page(identity, after, view),
        _ => {}
    }
    let mut rows = Vec::new();
    let mut last = None;
    if let NormalizedQuery::Nav(Some(parent)) = &identity.query {
        if !view.policy.entity(parent) || !view.graph.contains(parent) {
            return Ok((rows, true, None));
        }
        // Charge raw inverse work once, including denied/wrong-relation edges.
        for _ in view.graph.incoming_edges(parent) {
            view.budget.charge(BudgetKind::Inverse, 1)?;
        }
    }
    let mut accept = |id: &str, view: &mut View<'_>| -> Result<bool, ReadError> {
        view.budget.charge(BudgetKind::Candidates, 1)?;
        let Some(row) = view.entity(id)? else {
            return Ok(false);
        };
        let matched = match &identity.query {
            NormalizedQuery::Filter(Some(node)) => {
                filter::matches_controlled(node, row.clone(), view.graph.namespace(), view)?
            }
            NormalizedQuery::Nav(None) => row.has("site"),
            NormalizedQuery::Nav(Some(parent)) => {
                let mut found = false;
                for (tag, value) in row.iter() {
                    view.budget.charge(BudgetKind::Work, 1)?;
                    if tag != "id" && matches!(value, Kind::Ref(r) if r.val == *parent) {
                        found = true;
                    }
                }
                found
            }
            _ => true,
        };
        if !matched {
            return Ok(false);
        }
        if rows.len() == identity.page_size {
            return Ok(true);
        }
        let row = Arc::try_unwrap(row).map_err(|_| ReadError::Unavailable)?;
        let row = if matches!(identity.query, NormalizedQuery::Nav(_)) {
            nav_row(row, view.budget)?
        } else {
            row
        };
        let row = project(row, &identity.projection, view.budget)?;
        view.budget.charge(BudgetKind::Retained, 128)?;
        rows.push(row);
        last = Some(view.budget.copy_string(id)?);
        Ok(false)
    };
    let mut more = false;
    match &identity.query {
        NormalizedQuery::Ids(ids) => {
            for id in ids
                .iter()
                .filter(|id| after.is_none_or(|after| id.as_str() > after))
            {
                if accept(id, view)? {
                    more = true;
                    break;
                }
            }
        }
        _ => {
            for (id, _) in view.graph.entities_after(after) {
                if accept(id, view)? {
                    more = true;
                    break;
                }
            }
        }
    }
    Ok((rows, !more, last))
}

fn catalog_page(
    identity: &RequestIdentity,
    after: Option<&str>,
    view: &mut View<'_>,
) -> Result<(Vec<HDict>, bool, Option<String>), ReadError> {
    let Some(ns) = view.graph.namespace() else {
        return if matches!(identity.query, NormalizedQuery::Spec(_)) {
            Err(ReadError::Unavailable)
        } else {
            Ok((Vec::new(), true, None))
        };
    };
    // Keep only page_size+1 borrowed names while scanning the unordered maps.
    // Work includes denied entries; no namespace helper materializes all names.
    let mut selected: BTreeMap<&str, CatalogRow<'_>> = BTreeMap::new();
    match &identity.query {
        NormalizedQuery::Definitions(filter) => {
            for (name, def) in ns.defs() {
                catalog_visit(name, view.budget)?;
                let allowed = view.policy.catalog(CatalogKind::Definition, name)
                    && view.policy.catalog(CatalogKind::Library, &def.lib)
                    && filter.as_ref().is_none_or(|f| name.contains(f));
                select_catalog(
                    &mut selected,
                    (name, CatalogRow::Definition(def)),
                    allowed,
                    after,
                    identity.page_size,
                    view.budget,
                )?;
            }
        }
        NormalizedQuery::Libraries => {
            for (name, lib) in ns.libs() {
                catalog_visit(name, view.budget)?;
                select_catalog(
                    &mut selected,
                    (name, CatalogRow::Library(lib)),
                    view.policy.catalog(CatalogKind::Library, name),
                    after,
                    identity.page_size,
                    view.budget,
                )?;
            }
        }
        NormalizedQuery::Specs(lib) => {
            for (name, spec) in ns.specs_map() {
                catalog_visit(name, view.budget)?;
                let allowed = view.policy.catalog(CatalogKind::Spec, name)
                    && view.policy.catalog(CatalogKind::Library, &spec.lib)
                    && lib.as_ref().is_none_or(|lib| *lib == spec.lib);
                select_catalog(
                    &mut selected,
                    (name, CatalogRow::Spec(spec, false)),
                    allowed,
                    after,
                    identity.page_size,
                    view.budget,
                )?;
            }
        }
        NormalizedQuery::Spec(name) => {
            let spec = ns
                .get_spec(name)
                .filter(|s| {
                    view.policy.catalog(CatalogKind::Spec, name)
                        && view.policy.catalog(CatalogKind::Library, &s.lib)
                })
                .ok_or(ReadError::Unavailable)?;
            catalog_visit(&spec.qname, view.budget)?;
            select_catalog(
                &mut selected,
                (&spec.qname, CatalogRow::Spec(spec, true)),
                true,
                after,
                identity.page_size,
                view.budget,
            )?;
        }
        _ => unreachable!(),
    }
    let complete = selected.len() <= identity.page_size;
    if !complete {
        selected.pop_last();
    }
    let mut rows = Vec::new();
    let mut last = None;
    for (name, item) in selected {
        let row = catalog_row(item, ns, view)?;
        let row = project(row, &identity.projection, view.budget)?;
        view.budget.charge(BudgetKind::Retained, 128)?;
        rows.push(row);
        last = Some(view.budget.copy_string(name)?);
    }
    Ok((rows, complete, last))
}
fn catalog_visit(name: &str, budget: &mut Budget) -> Result<(), ReadError> {
    budget.charge(BudgetKind::Candidates, 1)?;
    budget.charge(BudgetKind::Work, name.len().saturating_add(1))
}
fn select_catalog<'a>(
    selected: &mut BTreeMap<&'a str, CatalogRow<'a>>,
    (name, item): (&'a str, CatalogRow<'a>),
    allowed: bool,
    after: Option<&str>,
    limit: usize,
    budget: &mut Budget,
) -> Result<(), ReadError> {
    if !allowed || after.is_some_and(|after| name <= after) {
        return Ok(());
    }
    if selected.len() > limit
        && selected
            .last_key_value()
            .is_some_and(|(last, _)| name >= *last)
    {
        return Ok(());
    }
    budget.charge(BudgetKind::Retained, 128)?;
    selected.insert(name, item);
    if selected.len() > limit + 1 {
        selected.pop_last();
    }
    Ok(())
}
enum CatalogRow<'a> {
    Definition(&'a haystack_core::ontology::Def),
    Library(&'a haystack_core::ontology::Lib),
    Spec(&'a Spec, bool),
}
fn put(row: &mut HDict, tag: &str, value: Kind, budget: &mut Budget) -> Result<(), ReadError> {
    budget.charge(BudgetKind::Retained, 512)?;
    row.set(budget.copy_string(tag)?, value);
    Ok(())
}
fn text(row: &mut HDict, tag: &str, value: &str, budget: &mut Budget) -> Result<(), ReadError> {
    let value = budget.copy_string(value)?;
    put(row, tag, Kind::Str(value), budget)
}
fn catalog_row(
    item: CatalogRow<'_>,
    ns: &DefNamespace,
    view: &mut View<'_>,
) -> Result<HDict, ReadError> {
    let mut row = HDict::new();
    match item {
        CatalogRow::Definition(def) => {
            let symbol = Symbol::new(view.budget.copy_string(&def.symbol)?);
            put(&mut row, "def", Kind::Symbol(symbol), view.budget)?;
            let lib = Symbol::new(view.budget.copy_string(&def.lib)?);
            put(&mut row, "lib", Kind::Symbol(lib), view.budget)?;
            text(&mut row, "doc", &def.doc, view.budget)?;
        }
        CatalogRow::Library(lib) => {
            text(&mut row, "name", &lib.name, view.budget)?;
            text(&mut row, "version", &lib.version, view.budget)?;
        }
        CatalogRow::Spec(spec, detail) => {
            for (tag, value) in [
                ("qname", &spec.qname),
                ("name", &spec.name),
                ("lib", &spec.lib),
                ("doc", &spec.doc),
            ] {
                text(&mut row, tag, value, view.budget)?;
            }
            if let Some(base) = &spec.base
                && filter::catalog_term_available(Some(ns), base, view)?
            {
                text(&mut row, "base", base, view.budget)?;
            }
            if spec.is_abstract {
                put(&mut row, "abstract", Kind::Marker, view.budget)?;
            }
            if detail {
                let mut slots = Vec::new();
                for slot in &spec.slots {
                    view.budget.charge(BudgetKind::Work, 1)?;
                    view.budget.charge(BudgetKind::Retained, 128)?;
                    let mut entry = HDict::new();
                    text(&mut entry, "name", &slot.name, view.budget)?;
                    if slot.is_marker {
                        put(&mut entry, "marker", Kind::Marker, view.budget)?;
                    }
                    if slot.is_maybe() {
                        put(&mut entry, "maybe", Kind::Marker, view.budget)?;
                    }
                    if slot.is_query {
                        put(&mut entry, "query", Kind::Marker, view.budget)?;
                    }
                    if let Some(ty) = &slot.type_ref {
                        let primitive = matches!(
                            ty.as_str(),
                            "Str"
                                | "Number"
                                | "Ref"
                                | "Bool"
                                | "Date"
                                | "Time"
                                | "DateTime"
                                | "Uri"
                                | "Coord"
                                | "List"
                                | "Dict"
                                | "Grid"
                                | "Marker"
                                | "Query"
                        );
                        if primitive || filter::catalog_term_available(Some(ns), ty, view)? {
                            text(&mut entry, "type", ty, view.budget)?;
                        }
                    }
                    slots.push(Kind::Dict(Box::new(entry)));
                }
                put(&mut row, "slots", Kind::List(slots), view.budget)?;
            }
        }
    }
    Ok(row)
}

fn application_read_error(error: ApplicationError) -> ReadError {
    match error {
        ApplicationError::NotReady => ReadError::NotReady,
        _ => ReadError::Closed,
    }
}

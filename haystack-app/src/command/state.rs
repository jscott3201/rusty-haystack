use super::*;
use crate::{ApiError, BudgetKind, ReadOperation, budget::Budget};
use haystack_core::{
    data::{HCol, HDict, HGrid},
    graph::{DiffOp, GraphState},
    kinds::{Kind, Number},
};
use std::collections::BTreeMap;

#[derive(Clone, PartialEq)]
struct Schema {
    kind: String,
    unit: Option<String>,
    default: Option<Kind>,
}
impl Schema {
    fn read(row: &HDict, limit: usize) -> Result<Self, ApiError> {
        if row.get("point") != Some(&Kind::Marker) || row.get("writable") != Some(&Kind::Marker) {
            return Err(ApiError::NotImplemented);
        }
        let Some(Kind::Str(kind)) = row.get("kind") else {
            return Err(ApiError::InvalidArgs);
        };
        if !matches!(kind.as_str(), "Bool" | "Number" | "Str") {
            return Err(ApiError::NotImplemented);
        }
        let unit = match row.get("unit") {
            None => None,
            Some(Kind::Str(s))
                if kind == "Number"
                    && s.len() <= 128
                    && haystack_core::kinds::unit_for(s).is_some() =>
            {
                Some(s.clone())
            }
            _ => return Err(ApiError::InvalidArgs),
        };
        let mut schema = Self {
            kind: kind.clone(),
            unit,
            default: None,
        };
        if let Some(value) = row.get("writeDef").filter(|v| !matches!(v, Kind::Null)) {
            schema.default = Some(schema.normalize(value, limit)?);
        }
        Ok(schema)
    }
    fn normalize(&self, value: &Kind, limit: usize) -> Result<Kind, ApiError> {
        match (self.kind.as_str(), value) {
            ("Bool", Kind::Bool(_)) => Ok(value.clone()),
            ("Str", Kind::Str(s)) if s.len() <= limit => Ok(value.clone()),
            ("Number", Kind::Number(n))
                if n.val.is_finite()
                    && n.unit
                        .as_ref()
                        .is_none_or(|unit| unit.len() <= 128 && unit.len() <= limit)
                    && (n.unit.is_none()
                        || n.unit
                            .as_deref()
                            .and_then(haystack_core::kinds::unit_for)
                            .is_some_and(|unit| {
                                self.unit
                                    .as_deref()
                                    .and_then(haystack_core::kinds::unit_for)
                                    == Some(unit)
                            })) =>
            {
                Ok(Kind::Number(Number::new(n.val, self.unit.clone())))
            }
            _ => Err(ApiError::InvalidArgs),
        }
    }
}
#[derive(Clone, Default)]
pub(super) struct Slot {
    pub(super) value: Option<Kind>,
    who: Option<String>,
    pub(super) generation: u64,
    deadline: Option<Duration>,
}
impl Slot {
    fn bytes(&self) -> usize {
        128 + self.who.as_ref().map_or(0, String::len) + self.value.as_ref().map_or(0, value_bytes)
    }
}
fn value_bytes(v: &Kind) -> usize {
    match v {
        Kind::Str(s) => s.len() + 64,
        Kind::Number(n) => n.unit.as_ref().map_or(0, String::len) + 64,
        _ => 64,
    }
}
pub(super) struct Point {
    schema: Schema,
    graph: GraphState,
    pub(super) slots: [Slot; 17],
    pub(super) effective: Option<Kind>,
    pub(super) effective_level: u8,
}
impl Point {
    fn new(schema: Schema, graph: GraphState) -> Self {
        let mut slots = std::array::from_fn(|_| Slot::default());
        slots[16].value = schema.default.clone();
        let mut point = Self {
            schema,
            graph,
            slots,
            effective: None,
            effective_level: 17,
        };
        point.resolve();
        point
    }
    fn resolve(&mut self) {
        let first = self
            .slots
            .iter()
            .enumerate()
            .find_map(|(index, slot)| slot.value.as_ref().map(|value| (index, value)));
        (self.effective, self.effective_level) = first.map_or((None, 17), |(index, value)| {
            (Some(value.clone()), (index + 1) as u8)
        });
    }
    fn bytes(&self, id: &str) -> usize {
        id.len()
            + 512
            + self.schema.unit.as_ref().map_or(0, String::len)
            + self.schema.default.as_ref().map_or(0, value_bytes)
            + self.slots.iter().map(Slot::bytes).sum::<usize>()
            + self
                .slots
                .iter()
                .filter_map(|slot| slot.value.as_ref())
                .map(value_bytes)
                .max()
                .unwrap_or(0)
    }
}
#[derive(Default)]
pub(super) struct Store {
    pub(super) points: BTreeMap<String, Point>,
    pub(super) bytes: usize,
}
impl Store {
    pub(super) fn expire(&mut self, now: Duration) {
        // Selection and clearing occur under one lock; a replaced slot can
        // never be cleared by a previously queued expiry observation.
        for (id, point) in &mut self.points {
            let before = point.bytes(id);
            let mut expired = false;
            for slot in &mut point.slots {
                if slot.deadline.is_some_and(|deadline| deadline <= now) {
                    slot.value = None;
                    slot.deadline = None;
                    expired = true;
                }
            }
            if expired {
                point.resolve();
                self.bytes = self.bytes - before + point.bytes(id);
            }
        }
    }
}
struct Pending<'a>(&'a std::sync::atomic::AtomicUsize);
impl Drop for Pending<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
impl SimulatedCommandService {
    pub(crate) fn available(&self, principal: &Principal) -> bool {
        let Ok(policy) = self.inner.policy.snapshot(principal) else {
            return false;
        };
        [
            CommandAction::Inspect,
            CommandAction::Set,
            CommandAction::Relinquish,
        ]
        .into_iter()
        .any(|action| {
            self.transport(principal, action)
                && policy.action(action)
                && (action == CommandAction::Inspect
                    || self.inner.mode == SimulatedCommandMode::Writable)
        })
    }
    fn transport(&self, principal: &Principal, action: CommandAction) -> bool {
        match principal {
            Principal::Anonymous => false,
            Principal::TrustedEmbedding { .. } => true,
            Principal::Authenticated { permissions, .. } => permissions.iter().any(|p| {
                p == "admin"
                    || p == if action == CommandAction::Inspect {
                        "read"
                    } else {
                        "write"
                    }
            }),
        }
    }
    fn check(&self, session: &crate::SubscriptionSession, budget: &Budget) -> Result<(), ApiError> {
        budget.check()?;
        if !session.scoped() || !session.is_active() {
            return Err(ApiError::Permission);
        }
        if !self.inner.started.load(Ordering::Acquire)
            || self.inner.stopped.load(Ordering::Acquire)
            || budget
                .owner_sealed
                .as_ref()
                .is_some_and(|t| t.is_cancelled())
        {
            return Err(ApiError::Unavailable);
        }
        Ok(())
    }
    pub(crate) fn execute<T>(
        &self,
        principal: &Principal,
        session: &crate::SubscriptionSession,
        request: CommandRequest,
        expected_policy: Option<&str>,
        budget: &mut Budget,
        encode: impl FnOnce(HGrid, &mut Budget) -> Result<T, ApiError>,
    ) -> Result<T, ApiError> {
        self.inner
            .pending
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < self.inner.limits.max_pending).then_some(n + 1)
            })
            .map_err(|_| ApiError::Unavailable)?;
        let _pending = Pending(&self.inner.pending);
        self.check(session, budget)?;
        if !session.matches(principal) {
            return Err(ApiError::Permission);
        }
        let action = request.action();
        if !self.transport(principal, action) {
            return Err(ApiError::Permission);
        }
        if action != CommandAction::Inspect && self.inner.mode != SimulatedCommandMode::Writable {
            return Err(ApiError::NotImplemented);
        }
        if request.id.is_empty()
            || request.id.len() > self.inner.limits.max_value_bytes
            || request.level.is_some_and(|n| !(1..=17).contains(&n))
            || request.duration.is_some_and(|d| {
                d.is_zero()
                    || d > self.inner.limits.max_duration
                    || request.level != Some(8)
                    || action != CommandAction::Set
            })
        {
            return Err(ApiError::InvalidArgs);
        }
        let command_policy = self.inner.policy.snapshot(principal)?;
        let policy = self.inner.reads.policy_snapshot(principal)?;
        if command_policy.scope_key().len() > 1024 || policy.scope_key().len() > 1024 {
            return Err(ApiError::Unavailable);
        }
        if !command_policy.action(action) {
            return Err(ApiError::Permission);
        }
        if expected_policy.is_some_and(|scope| scope != policy.scope_key()) {
            return Err(ApiError::Permission);
        }
        if action == CommandAction::Inspect && !policy.operation(ReadOperation::Read) {
            return Err(ApiError::Permission);
        }
        if !command_policy.point(&request.id, action)
            || !policy.entity(&request.id)
            || !policy.tag(&request.id, "id")
        {
            return Err(ApiError::UnknownEntity);
        }
        for tag in ["point", "writable", "kind", "unit", "writeDef"] {
            if !policy.tag(&request.id, tag) {
                return Err(ApiError::UnknownEntity);
            }
        }
        let who = match principal {
            Principal::Authenticated { subject, .. } | Principal::TrustedEmbedding { subject } => {
                request.who.as_deref().unwrap_or(subject)
            }
            Principal::Anonymous => return Err(ApiError::Permission),
        };
        if who.len() > self.inner.limits.max_who_bytes {
            return Err(ApiError::InvalidArgs);
        }
        budget.charge(
            BudgetKind::Retained,
            4096 + self.inner.limits.max_value_bytes.saturating_mul(40)
                + self.inner.limits.max_who_bytes.saturating_mul(34)
                + request.id.len(),
        )?;
        let graph = self
            .inner
            .reads
            .native_graph()
            .ok_or(ApiError::Unavailable)?;
        let (schema, observed) = loop {
            if let Some(result) = graph.read_for(budget.wait_quantum()?, |graph| {
                budget.check_catalog(graph)?;
                let row = graph.get(&request.id).ok_or(ApiError::UnknownEntity)?;
                Ok::<_, ApiError>((
                    Schema::read(row, self.inner.limits.max_value_bytes)?,
                    graph.state(),
                ))
            }) {
                break result?;
            }
        };
        let value = if action == CommandAction::Set {
            Some(schema.normalize(
                request.value.as_ref().ok_or(ApiError::InvalidArgs)?,
                self.inner.limits.max_value_bytes,
            )?)
        } else {
            None
        };
        #[cfg(test)]
        {
            let hook = self.inner.before_dispatch.lock().clone();
            if let Some(hook) = hook {
                hook();
            }
        }
        let disclosure_service = self.clone();
        let disclosure_principal = principal.clone();
        let disclosure_session = session.clone();
        let read_scope = policy.scope_key().to_owned();
        let command_scope = command_policy.scope_key().to_owned();
        budget.command_disclosure = Some(Arc::new(move || {
            if !disclosure_session.is_active()
                || disclosure_service.inner.stopped.load(Ordering::Acquire)
            {
                return Err(ApiError::Permission);
            }
            if disclosure_service
                .inner
                .reads
                .policy_snapshot(&disclosure_principal)?
                .scope_key()
                != read_scope
                || disclosure_service
                    .inner
                    .policy
                    .snapshot(&disclosure_principal)?
                    .scope_key()
                    != command_scope
            {
                return Err(ApiError::Permission);
            }
            let current = disclosure_service
                .inner
                .reads
                .graph()
                .read_for(Duration::ZERO, |graph| graph.state());
            if current != Some(observed) {
                return Err(ApiError::Unavailable);
            }
            Ok(())
        }));
        let (slots, final_command, final_policy) = loop {
            self.check(session, budget)?;
            // Immutable authority must still match the prepared intent and
            // typed binding. Never carry these snapshots across a lock wait
            // or a failed publication attempt. Both final guards are try-only;
            // callbacks, waits and encoding stay outside publication guards.
            let final_command = self.inner.policy.snapshot(principal)?;
            let final_policy = self.inner.reads.policy_snapshot(principal)?;
            if final_command.scope_key() != command_policy.scope_key()
                || final_policy.scope_key() != policy.scope_key()
            {
                return Err(ApiError::Permission);
            }
            let Some(mut store) = self.inner.state.try_lock() else {
                // Wait only for progress, then release and refresh authority.
                // Acquiring the store here never grants publication authority.
                drop((final_command, final_policy));
                #[cfg(test)]
                {
                    // Pin an actual failed acquisition, not just the earlier
                    // snapshot callback, without holding publication guards.
                    let hook = self.inner.store_retry.lock().clone();
                    if let Some(hook) = hook {
                        hook();
                    }
                }
                drop(self.inner.state.try_lock_for(budget.wait_quantum()?));
                budget.charge(BudgetKind::Work, 1)?;
                continue;
            };
            if let Some(result) = graph.read_for(Duration::ZERO, |graph| {
                budget.check_catalog(graph)?;
                if graph.state() != observed {
                    return Err(ApiError::Unavailable);
                }
                self.check(session, budget)?;
                store.expire(self.inner.clock.now());
                if let Some(point) = store.points.get(&request.id) {
                    if point.schema != schema
                        || point.graph.incarnation != observed.incarnation
                        || point.graph.catalog_generation != observed.catalog_generation
                    {
                        return Err(ApiError::Unavailable);
                    }
                    for unit in graph
                        .change_units_since(point.graph.revision)
                        .map_err(|_| ApiError::Unavailable)?
                    {
                        budget.charge(BudgetKind::Work, unit.len() + 1)?;
                        for diff in unit.diffs().filter(|diff| diff.ref_val == request.id) {
                            if diff.op == DiffOp::Remove
                                || diff.changed_tags.as_ref().is_some_and(|tags| {
                                    ["point", "writable", "kind", "unit", "writeDef"]
                                        .iter()
                                        .any(|tag| tags.has(tag))
                                })
                                || diff.new.as_ref().is_some_and(|row| {
                                    Schema::read(row, self.inner.limits.max_value_bytes).as_ref()
                                        != Ok(&point.schema)
                                })
                            {
                                return Err(ApiError::Unavailable);
                            }
                        }
                    }
                } else if store.points.len() >= self.inner.limits.max_points {
                    return Err(ApiError::Unavailable);
                }
                let mut point = store
                    .points
                    .get(&request.id)
                    .map(|p| Point {
                        schema: p.schema.clone(),
                        graph: observed,
                        slots: p.slots.clone(),
                        effective: p.effective.clone(),
                        effective_level: p.effective_level,
                    })
                    .unwrap_or_else(|| Point::new(schema.clone(), observed));
                if let Some(level) = request.level {
                    let slot = &mut point.slots[usize::from(level - 1)];
                    slot.generation = slot
                        .generation
                        .checked_add(1)
                        .ok_or(ApiError::Unavailable)?;
                    slot.value = value.clone();
                    // Attribution records the last explicit controller even
                    // when that controller relinquishes to a null value.
                    slot.who = Some(who.to_owned());
                    slot.deadline = request
                        .duration
                        .map(|duration| {
                            self.inner
                                .clock
                                .now()
                                .checked_add(duration)
                                .ok_or(ApiError::InvalidArgs)
                        })
                        .transpose()?;
                }
                point.resolve();
                let before = store
                    .points
                    .get(&request.id)
                    .map_or(0, |p| p.bytes(&request.id));
                let after = point.bytes(&request.id);
                let total = store
                    .bytes
                    .saturating_sub(before)
                    .checked_add(after)
                    .ok_or(ApiError::Unavailable)?;
                if total > self.inner.limits.max_retained_bytes {
                    return Err(ApiError::Unavailable);
                }
                self.check(session, budget)?;
                if action != CommandAction::Inspect {
                    budget
                        .command_attempt
                        .as_ref()
                        .ok_or(ApiError::Internal)?
                        .dispatch()?;
                }
                let slots = point.slots.clone();
                store.points.insert(request.id.clone(), point);
                store.bytes = total;
                Ok(slots)
            }) {
                break (result?, final_command, final_policy);
            }
            drop(store);
            drop((final_command, final_policy));
            #[cfg(test)]
            if let Some(hook) = self.inner.publication_retry.lock().clone() {
                hook();
            }
            budget.charge(BudgetKind::Work, 1)?;
            std::thread::yield_now();
        };
        #[cfg(test)]
        {
            let hook = self.inner.after_dispatch.lock().clone();
            if action != CommandAction::Inspect
                && let Some(hook) = hook
            {
                hook();
            }
        }
        let mut grid = HGrid::new();
        if action == CommandAction::Inspect {
            grid.cols = ["level", "levelDis", "val", "who"]
                .into_iter()
                .map(HCol::new)
                .collect();
            for (index, slot) in slots.into_iter().enumerate() {
                let mut row = HDict::new();
                row.set("level", Kind::Number(Number::unitless((index + 1) as f64)));
                row.set(
                    "levelDis",
                    Kind::Str(match index + 1 {
                        1 => "Emergency Override".into(),
                        8 => "Manual Override".into(),
                        17 => "Default".into(),
                        n => format!("Level {n}"),
                    }),
                );
                if let Some(value) = slot.value {
                    row.set("val", value);
                }
                if let Some(who) = slot.who {
                    row.set("who", Kind::Str(who));
                }
                grid.rows.push(
                    crate::sanitize::record(&request.id, &row, final_policy.as_ref(), budget)?
                        .ok_or(ApiError::UnknownEntity)?,
                );
            }
        }
        let result = encode(grid, budget)?;
        self.check(session, budget)?;
        if let Some(check) = &budget.command_disclosure {
            check()?;
        }
        let current = self.inner.reads.policy_snapshot(principal)?;
        let command = self.inner.policy.snapshot(principal)?;
        if current.scope_key() != final_policy.scope_key()
            || command.scope_key() != final_command.scope_key()
        {
            return Err(ApiError::Permission);
        }
        Ok(result)
    }
}

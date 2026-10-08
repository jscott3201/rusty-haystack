//! Opaque atomic entity plans. Preparation is fallible; publication moves a
//! completely validated plan without CRUD callbacks, awaits, or rollback.
use std::collections::HashSet;

use super::super::{
    adjacency::PreparedAdjacency,
    bitmap::PreparedTags,
    changelog::{CommitSpan, DiffOp, GraphDiff, GraphState},
    size::{ValueBudget, diff_bytes, patch_diff_bytes},
    value_index::PreparedValues,
};
use super::{EntityGraph, GraphError, MAX_ENTITY_ID, query_cache_capacity_for};
use crate::{data::HDict, kinds::Kind};

#[derive(Debug, Clone)]
pub enum EntityOperation {
    Add(HDict),
    Patch { id: String, changes: HDict },
    Remove { id: String },
}
impl EntityOperation {
    pub fn target(&self) -> Result<&str, GraphError> {
        match self {
            Self::Add(row) => match row.get("id") {
                Some(Kind::Ref(id)) => Ok(&id.val),
                Some(_) => Err(GraphError::InvalidId),
                None => Err(GraphError::MissingId),
            },
            Self::Patch { id, .. } | Self::Remove { id } => Ok(id),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct BatchLimits {
    pub max_operations: usize,
    pub max_work: usize,
    pub max_retained_bytes: usize,
    pub max_value_depth: usize,
}
impl Default for BatchLimits {
    fn default() -> Self {
        Self {
            max_operations: 128,
            max_work: 1_000_000,
            max_retained_bytes: 16 * 1024 * 1024,
            max_value_depth: 32,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BatchError {
    #[error("entity batch is empty")]
    Empty,
    #[error("entity batch repeats a target")]
    DuplicateTarget,
    #[error("entity batch limit exceeded")]
    Limit,
    #[error("entity revision or graph/catalog/index state changed")]
    Conflict,
    #[error(transparent)]
    Entity(#[from] GraphError),
}

/// Before/after rows for policy validation, in request order. Empty patches
/// remain observable to policy but have `changed == false` and create no diff.
#[derive(Debug)]
pub struct PreparedChange {
    pub id: String,
    pub op: DiffOp,
    pub before: Option<HDict>,
    pub after: Option<HDict>,
    pub changed: bool,
    numeric_id: usize,
    map_keys: Option<(String, String)>,
}
pub(crate) struct IndexChange<'a> {
    pub id: usize,
    pub before: Option<&'a HDict>,
    pub after: Option<&'a HDict>,
}

/// A plan cannot be edited or forged. It is tied to one graph incarnation,
/// entity revision, catalog generation and index registration generation.
pub struct PreparedBatch {
    state: GraphState,
    index_generation: u64,
    after_revision: u64,
    retained_bytes: usize,
    changes: Vec<PreparedChange>,
    diffs: Vec<GraphDiff>,
    tags: PreparedTags,
    adjacency: PreparedAdjacency,
    values: PreparedValues,
    free_ids_consumed: usize,
    freed_ids: Vec<usize>,
    next_id: usize,
}
impl PreparedBatch {
    pub fn state(&self) -> GraphState {
        self.state
    }
    pub fn after_revision(&self) -> u64 {
        self.after_revision
    }
    pub fn span(&self) -> Option<CommitSpan> {
        self.diffs.first().map(|diff| diff.span)
    }
    /// Conservative allocations charged during preparation.
    pub fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }
    pub fn changes(&self) -> &[PreparedChange] {
        &self.changes
    }
    pub fn diffs(&self) -> &[GraphDiff] {
        &self.diffs
    }
}

impl EntityGraph {
    pub fn prepare_batch(
        &self,
        expected_revision: u64,
        operations: &[EntityOperation],
        limits: BatchLimits,
    ) -> Result<PreparedBatch, BatchError> {
        if expected_revision != self.version {
            return Err(BatchError::Conflict);
        }
        if operations.is_empty() {
            return Err(BatchError::Empty);
        }
        if operations.len() > limits.max_operations
            || limits.max_value_depth == 0
            || limits.max_value_depth > 64
        {
            return Err(BatchError::Limit);
        }
        let mut budget = ValueBudget::new(
            limits.max_work,
            limits.max_retained_bytes,
            limits.max_value_depth,
        );
        let mut seen = HashSet::new();
        let mut changes = Vec::with_capacity(operations.len());
        let mut diffs = Vec::with_capacity(operations.len());
        let mut free_ids_consumed = 0;
        let mut freed_ids = Vec::new();
        let mut next_id = self.next_id;
        let mut revision = self.version;
        for operation in operations {
            let id = operation.target()?;
            budget
                .charge(
                    id.len().saturating_add(1),
                    id.len().saturating_mul(4).saturating_add(1024),
                )
                .map_err(|_| BatchError::Limit)?;
            if !seen.insert(id) {
                return Err(BatchError::DuplicateTarget);
            }
            let before = self.entities.get(id);
            if let Some(row) = before {
                budget.dict(row, 1).map_err(|_| BatchError::Limit)?;
            }
            let (op, after, numeric_id, changed, delta) = match operation {
                EntityOperation::Add(row) => {
                    if before.is_some() {
                        return Err(GraphError::DuplicateRef(id.into()).into());
                    }
                    budget.dict(row, 1).map_err(|_| BatchError::Limit)?;
                    let numeric = if let Some(id) = freed_ids.pop() {
                        id
                    } else if free_ids_consumed < self.free_ids.len() {
                        free_ids_consumed += 1;
                        self.free_ids[self.free_ids.len() - free_ids_consumed]
                    } else {
                        if next_id > MAX_ENTITY_ID {
                            return Err(GraphError::IdExhausted.into());
                        }
                        let id = next_id;
                        next_id = next_id.checked_add(1).ok_or(GraphError::IdExhausted)?;
                        id
                    };
                    (DiffOp::Add, Some(row.clone()), numeric, true, None)
                }
                EntityOperation::Patch { changes, .. } => {
                    let old = before.ok_or_else(|| GraphError::NotFound(id.into()))?;
                    if let Some(value) = changes.get("id")
                        && !matches!(value, Kind::Ref(value) if value.val == id)
                    {
                        return Err(GraphError::ImmutableId.into());
                    }
                    budget.dict(changes, 1).map_err(|_| BatchError::Limit)?;
                    budget.dict(old, 1).map_err(|_| BatchError::Limit)?;
                    let mut after = old.clone();
                    after.merge(changes);
                    budget.dict(&after, 1).map_err(|_| BatchError::Limit)?;
                    let mut previous = HDict::new();
                    for (tag, _) in changes.iter() {
                        if let Some(value) = old.get(tag) {
                            budget
                                .charge(tag.len().saturating_add(1), tag.len().saturating_add(64))
                                .map_err(|_| BatchError::Limit)?;
                            budget.value(value, 2).map_err(|_| BatchError::Limit)?;
                            previous.set(tag, value.clone());
                        }
                    }
                    budget.dict(changes, 1).map_err(|_| BatchError::Limit)?;
                    (
                        DiffOp::Update,
                        Some(after),
                        self.id_map[id],
                        !changes.is_empty(),
                        Some((changes.clone(), previous)),
                    )
                }
                EntityOperation::Remove { .. } => {
                    if before.is_none() {
                        return Err(GraphError::NotFound(id.into()).into());
                    }
                    let numeric = self.id_map[id];
                    freed_ids.push(numeric);
                    (DiffOp::Remove, None, numeric, true, None)
                }
            };
            if changed {
                revision = revision
                    .checked_add(1)
                    .ok_or(GraphError::RevisionExhausted)?;
                let retained_bytes = match &op {
                    DiffOp::Add => diff_bytes(id, after.iter(), self.changelog_byte_capacity),
                    DiffOp::Remove => diff_bytes(id, before, self.changelog_byte_capacity),
                    DiffOp::Update => patch_diff_bytes(
                        id,
                        before.expect("validated old row"),
                        match operation {
                            EntityOperation::Patch { changes, .. } => changes,
                            _ => unreachable!(),
                        },
                        self.changelog_byte_capacity,
                    ),
                }
                .ok_or(BatchError::Limit)?;
                budget
                    .charge(1, retained_bytes)
                    .map_err(|_| BatchError::Limit)?;
                let (changed_tags, previous_tags) =
                    delta.map_or((None, None), |(new, old)| (Some(new), Some(old)));
                diffs.push(GraphDiff {
                    version: revision,
                    span: CommitSpan::singleton(revision),
                    retained_bytes,
                    timestamp: 0,
                    op: op.clone(),
                    ref_val: id.into(),
                    old: if op == DiffOp::Remove {
                        before.cloned()
                    } else {
                        None
                    },
                    new: if op == DiffOp::Add {
                        after.clone()
                    } else {
                        None
                    },
                    changed_tags,
                    previous_tags,
                });
            }
            let map_keys = (op == DiffOp::Add).then(|| (id.to_string(), id.to_string()));
            changes.push(PreparedChange {
                id: id.into(),
                map_keys,
                op,
                before: before.cloned(),
                after,
                numeric_id,
                changed,
            });
        }
        let bytes = diffs
            .iter()
            .try_fold(0usize, |bytes, diff| bytes.checked_add(diff.retained_bytes))
            .ok_or(BatchError::Limit)?;
        if diffs.len() > self.changelog_capacity || bytes > self.changelog_byte_capacity {
            return Err(BatchError::Limit);
        }
        if let Some(first) = diffs.first() {
            let span = CommitSpan {
                first: first.version,
                last: revision,
            };
            for diff in &mut diffs {
                diff.span = span;
            }
        }
        let additions = changes
            .iter()
            .filter(|change| change.op == DiffOp::Add)
            .count();
        for (len, capacity) in [
            (self.id_map.len(), self.id_map.capacity()),
            (self.reverse_id.len(), self.reverse_id.capacity()),
        ] {
            if additions > capacity.saturating_sub(len) {
                budget
                    .charge(
                        len.saturating_add(additions),
                        len.saturating_add(additions).saturating_mul(256),
                    )
                    .map_err(|_| BatchError::Limit)?;
            }
        }
        if diffs.len()
            > self
                .changelog
                .capacity()
                .saturating_sub(self.changelog.len())
        {
            budget
                .charge(
                    self.changelog.len(),
                    self.changelog
                        .len()
                        .saturating_add(diffs.len())
                        .saturating_mul(std::mem::size_of::<GraphDiff>())
                        .saturating_mul(2),
                )
                .map_err(|_| BatchError::Limit)?;
        }
        if freed_ids.len() > self.free_ids.capacity().saturating_sub(self.free_ids.len()) {
            budget
                .charge(
                    self.free_ids.len(),
                    self.free_ids
                        .len()
                        .saturating_add(freed_ids.len())
                        .saturating_mul(std::mem::size_of::<usize>())
                        .saturating_mul(2),
                )
                .map_err(|_| BatchError::Limit)?;
        }
        let indexed: Vec<_> = changes
            .iter()
            .filter(|change| change.changed)
            .map(|change| IndexChange {
                id: change.numeric_id,
                before: change.before.as_ref(),
                after: change.after.as_ref(),
            })
            .collect();
        let tags = self
            .tag_index
            .prepare_changes(&indexed, &mut budget)
            .map_err(|_| BatchError::Limit)?;
        let adjacency = self
            .adjacency
            .prepare_changes(&indexed, &mut budget)
            .map_err(|_| BatchError::Limit)?;
        let values = self
            .value_index
            .prepare_changes(&indexed, &mut budget)
            .map_err(|_| BatchError::Limit)?;
        // Account for whole retained units that this publication will discard.
        let mut count = self.changelog.len().saturating_add(diffs.len());
        let mut retained = self.changelog_bytes.saturating_add(bytes);
        for diff in &self.changelog {
            if count <= self.changelog_capacity
                && retained <= self.changelog_byte_capacity
                && diff.version == diff.span.first
            {
                break;
            }
            budget.charge(1, 0).map_err(|_| BatchError::Limit)?;
            count = count.saturating_sub(1);
            retained = retained.saturating_sub(diff.retained_bytes);
        }
        Ok(PreparedBatch {
            state: self.state(),
            index_generation: self.index_generation,
            after_revision: revision,
            retained_bytes: budget.used_bytes(),
            changes,
            diffs,
            tags,
            adjacency,
            values,
            free_ids_consumed,
            freed_ids,
            next_id,
        })
    }

    /// Checks all mutable prerequisites and reserves map/vector growth before
    /// effects. The publication phase consists only of infallible owned moves;
    /// ordinary allocator exhaustion may abort the process, as with native CRUD.
    pub fn apply_prepared(
        &mut self,
        prepared: PreparedBatch,
    ) -> Result<Option<CommitSpan>, BatchError> {
        if self.state() != prepared.state || self.index_generation != prepared.index_generation {
            return Err(BatchError::Conflict);
        }
        let additions = prepared
            .changes
            .iter()
            .filter(|change| change.op == DiffOp::Add)
            .count();
        self.id_map
            .try_reserve(additions)
            .map_err(|_| BatchError::Limit)?;
        self.reverse_id
            .try_reserve(additions)
            .map_err(|_| BatchError::Limit)?;
        self.free_ids
            .try_reserve(prepared.freed_ids.len())
            .map_err(|_| BatchError::Limit)?;
        self.changelog
            .try_reserve(prepared.diffs.len())
            .map_err(|_| BatchError::Limit)?;
        self.tag_index
            .reserve_prepared(&prepared.tags)
            .map_err(|_| BatchError::Limit)?;
        self.adjacency
            .reserve_prepared(&prepared.adjacency)
            .map_err(|_| BatchError::Limit)?;
        let span = prepared.span();
        for change in prepared.changes {
            if !change.changed {
                continue;
            }
            match change.op {
                DiffOp::Add => {
                    let (forward_key, reverse_key) =
                        change.map_keys.expect("prepared identity keys");
                    self.id_map.insert(forward_key, change.numeric_id);
                    self.reverse_id.insert(change.numeric_id, reverse_key);
                    self.entities
                        .insert(change.id, change.after.expect("prepared added row"));
                }
                DiffOp::Update => {
                    self.entities
                        .insert(change.id, change.after.expect("prepared patched row"));
                }
                DiffOp::Remove => {
                    self.entities.remove(&change.id);
                    self.id_map.remove(&change.id);
                    self.reverse_id.remove(&change.numeric_id);
                }
            }
        }
        self.free_ids
            .truncate(self.free_ids.len() - prepared.free_ids_consumed);
        self.free_ids.extend(prepared.freed_ids);
        self.next_id = prepared.next_id;
        self.tag_index.apply_prepared(prepared.tags);
        self.adjacency.apply_prepared(prepared.adjacency);
        self.value_index.apply_prepared(prepared.values);
        self.version = prepared.after_revision;
        if !prepared.diffs.is_empty() {
            self.append_committed_unit(prepared.diffs);
        }
        let target = query_cache_capacity_for(self.entities.len());
        let mut cache = self.query_cache.lock();
        cache.capacity = cache.capacity.max(target);
        Ok(span)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kinds::HRef;
    fn entity(id: &str) -> HDict {
        let mut row = HDict::new();
        row.set("id", Kind::Ref(HRef::from_val(id)));
        row
    }
    #[test]
    fn revision_capacity_is_checked_before_native_or_batch_effects() {
        let mut graph = EntityGraph::new();
        graph.version = u64::MAX - 1;
        graph.floor_version = graph.version;
        assert!(matches!(
            graph.prepare_batch(
                graph.version,
                &[
                    EntityOperation::Add(entity("a")),
                    EntityOperation::Add(entity("b"))
                ],
                BatchLimits::default()
            ),
            Err(BatchError::Entity(GraphError::RevisionExhausted))
        ));
        assert!(graph.is_empty());
        graph.add(entity("last")).unwrap();
        assert_eq!(graph.version(), u64::MAX);
        assert!(matches!(
            graph.add(entity("overflow")),
            Err(GraphError::RevisionExhausted)
        ));
        assert!(matches!(
            graph.remove("last"),
            Err(GraphError::RevisionExhausted)
        ));
        graph.update("last", HDict::new()).unwrap();
        assert!(graph.get("last").is_some());
        assert_eq!(graph.len(), 1);
        assert_eq!(
            graph
                .change_units_since(u64::MAX - 1)
                .unwrap()
                .next()
                .unwrap()
                .span,
            CommitSpan {
                first: u64::MAX,
                last: u64::MAX
            }
        );
    }
}

//! Database-independent, bounded restoration of authoritative graph state.
use super::*;
use crate::graph::GraphState;

#[derive(Debug, Clone, Copy)]
pub struct RecoveryLimits {
    pub max_rows: usize,
    pub max_changes: usize,
    pub max_bytes: usize,
    pub max_work: usize,
}
impl Default for RecoveryLimits {
    fn default() -> Self {
        Self {
            max_rows: 4096,
            max_changes: 4096,
            max_bytes: 64 * 1024 * 1024,
            max_work: 16_000_000,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RecoveryError {
    #[error("invalid durable graph state")]
    Invalid,
    #[error("durable graph recovery limit exceeded")]
    Limit,
}
#[derive(Debug, Clone)]
pub struct RecoveryChange {
    pub version: u64,
    pub span: CommitSpan,
    pub timestamp: i64,
    pub op: DiffOp,
    pub id: String,
    pub old: Option<HDict>,
    pub new: Option<HDict>,
    pub changed: Option<HDict>,
    pub previous: Option<HDict>,
}
impl From<&GraphDiff> for RecoveryChange {
    fn from(diff: &GraphDiff) -> Self {
        Self {
            version: diff.version,
            span: diff.span,
            timestamp: diff.timestamp,
            op: diff.op.clone(),
            id: diff.ref_val.clone(),
            old: diff.old.clone(),
            new: diff.new.clone(),
            changed: diff.changed_tags.clone(),
            previous: diff.previous_tags.clone(),
        }
    }
}
#[derive(Debug, Clone)]
pub struct GraphImage {
    pub state: GraphState,
    pub rows: Vec<HDict>,
    pub floor: u64,
    pub changes: Vec<RecoveryChange>,
}
impl RecoveryLimits {
    pub fn validate(self) -> Result<(), RecoveryError> {
        if self.max_rows == 0
            || self.max_rows > 100_000
            || self.max_changes == 0
            || self.max_changes > 100_000
            || self.max_bytes == 0
            || self.max_bytes > 256 * 1024 * 1024
            || self.max_work == 0
            || self.max_work > 64_000_000
        {
            return Err(RecoveryError::Limit);
        }
        Ok(())
    }
    fn budget(self) -> crate::graph::size::ValueBudget {
        crate::graph::size::ValueBudget::new(self.max_work, self.max_bytes, 64)
    }
}
impl RecoveryChange {
    fn into_diff(self, limit: usize) -> Result<GraphDiff, RecoveryError> {
        let valid = match self.op {
            DiffOp::Add => {
                self.new
                    .as_ref()
                    .is_some_and(|row| row.id().is_some_and(|id| id.val == self.id))
                    && self.old.is_none()
                    && self.changed.is_none()
                    && self.previous.is_none()
            }
            DiffOp::Remove => {
                self.old
                    .as_ref()
                    .is_some_and(|row| row.id().is_some_and(|id| id.val == self.id))
                    && self.new.is_none()
                    && self.changed.is_none()
                    && self.previous.is_none()
            }
            DiffOp::Update => {
                self.new.is_none()
                    && self.old.is_none()
                    && self.changed.is_some()
                    && self.previous.is_some()
            }
        };
        if !valid || self.id.is_empty() || self.id.len() > 4096 {
            return Err(RecoveryError::Invalid);
        }
        let bytes = crate::graph::size::diff_bytes(
            &self.id,
            [&self.old, &self.new, &self.changed, &self.previous]
                .into_iter()
                .filter_map(Option::as_ref),
            limit,
        )
        .ok_or(RecoveryError::Limit)?;
        Ok(GraphDiff {
            version: self.version,
            span: self.span,
            timestamp: self.timestamp,
            op: self.op,
            ref_val: self.id,
            old: self.old,
            new: self.new,
            changed_tags: self.changed,
            previous_tags: self.previous,
            retained_bytes: bytes,
        })
    }
}
impl EntityGraph {
    /// Capture bounded primary rows and complete retained changes before cloning.
    pub fn image(&self, limits: RecoveryLimits) -> Result<GraphImage, RecoveryError> {
        limits.validate()?;
        if self.entities.len() > limits.max_rows || self.changelog.len() > limits.max_changes {
            return Err(RecoveryError::Limit);
        }
        let mut budget = limits.budget();
        for row in self.entities.values() {
            budget.dict(row, 0).map_err(|_| RecoveryError::Limit)?;
        }
        for change in &self.changelog {
            budget
                .charge(1, change.retained_bytes())
                .map_err(|_| RecoveryError::Limit)?;
        }
        Ok(GraphImage {
            state: self.state(),
            rows: self.entities.values().cloned().collect(),
            floor: self.floor_version,
            changes: self.changelog.iter().map(RecoveryChange::from).collect(),
        })
    }
    /// Rebuild indexes from owned authoritative rows without minting an incarnation
    /// or replaying public revisions. The caller establishes durable/catalog provenance.
    pub fn restore(
        image: GraphImage,
        catalog: Arc<ActivatedCatalog>,
        limits: RecoveryLimits,
    ) -> Result<Self, RecoveryError> {
        limits.validate()?;
        if image.rows.len() > limits.max_rows || image.changes.len() > limits.max_changes {
            return Err(RecoveryError::Limit);
        }
        if image.state.incarnation == [0; 16]
            || image.state.catalog_generation == 0
            || image.floor > image.state.revision
        {
            return Err(RecoveryError::Invalid);
        }
        let mut budget = limits.budget();
        for row in &image.rows {
            budget.dict(row, 0).map_err(|_| RecoveryError::Limit)?;
        }
        let mut changes = std::collections::VecDeque::new();
        let mut bytes = 0usize;
        let mut previous = image.floor;
        let mut span: Option<CommitSpan> = None;
        for change in image.changes {
            if previous.checked_add(1) != Some(change.version) {
                return Err(RecoveryError::Invalid);
            }
            match span {
                None => {
                    if change.span.first != change.version
                        || change.span.last < change.version
                        || change.span.last > image.state.revision
                    {
                        return Err(RecoveryError::Invalid);
                    }
                    span = Some(change.span);
                }
                Some(unit) if unit != change.span => return Err(RecoveryError::Invalid),
                _ => {}
            }
            previous = change.version;
            if previous == change.span.last {
                span = None;
            }
            let diff = change.into_diff(limits.max_bytes)?;
            budget
                .charge(1, diff.retained_bytes())
                .map_err(|_| RecoveryError::Limit)?;
            bytes = bytes
                .checked_add(diff.retained_bytes())
                .ok_or(RecoveryError::Limit)?;
            changes.push_back(diff);
        }
        if previous != image.state.revision || span.is_some() {
            return Err(RecoveryError::Invalid);
        }
        // Loading rows must not retain or mint a replay log. Add validates IDs
        // and builds indexes; only the independently checked log is installed.
        let mut graph = Self::with_changelog_limits(1, 1);
        graph.namespace = Some(catalog.namespace().clone());
        graph.activated = Some(catalog);
        for row in image.rows {
            graph.add(row).map_err(|_| RecoveryError::Invalid)?;
        }
        graph.incarnation = image.state.incarnation;
        graph.version = image.state.revision;
        graph.catalog_generation = image.state.catalog_generation;
        graph.floor_version = image.floor;
        graph.changelog = changes;
        graph.changelog_bytes = bytes;
        graph.changelog_capacity = limits.max_changes;
        graph.changelog_byte_capacity = limits.max_bytes;
        graph.query_cache.lock().clear();
        Ok(graph)
    }
}

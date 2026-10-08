// Graph change tracking — records mutations for replication / undo.

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::data::HDict;

/// Default changelog capacity (50,000 entries).
pub const DEFAULT_CHANGELOG_CAPACITY: usize = 50_000;
/// Conservative retained-value budget for the changelog (64 MiB).
pub const DEFAULT_CHANGELOG_BYTES: usize = 64 * 1024 * 1024;

/// One complete public commit unit. This contains no submitter identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitSpan {
    pub first: u64,
    pub last: u64,
}
impl CommitSpan {
    pub(crate) fn singleton(version: u64) -> Self {
        Self {
            first: version,
            last: version,
        }
    }
}

/// Coherent entity/catalog identity captured under one graph guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphState {
    pub incarnation: [u8; 16],
    pub revision: u64,
    pub catalog_generation: u64,
}

/// Wakeup hints only; callers retrieve retained changes for authoritative data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphWake {
    Entities(GraphState),
    Reset(GraphState),
    Catalog(GraphState),
}

/// The kind of mutation that was applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffOp {
    /// A new entity was added.
    Add,
    /// An existing entity was updated.
    Update,
    /// An entity was removed.
    Remove,
}

/// A single mutation record.
///
/// Note: Each GraphDiff stores full entity clones (old and/or new). With the default
/// changelog capacity (50,000 entries), this can use significant memory for entities
/// with many tags or large string values.
#[derive(Debug, Clone)]
pub struct GraphDiff {
    /// The graph version *after* this mutation.
    pub version: u64,
    /// Common first/last revisions for every diff in this atomic unit.
    pub span: CommitSpan,
    pub(crate) retained_bytes: usize,
    /// Wall-clock timestamp as Unix nanoseconds (0 if unavailable).
    pub timestamp: i64,
    /// The kind of mutation.
    pub op: DiffOp,
    /// The entity's ref value.
    pub ref_val: String,
    /// The entity state before the mutation (Some for Remove; None for Add/Update).
    pub old: Option<HDict>,
    /// The entity state after the mutation (Some for Add; None for Remove/Update).
    pub new: Option<HDict>,
    /// For Update: only the tags that changed, with their **new** values.
    pub changed_tags: Option<HDict>,
    /// For Update: only the tags that changed, with their **previous** values.
    pub previous_tags: Option<HDict>,
}

impl GraphDiff {
    /// Conservative owned-value bytes retained for this diff.
    pub fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }
    /// Returns the current wall-clock time as Unix nanoseconds.
    pub(crate) fn now_nanos() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
            .unwrap_or(0)
    }
}

/// Error returned when a subscriber has fallen behind and the changelog
/// no longer contains entries at their requested version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangelogGap {
    /// The version the subscriber requested changes since.
    pub subscriber_version: u64,
    /// Last revision wholly discarded; changes strictly after it remain available.
    pub floor_version: u64,
}

impl fmt::Display for ChangelogGap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "changelog gap: subscriber at version {}, oldest available is {}",
            self.subscriber_version, self.floor_version
        )
    }
}

impl std::error::Error for ChangelogGap {}

/// An application feed position must be a complete unit boundary.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChangeCursorError {
    #[error(transparent)]
    Gap(#[from] ChangelogGap),
    #[error("change cursor exceeds current head {head}")]
    Future { head: u64 },
    #[error("change cursor lies inside commit span {span:?}")]
    InsideUnit { span: CommitSpan },
}

/// One borrowed complete unit. No whole-suffix allocation or cloning occurs.
pub struct ChangeUnit<'a> {
    pub span: CommitSpan,
    log: &'a std::collections::VecDeque<GraphDiff>,
    start: usize,
    end: usize,
}
impl ChangeUnit<'_> {
    pub fn len(&self) -> usize {
        self.end - self.start
    }
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
    pub fn diffs(&self) -> impl Iterator<Item = &GraphDiff> {
        self.log.range(self.start..self.end)
    }
    pub fn retained_bytes(&self) -> usize {
        self.diffs().map(|diff| diff.retained_bytes).sum()
    }
}

/// Identity/head/floor and span traversal borrow the same coherent graph view.
/// Consumers must charge and fully evaluate a unit before advancing a cursor.
pub struct ChangeUnits<'a> {
    pub state: GraphState,
    pub floor: u64,
    pub(crate) log: &'a std::collections::VecDeque<GraphDiff>,
    pub(crate) next: usize,
}
impl<'a> Iterator for ChangeUnits<'a> {
    type Item = ChangeUnit<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        let first = self.log.get(self.next)?;
        let count = usize::try_from(first.span.last - first.span.first)
            .expect("retained span fits usize")
            + 1;
        let start = self.next;
        self.next += count;
        Some(ChangeUnit {
            span: first.span,
            log: self.log,
            start,
            end: self.next,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_op_equality() {
        assert_eq!(DiffOp::Add, DiffOp::Add);
        assert_ne!(DiffOp::Add, DiffOp::Update);
        assert_ne!(DiffOp::Update, DiffOp::Remove);
    }

    #[test]
    fn diff_op_clone() {
        let op = DiffOp::Remove;
        let cloned = op.clone();
        assert_eq!(op, cloned);
    }

    #[test]
    fn graph_diff_construction() {
        let diff = GraphDiff {
            version: 1,
            span: CommitSpan::singleton(1),
            retained_bytes: 256,
            timestamp: 0,
            op: DiffOp::Add,
            ref_val: "site-1".to_string(),
            old: None,
            new: Some(HDict::new()),
            changed_tags: None,
            previous_tags: None,
        };
        assert_eq!(diff.version, 1);
        assert_eq!(diff.op, DiffOp::Add);
        assert_eq!(diff.ref_val, "site-1");
        assert!(diff.old.is_none());
        assert!(diff.new.is_some());
        assert!(diff.changed_tags.is_none());
        assert!(diff.previous_tags.is_none());
    }

    #[test]
    fn graph_diff_clone() {
        let diff = GraphDiff {
            version: 2,
            span: CommitSpan::singleton(2),
            retained_bytes: 256,
            timestamp: 0,
            op: DiffOp::Update,
            ref_val: "equip-1".to_string(),
            old: None,
            new: None,
            changed_tags: Some(HDict::new()),
            previous_tags: Some(HDict::new()),
        };
        let cloned = diff.clone();
        assert_eq!(cloned.version, 2);
        assert_eq!(cloned.op, DiffOp::Update);
        assert_eq!(cloned.ref_val, "equip-1");
        assert!(cloned.changed_tags.is_some());
        assert!(cloned.previous_tags.is_some());
    }

    #[test]
    fn changelog_gap_display() {
        let gap = ChangelogGap {
            subscriber_version: 5,
            floor_version: 100,
        };
        let msg = format!("{gap}");
        assert!(msg.contains("5"));
        assert!(msg.contains("100"));
    }

    #[test]
    fn changelog_gap_equality() {
        let a = ChangelogGap {
            subscriber_version: 1,
            floor_version: 10,
        };
        let b = ChangelogGap {
            subscriber_version: 1,
            floor_version: 10,
        };
        assert_eq!(a, b);
    }

    #[test]
    fn now_nanos_returns_positive() {
        let ts = GraphDiff::now_nanos();
        assert!(ts > 0, "timestamp should be positive, got {ts}");
    }
}

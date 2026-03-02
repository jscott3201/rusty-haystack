// Graph change tracking — records mutations for replication / undo.

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::data::HDict;

/// Default changelog capacity (50,000 entries).
pub const DEFAULT_CHANGELOG_CAPACITY: usize = 50_000;

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
#[derive(Debug, Clone)]
pub struct GraphDiff {
    /// The graph version *after* this mutation.
    pub version: u64,
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
    /// The lowest version still retained in the changelog.
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

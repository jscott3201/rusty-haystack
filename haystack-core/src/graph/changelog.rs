// Graph change tracking — records mutations for replication / undo.

use crate::data::HDict;

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
    /// The kind of mutation.
    pub op: DiffOp,
    /// The entity's ref value.
    pub ref_val: String,
    /// The entity state before the mutation (None for Add).
    pub old: Option<HDict>,
    /// The entity state after the mutation (None for Remove).
    pub new: Option<HDict>,
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
            op: DiffOp::Add,
            ref_val: "site-1".to_string(),
            old: None,
            new: Some(HDict::new()),
        };
        assert_eq!(diff.version, 1);
        assert_eq!(diff.op, DiffOp::Add);
        assert_eq!(diff.ref_val, "site-1");
        assert!(diff.old.is_none());
        assert!(diff.new.is_some());
    }

    #[test]
    fn graph_diff_clone() {
        let diff = GraphDiff {
            version: 2,
            op: DiffOp::Update,
            ref_val: "equip-1".to_string(),
            old: Some(HDict::new()),
            new: Some(HDict::new()),
        };
        let cloned = diff.clone();
        assert_eq!(cloned.version, 2);
        assert_eq!(cloned.op, DiffOp::Update);
        assert_eq!(cloned.ref_val, "equip-1");
    }
}

// Filter AST — the node types for Haystack filter expressions.

use crate::kinds::Kind;

/// Comparison operators supported in filter expressions.
#[derive(Debug, Clone, PartialEq)]
pub enum CmpOp {
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
}

/// A dotted path through entity references, e.g. `equipRef->siteRef->area`.
#[derive(Debug, Clone, PartialEq)]
pub struct Path(pub Vec<String>);

impl Path {
    /// Create a single-segment path.
    pub fn single(name: impl Into<String>) -> Self {
        Self(vec![name.into()])
    }

    /// Returns `true` if the path has exactly one segment.
    pub fn is_single(&self) -> bool {
        self.0.len() == 1
    }

    /// Returns the first segment of the path.
    pub fn first(&self) -> &str {
        &self.0[0]
    }
}

/// A node in the filter AST.
#[derive(Debug, Clone, PartialEq)]
pub enum FilterNode {
    /// Tag existence check: the path resolves to a non-null value.
    Has(Path),
    /// Tag absence check: the path does not resolve to a value.
    Missing(Path),
    /// Comparison: resolve path and compare with a literal value.
    Cmp { path: Path, op: CmpOp, val: Kind },
    /// Logical AND of two filters (short-circuit).
    And(Box<FilterNode>, Box<FilterNode>),
    /// Logical OR of two filters (short-circuit).
    Or(Box<FilterNode>, Box<FilterNode>),
    /// Structural spec match against a qualified type name, e.g. `ph::Point`.
    ///
    /// Resolved through [`DefNamespace::fits`](crate::ontology::DefNamespace::fits),
    /// so it needs a namespace: evaluated without one it reports false, which is
    /// indistinguishable from a genuine non-match. Use
    /// [`matches_with_ns`](crate::filter::matches_with_ns) rather than
    /// [`matches`](crate::filter::matches) wherever a namespace is available.
    SpecMatch(String),
}

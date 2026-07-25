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
    /// Type match against a qualified name, e.g. `ph::Point` or
    /// `ph.equips::WaterMeter`.
    ///
    /// Reads as "is this entity one of these": a def term matches an entity
    /// carrying that marker or a subtype of it, and a Xeto spec term matches an
    /// entity satisfying the spec's slots. Resolved through
    /// [`DefNamespace::fits_spec_term`](crate::ontology::DefNamespace::fits_spec_term).
    ///
    /// Note this is *not*
    /// [`DefNamespace::fits`](crate::ontology::DefNamespace::fits), which asks
    /// the different question of whether an entity is well-formed as a type.
    ///
    /// Needs a namespace: evaluated without one it reports false, which is
    /// indistinguishable from a genuine non-match. Use
    /// [`matches_with_ns`](crate::filter::matches_with_ns) rather than
    /// [`matches`](crate::filter::matches) wherever a namespace is available,
    /// and [`unresolved_specs`](crate::filter::unresolved_specs) to reject a
    /// filter naming a type the namespace does not define.
    SpecMatch(String),
}

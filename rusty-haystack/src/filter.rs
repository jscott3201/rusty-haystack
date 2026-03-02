// Filter bindings — parse, evaluate, and compose Haystack filter expressions.

use pyo3::class::basic::CompareOp;
use pyo3::prelude::*;

use haystack_core::filter;
use haystack_core::filter::{CmpOp, FilterNode, Path};

use crate::convert::{kind_to_py, py_to_kind};
use crate::data::PyHDict;
use crate::exceptions;

// ── CmpOp ──

/// Comparison operator for filter expressions.
#[pyclass(name = "CmpOp", frozen, eq, from_py_object)]
#[derive(Clone, PartialEq)]
pub enum PyCmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[pymethods]
impl PyCmpOp {
    fn __repr__(&self) -> &str {
        match self {
            PyCmpOp::Eq => "CmpOp.Eq",
            PyCmpOp::Ne => "CmpOp.Ne",
            PyCmpOp::Lt => "CmpOp.Lt",
            PyCmpOp::Le => "CmpOp.Le",
            PyCmpOp::Gt => "CmpOp.Gt",
            PyCmpOp::Ge => "CmpOp.Ge",
        }
    }
}

impl PyCmpOp {
    fn from_core(op: &CmpOp) -> Self {
        match op {
            CmpOp::Eq => PyCmpOp::Eq,
            CmpOp::Ne => PyCmpOp::Ne,
            CmpOp::Lt => PyCmpOp::Lt,
            CmpOp::Le => PyCmpOp::Le,
            CmpOp::Gt => PyCmpOp::Gt,
            CmpOp::Ge => PyCmpOp::Ge,
        }
    }

    fn to_core(&self) -> CmpOp {
        match self {
            PyCmpOp::Eq => CmpOp::Eq,
            PyCmpOp::Ne => CmpOp::Ne,
            PyCmpOp::Lt => CmpOp::Lt,
            PyCmpOp::Le => CmpOp::Le,
            PyCmpOp::Gt => CmpOp::Gt,
            PyCmpOp::Ge => CmpOp::Ge,
        }
    }
}

// ── Path ──

/// Haystack filter path — a dot-separated tag traversal path (e.g., "equipRef->siteRef").
#[pyclass(name = "Path", frozen, from_py_object)]
#[derive(Clone)]
pub struct PyPath {
    inner: Path,
}

#[pymethods]
impl PyPath {
    #[new]
    fn new(segments: Vec<String>) -> Self {
        Self {
            inner: Path(segments),
        }
    }

    /// Create a single-segment path.
    #[staticmethod]
    fn single(name: &str) -> Self {
        Self {
            inner: Path::single(name),
        }
    }

    /// Path segments.
    #[getter]
    fn segments(&self) -> Vec<String> {
        self.inner.0.clone()
    }

    /// True if this path has only one segment.
    fn is_single(&self) -> bool {
        self.inner.is_single()
    }

    /// First segment name.
    fn first(&self) -> &str {
        self.inner.first()
    }

    fn __repr__(&self) -> String {
        format!("Path({})", self.inner.0.join("->"))
    }

    fn __str__(&self) -> String {
        self.inner.0.join("->")
    }

    fn __len__(&self) -> usize {
        self.inner.0.len()
    }
}

// ── Filter (wraps FilterNode) ──

/// Haystack filter expression with builder pattern and AST inspection.
///
/// Filters can be constructed by parsing a string or using the builder API.
/// Supports Python & (and) and | (or) operators.
///
/// Examples:
///     f = Filter.parse("site and area > 1000")
///     f = Filter.has("site") & Filter.eq("area", Number(1000))
///     f.matches(entity)
#[pyclass(name = "Filter", from_py_object)]
#[derive(Clone)]
pub struct PyFilter {
    inner: FilterNode,
}

#[pymethods]
impl PyFilter {
    /// Parse a filter expression string.
    #[staticmethod]
    fn parse(expr: &str) -> PyResult<Self> {
        filter::parse_filter(expr)
            .map(|node| Self { inner: node })
            .map_err(|e| PyErr::new::<exceptions::FilterError, _>(e.to_string()))
    }

    // -- Builder methods --

    /// Create a "has" filter: tag is present.
    #[staticmethod]
    fn has(tag: &str) -> Self {
        Self {
            inner: FilterNode::Has(Path::single(tag)),
        }
    }

    /// Create a "missing" filter: tag is absent.
    #[staticmethod]
    fn missing(tag: &str) -> Self {
        Self {
            inner: FilterNode::Missing(Path::single(tag)),
        }
    }

    /// Create a comparison filter.
    #[staticmethod]
    fn cmp(path: &PyPath, op: &PyCmpOp, val: &Bound<'_, PyAny>) -> PyResult<Self> {
        let kind = py_to_kind(val)?;
        Ok(Self {
            inner: FilterNode::Cmp {
                path: path.inner.clone(),
                op: op.to_core(),
                val: kind,
            },
        })
    }

    /// Create an "equals" comparison: path == val.
    #[staticmethod]
    fn eq(tag: &str, val: &Bound<'_, PyAny>) -> PyResult<Self> {
        let kind = py_to_kind(val)?;
        Ok(Self {
            inner: FilterNode::Cmp {
                path: Path::single(tag),
                op: CmpOp::Eq,
                val: kind,
            },
        })
    }

    /// Combine with AND.
    fn and_(&self, other: &PyFilter) -> Self {
        Self {
            inner: FilterNode::And(Box::new(self.inner.clone()), Box::new(other.inner.clone())),
        }
    }

    /// Combine with OR.
    fn or_(&self, other: &PyFilter) -> Self {
        Self {
            inner: FilterNode::Or(Box::new(self.inner.clone()), Box::new(other.inner.clone())),
        }
    }

    // -- Inspection --

    /// Get the node type as a string: "has", "missing", "cmp", "and", "or", "specMatch".
    #[getter]
    fn node_type(&self) -> &str {
        match &self.inner {
            FilterNode::Has(_) => "has",
            FilterNode::Missing(_) => "missing",
            FilterNode::Cmp { .. } => "cmp",
            FilterNode::And(_, _) => "and",
            FilterNode::Or(_, _) => "or",
            FilterNode::SpecMatch(_) => "specMatch",
        }
    }

    /// For Has/Missing nodes, return the path. None for others.
    #[getter]
    fn path(&self) -> Option<PyPath> {
        match &self.inner {
            FilterNode::Has(p) | FilterNode::Missing(p) => Some(PyPath { inner: p.clone() }),
            FilterNode::Cmp { path, .. } => Some(PyPath {
                inner: path.clone(),
            }),
            _ => None,
        }
    }

    /// For Cmp nodes, return the comparison op. None for others.
    #[getter]
    fn op(&self) -> Option<PyCmpOp> {
        match &self.inner {
            FilterNode::Cmp { op, .. } => Some(PyCmpOp::from_core(op)),
            _ => None,
        }
    }

    /// For Cmp nodes, return the comparison value. None for others.
    fn val(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        match &self.inner {
            FilterNode::Cmp { val, .. } => Ok(Some(kind_to_py(py, val)?)),
            _ => Ok(None),
        }
    }

    /// For And/Or nodes, return (left, right). None for others.
    fn children(&self) -> Option<(PyFilter, PyFilter)> {
        match &self.inner {
            FilterNode::And(l, r) | FilterNode::Or(l, r) => Some((
                PyFilter { inner: *l.clone() },
                PyFilter { inner: *r.clone() },
            )),
            _ => None,
        }
    }

    // -- Evaluation --

    /// Evaluate this filter against an entity dict.
    fn matches(&self, entity: &PyHDict) -> bool {
        filter::matches(&self.inner, &entity.inner, None)
    }

    fn __repr__(&self) -> String {
        format!("{:?}", self.inner)
    }

    fn __str__(&self) -> String {
        format!("{:?}", self.inner)
    }

    fn __richcmp__(&self, other: &Self, op: CompareOp) -> PyResult<bool> {
        match op {
            CompareOp::Eq => Ok(self.inner == other.inner),
            CompareOp::Ne => Ok(self.inner != other.inner),
            _ => Err(PyErr::new::<pyo3::exceptions::PyTypeError, _>(
                "Filter only supports == and != comparison",
            )),
        }
    }

    /// Python & operator for AND composition.
    fn __and__(&self, other: &PyFilter) -> Self {
        self.and_(other)
    }

    /// Python | operator for OR composition.
    fn __or__(&self, other: &PyFilter) -> Self {
        self.or_(other)
    }
}

// ── Module-level functions (backward compatibility) ──

/// Parse a filter expression and return its AST as a debug string.
/// Raises FilterError if the expression is invalid.
#[pyfunction]
pub fn parse_filter(expr: &str) -> PyResult<String> {
    filter::parse_filter(expr)
        .map(|node| format!("{:?}", node))
        .map_err(|e| PyErr::new::<exceptions::FilterError, _>(e.to_string()))
}

/// Evaluate a filter expression against an entity dict.
/// Returns True if the entity matches the filter, False otherwise.
/// Raises FilterError if the filter expression is invalid.
#[pyfunction]
pub fn matches_filter(filter_expr: &str, entity: &PyHDict) -> PyResult<bool> {
    let node = filter::parse_filter(filter_expr)
        .map_err(|e| PyErr::new::<exceptions::FilterError, _>(e.to_string()))?;
    Ok(filter::matches(&node, &entity.inner, None))
}

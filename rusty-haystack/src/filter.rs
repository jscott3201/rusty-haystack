// Filter bindings — parse and evaluate Haystack filter expressions.

use pyo3::prelude::*;

use haystack_core::filter;

use crate::data::PyHDict;

/// Parse a filter expression and return its AST as a debug string.
/// Raises ValueError if the expression is invalid.
#[pyfunction]
pub fn parse_filter(expr: &str) -> PyResult<String> {
    filter::parse_filter(expr)
        .map(|node| format!("{:?}", node))
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(e.to_string()))
}

/// Evaluate a filter expression against an entity dict.
/// Returns True if the entity matches the filter, False otherwise.
/// Raises ValueError if the filter expression is invalid.
#[pyfunction]
pub fn matches_filter(filter_expr: &str, entity: &PyHDict) -> PyResult<bool> {
    let node = filter::parse_filter(filter_expr)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(e.to_string()))?;
    Ok(filter::matches(&node, &entity.inner, None))
}

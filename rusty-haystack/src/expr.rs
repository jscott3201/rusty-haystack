// Expression bindings — parse, evaluate, and inspect Haystack expressions.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use std::collections::HashMap;

use haystack_core::expr::{Expr, ExprContext};

use crate::convert::{kind_to_py, py_to_kind};

/// Parsed Haystack expression that can be evaluated with variable bindings.
///
/// Supports arithmetic (+, -, *, /, %), comparisons (==, !=, <, <=, >, >=),
/// logic (and, or, not), and built-in functions (abs, min, max, sqrt, clamp, avg, between).
///
/// Examples:
///     expr = Expr.parse("(temp - 32) * 5 / 9")
///     celsius = expr.eval({"temp": 72.0})
///
///     expr = Expr.parse("between(x, 0, 100)")
///     expr.eval({"x": 50.0})  # True
#[pyclass(name = "Expr", frozen)]
pub struct PyExpr {
    inner: Expr,
}

#[pymethods]
impl PyExpr {
    /// Parse an expression string into a ready-to-evaluate Expr.
    #[staticmethod]
    fn parse(source: &str) -> PyResult<Self> {
        Expr::parse(source)
            .map(|inner| PyExpr { inner })
            .map_err(|e| PyValueError::new_err(e.to_string()))
    }

    /// Evaluate the expression with the given variable bindings.
    ///
    /// Args:
    ///     variables: Dict mapping variable names to values. Values can be
    ///         Python int/float, str, bool, None, or Haystack Kind types
    ///         (Number, Ref, etc.).
    ///
    /// Returns:
    ///     The evaluated result as a Python object.
    fn eval(
        &self,
        variables: HashMap<String, Bound<'_, PyAny>>,
        py: Python<'_>,
    ) -> PyResult<Py<PyAny>> {
        let mut ctx = ExprContext::new();
        for (key, val) in &variables {
            let kind = py_to_kind(val)?;
            ctx.set(key, kind);
        }
        let result = self.inner.eval(&ctx);
        kind_to_py(py, &result)
    }

    /// Evaluate the expression and return the result as an f64.
    /// Returns NaN if the result is not a number.
    fn eval_number(&self, variables: HashMap<String, Bound<'_, PyAny>>) -> PyResult<f64> {
        let mut ctx = ExprContext::new();
        for (key, val) in &variables {
            let kind = py_to_kind(val)?;
            ctx.set(key, kind);
        }
        Ok(self.inner.eval_number(&ctx))
    }

    /// Evaluate the expression and return the result as a bool.
    /// Returns False if the result is not a boolean.
    fn eval_bool(&self, variables: HashMap<String, Bound<'_, PyAny>>) -> PyResult<bool> {
        let mut ctx = ExprContext::new();
        for (key, val) in &variables {
            let kind = py_to_kind(val)?;
            ctx.set(key, kind);
        }
        Ok(self.inner.eval_bool(&ctx))
    }

    /// Return the sorted, deduplicated list of variable names referenced in this expression.
    fn variables(&self) -> Vec<String> {
        self.inner.variables().to_vec()
    }

    fn __repr__(&self) -> String {
        let vars = self.inner.variables();
        if vars.is_empty() {
            "Expr()".to_string()
        } else {
            format!("Expr(vars=[{}])", vars.join(", "))
        }
    }
}

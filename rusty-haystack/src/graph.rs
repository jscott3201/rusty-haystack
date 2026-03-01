// Graph bindings — EntityGraph with CRUD, query, and ref traversal.

use pyo3::prelude::*;

use haystack_core::graph::EntityGraph;

use crate::data::{PyHDict, PyHGrid};

#[pyclass(name = "EntityGraph")]
pub struct PyEntityGraph {
    inner: EntityGraph,
}

#[pymethods]
impl PyEntityGraph {
    #[new]
    fn new() -> Self {
        Self {
            inner: EntityGraph::new(),
        }
    }

    /// Add an entity (must have an 'id' Ref tag). Returns the ref value string.
    fn add(&mut self, entity: &PyHDict) -> PyResult<String> {
        self.inner
            .add(entity.inner.clone())
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(e.to_string()))
    }

    /// Get an entity by ref value. Returns HDict or None.
    fn get(&self, py: Python<'_>, ref_val: &str) -> PyResult<Option<PyObject>> {
        match self.inner.get(ref_val) {
            Some(entity) => Ok(Some(
                PyHDict::from_core(entity)
                    .into_pyobject(py)?
                    .into_any()
                    .unbind(),
            )),
            None => Ok(None),
        }
    }

    /// Update an entity by merging changes.
    fn update(&mut self, ref_val: &str, changes: &PyHDict) -> PyResult<()> {
        self.inner
            .update(ref_val, changes.inner.clone())
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(e.to_string()))
    }

    /// Remove an entity by ref value. Returns the removed entity as HDict.
    fn remove(&mut self, py: Python<'_>, ref_val: &str) -> PyResult<PyObject> {
        let entity = self
            .inner
            .remove(ref_val)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(e.to_string()))?;
        Ok(PyHDict::from_core(&entity)
            .into_pyobject(py)?
            .into_any()
            .unbind())
    }

    /// Run a filter expression and return matching entities as a grid.
    #[pyo3(signature = (filter_expr, limit = 0))]
    fn read(&self, filter_expr: &str, limit: usize) -> PyResult<PyHGrid> {
        let grid = self
            .inner
            .read(filter_expr, limit)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(e.to_string()))?;
        Ok(PyHGrid::from_core(&grid))
    }

    /// Get ref values that the given entity points to.
    #[pyo3(signature = (ref_val, ref_type = None))]
    fn refs_from(&self, ref_val: &str, ref_type: Option<&str>) -> Vec<String> {
        self.inner.refs_from(ref_val, ref_type)
    }

    /// Get ref values of entities that point to the given entity.
    #[pyo3(signature = (ref_val, ref_type = None))]
    fn refs_to(&self, ref_val: &str, ref_type: Option<&str>) -> Vec<String> {
        self.inner.refs_to(ref_val, ref_type)
    }

    /// Export matching entities to a grid. Empty filter exports all entities.
    #[pyo3(signature = (filter_expr = ""))]
    fn to_grid(&self, filter_expr: &str) -> PyResult<PyHGrid> {
        let grid = self
            .inner
            .to_grid(filter_expr)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(e.to_string()))?;
        Ok(PyHGrid::from_core(&grid))
    }

    fn __len__(&self) -> usize {
        self.inner.len()
    }

    fn __contains__(&self, ref_val: &str) -> bool {
        self.inner.contains(ref_val)
    }

    #[getter]
    fn version(&self) -> u64 {
        self.inner.version()
    }

    fn __repr__(&self) -> String {
        format!(
            "EntityGraph(len={}, version={})",
            self.inner.len(),
            self.inner.version()
        )
    }
}

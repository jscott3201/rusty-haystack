// Graph bindings — EntityGraph with CRUD, query, and ref traversal.
// Also SharedGraph (thread-safe) and GraphDiff/DiffOp change tracking.

use pyo3::prelude::*;

use haystack_core::graph::{DiffOp, EntityGraph, GraphDiff, SharedGraph};

use crate::data::{PyHDict, PyHGrid};
use crate::exceptions;
use crate::ontology::PyDefNamespace;

// ── DiffOp ──

/// Type of change in a GraphDiff: Add, Update, or Remove.
#[pyclass(name = "DiffOp", frozen, eq, from_py_object)]
#[derive(Clone, PartialEq)]
pub enum PyDiffOp {
    Add,
    Update,
    Remove,
}

#[pymethods]
impl PyDiffOp {
    fn __repr__(&self) -> &str {
        match self {
            PyDiffOp::Add => "DiffOp.Add",
            PyDiffOp::Update => "DiffOp.Update",
            PyDiffOp::Remove => "DiffOp.Remove",
        }
    }
}

impl PyDiffOp {
    fn from_core(op: &DiffOp) -> Self {
        match op {
            DiffOp::Add => PyDiffOp::Add,
            DiffOp::Update => PyDiffOp::Update,
            DiffOp::Remove => PyDiffOp::Remove,
        }
    }
}

// ── GraphDiff ──

/// Record of a single change to the entity graph.
///
/// Contains the version, operation type, entity ref, and old/new entity state.
#[pyclass(name = "GraphDiff", frozen, from_py_object)]
#[derive(Clone)]
pub struct PyGraphDiff {
    #[pyo3(get)]
    pub version: u64,
    #[pyo3(get)]
    pub op: PyDiffOp,
    #[pyo3(get)]
    pub ref_val: String,
    old: Option<haystack_core::data::HDict>,
    new: Option<haystack_core::data::HDict>,
}

#[pymethods]
impl PyGraphDiff {
    /// Entity state before the mutation (None for Add).
    #[getter]
    fn old(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        match &self.old {
            Some(d) => Ok(Some(
                PyHDict::from_core(d).into_pyobject(py)?.into_any().unbind(),
            )),
            None => Ok(None),
        }
    }

    /// Entity state after the mutation (None for Remove).
    #[getter]
    fn new(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        match &self.new {
            Some(d) => Ok(Some(
                PyHDict::from_core(d).into_pyobject(py)?.into_any().unbind(),
            )),
            None => Ok(None),
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "GraphDiff(version={}, op={}, ref='{}')",
            self.version,
            self.op.__repr__(),
            self.ref_val
        )
    }
}

impl PyGraphDiff {
    fn from_core(d: &GraphDiff) -> Self {
        Self {
            version: d.version,
            op: PyDiffOp::from_core(&d.op),
            ref_val: d.ref_val.clone(),
            old: d.old.clone(),
            new: d.new.clone(),
        }
    }
}

// ── EntityGraph ──

/// In-memory entity graph with tag indexing, ref adjacency, and filter queries.
///
/// Stores Haystack entities (HDict) indexed by their `id` ref tag.
/// Supports CRUD operations, tag-bitmap-accelerated filter queries,
/// bidirectional ref traversal, and change tracking.
///
/// Examples:
///     g = EntityGraph()
///     g.add(HDict({"id": Ref("site-1"), "site": Marker(), "dis": "HQ"}))
///     results = g.read_all("site")
#[pyclass(name = "EntityGraph")]
pub struct PyEntityGraph {
    pub(crate) inner: EntityGraph,
}

#[pymethods]
impl PyEntityGraph {
    #[new]
    fn new() -> Self {
        Self {
            inner: EntityGraph::new(),
        }
    }

    /// Create a graph with an attached ontology namespace for validation.
    #[staticmethod]
    fn with_namespace(ns: &mut PyDefNamespace) -> Self {
        // Take the namespace out to avoid Clone (DefNamespace doesn't implement Clone)
        let taken = std::mem::replace(&mut ns.inner, haystack_core::ontology::DefNamespace::new());
        Self {
            inner: EntityGraph::with_namespace(taken),
        }
    }

    /// Bulk-load entities from an HGrid. Each row must have an 'id' Ref tag.
    #[staticmethod]
    #[pyo3(signature = (grid, ns = None))]
    fn from_grid(grid: &PyHGrid, ns: Option<&mut PyDefNamespace>) -> PyResult<Self> {
        let namespace = ns
            .map(|n| std::mem::replace(&mut n.inner, haystack_core::ontology::DefNamespace::new()));
        EntityGraph::from_grid(&grid.inner, namespace)
            .map(|g| Self { inner: g })
            .map_err(|e| PyErr::new::<exceptions::GraphError, _>(e.to_string()))
    }

    /// Add an entity (must have an 'id' Ref tag). Returns the ref value string.
    fn add(&mut self, entity: &PyHDict) -> PyResult<String> {
        self.inner
            .add(entity.inner.clone())
            .map_err(|e| PyErr::new::<exceptions::GraphError, _>(e.to_string()))
    }

    /// Add multiple entities from an HGrid. Returns number of entities added.
    fn add_grid(&mut self, grid: &PyHGrid) -> PyResult<usize> {
        let mut count = 0;
        for row in &grid.inner.rows {
            self.inner
                .add(row.clone())
                .map_err(|e| PyErr::new::<exceptions::GraphError, _>(e.to_string()))?;
            count += 1;
        }
        Ok(count)
    }

    /// Get an entity by ref value. Returns HDict or None.
    fn get(&self, py: Python<'_>, ref_val: &str) -> PyResult<Option<Py<PyAny>>> {
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
            .map_err(|e| PyErr::new::<exceptions::GraphError, _>(e.to_string()))
    }

    /// Remove an entity by ref value. Returns the removed entity as HDict.
    fn remove(&mut self, py: Python<'_>, ref_val: &str) -> PyResult<Py<PyAny>> {
        let entity = self
            .inner
            .remove(ref_val)
            .map_err(|e| PyErr::new::<exceptions::GraphError, _>(e.to_string()))?;
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
            .map_err(|e| PyErr::new::<exceptions::GraphError, _>(e.to_string()))?;
        Ok(PyHGrid::from_core(&grid))
    }

    /// Return all entities as a list of HDict.
    fn all(&self, py: Python<'_>) -> PyResult<Vec<Py<PyAny>>> {
        self.inner
            .all()
            .into_iter()
            .map(|d| Ok(PyHDict::from_core(d).into_pyobject(py)?.into_any().unbind()))
            .collect()
    }

    fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Validate all entities against the attached namespace.
    fn validate(&self) -> Vec<String> {
        self.inner
            .validate()
            .iter()
            .map(|issue| issue.to_string())
            .collect()
    }

    /// Return entities that structurally fit a given spec name.
    fn entities_fitting(&self, py: Python<'_>, spec_name: &str) -> PyResult<Vec<Py<PyAny>>> {
        self.inner
            .entities_fitting(spec_name)
            .into_iter()
            .map(|d| Ok(PyHDict::from_core(d).into_pyobject(py)?.into_any().unbind()))
            .collect()
    }

    /// Return changelog entries since a given graph version.
    fn changes_since(&self, version: u64) -> Vec<PyGraphDiff> {
        self.inner
            .changes_since(version)
            .iter()
            .map(|d| PyGraphDiff::from_core(d))
            .collect()
    }

    /// Enable a B-tree value index on a tag for faster range queries.
    fn index_field(&mut self, field: &str) {
        self.inner.index_field(field);
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

    /// Return all edges as list of (source, ref_tag, target) tuples.
    fn all_edges(&self) -> Vec<(String, String, String)> {
        self.inner.all_edges()
    }

    /// BFS neighborhood: entities and edges within `hops` of `ref_val`.
    ///
    /// Returns (entities, edges) where edges are (source, ref_tag, target) tuples.
    #[pyo3(signature = (ref_val, hops = 1, ref_types = None))]
    fn neighbors(
        &self,
        py: Python<'_>,
        ref_val: &str,
        hops: usize,
        ref_types: Option<Vec<String>>,
    ) -> PyResult<(Vec<Py<PyAny>>, Vec<(String, String, String)>)> {
        let refs: Option<Vec<&str>> = ref_types
            .as_ref()
            .map(|v| v.iter().map(|s| s.as_str()).collect());
        let (entities, edges) = self.inner.neighbors(ref_val, hops, refs.as_deref());
        let py_entities: Vec<Py<PyAny>> = entities
            .into_iter()
            .map(|e| Ok(PyHDict::from_core(e).into_pyobject(py)?.into_any().unbind()))
            .collect::<PyResult<_>>()?;
        Ok((py_entities, edges))
    }

    /// BFS shortest path from `from_ref` to `to_ref`. Returns list of ref strings.
    fn shortest_path(&self, from_ref: &str, to_ref: &str) -> Vec<String> {
        self.inner.shortest_path(from_ref, to_ref)
    }

    /// Return the subtree rooted at `root` up to `max_depth` levels.
    ///
    /// Returns list of (entity_dict, depth) tuples.
    #[pyo3(signature = (root, max_depth = 10))]
    fn subtree(
        &self,
        py: Python<'_>,
        root: &str,
        max_depth: usize,
    ) -> PyResult<Vec<(Py<PyAny>, usize)>> {
        self.inner
            .subtree(root, max_depth)
            .into_iter()
            .map(|(e, d)| {
                Ok((
                    PyHDict::from_core(e).into_pyobject(py)?.into_any().unbind(),
                    d,
                ))
            })
            .collect()
    }

    /// Export matching entities to a grid. Empty filter exports all entities.
    #[pyo3(signature = (filter_expr = ""))]
    fn to_grid(&self, filter_expr: &str) -> PyResult<PyHGrid> {
        let grid = self
            .inner
            .to_grid(filter_expr)
            .map_err(|e| PyErr::new::<exceptions::GraphError, _>(e.to_string()))?;
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

// ── SharedGraph (thread-safe wrapper) ──

/// Thread-safe shared entity graph backed by Arc<RwLock<EntityGraph>>.
///
/// Safe for concurrent reads and serialized writes. Use for multi-threaded
/// server scenarios or sharing a graph between Python threads.
#[pyclass(name = "SharedGraph")]
pub struct PySharedGraph {
    inner: SharedGraph,
}

impl PySharedGraph {
    pub fn clone_inner(&self) -> SharedGraph {
        self.inner.clone()
    }
}

#[pymethods]
impl PySharedGraph {
    /// Create a new SharedGraph wrapping an EntityGraph.
    #[new]
    #[pyo3(signature = (graph = None))]
    fn new(graph: Option<&mut PyEntityGraph>) -> Self {
        let eg = match graph {
            Some(g) => std::mem::replace(&mut g.inner, EntityGraph::new()),
            None => EntityGraph::new(),
        };
        Self {
            inner: SharedGraph::new(eg),
        }
    }

    /// Create a SharedGraph from an HGrid.
    #[staticmethod]
    #[pyo3(signature = (grid, ns = None))]
    fn from_grid(grid: &PyHGrid, ns: Option<&mut PyDefNamespace>) -> PyResult<Self> {
        let namespace = ns
            .map(|n| std::mem::replace(&mut n.inner, haystack_core::ontology::DefNamespace::new()));
        let eg = EntityGraph::from_grid(&grid.inner, namespace)
            .map_err(|e| PyErr::new::<exceptions::GraphError, _>(e.to_string()))?;
        Ok(Self {
            inner: SharedGraph::new(eg),
        })
    }

    /// Add an entity to the shared graph. Returns the ref value string.
    fn add(&self, entity: &PyHDict) -> PyResult<String> {
        self.inner
            .add(entity.inner.clone())
            .map_err(|e| PyErr::new::<exceptions::GraphError, _>(e.to_string()))
    }

    /// Get an entity by ref value. Returns HDict or None.
    fn get(&self, py: Python<'_>, ref_val: &str) -> PyResult<Option<Py<PyAny>>> {
        match self.inner.get(ref_val) {
            Some(entity) => Ok(Some(
                PyHDict::from_core(&entity)
                    .into_pyobject(py)?
                    .into_any()
                    .unbind(),
            )),
            None => Ok(None),
        }
    }

    /// Update an entity by merging changes into existing tags.
    fn update(&self, ref_val: &str, changes: &PyHDict) -> PyResult<()> {
        self.inner
            .update(ref_val, changes.inner.clone())
            .map_err(|e| PyErr::new::<exceptions::GraphError, _>(e.to_string()))
    }

    /// Remove an entity by ref value. Returns the removed HDict.
    fn remove(&self, py: Python<'_>, ref_val: &str) -> PyResult<Py<PyAny>> {
        let entity = self
            .inner
            .remove(ref_val)
            .map_err(|e| PyErr::new::<exceptions::GraphError, _>(e.to_string()))?;
        Ok(PyHDict::from_core(&entity)
            .into_pyobject(py)?
            .into_any()
            .unbind())
    }

    /// Query entities matching a Haystack filter. Returns an HGrid of results.
    #[pyo3(signature = (filter_expr, limit = 0))]
    fn read(&self, filter_expr: &str, limit: usize) -> PyResult<PyHGrid> {
        let grid = self
            .inner
            .read_filter(filter_expr, limit)
            .map_err(|e| PyErr::new::<exceptions::GraphError, _>(e.to_string()))?;
        Ok(PyHGrid::from_core(&grid))
    }

    /// Return all entities as a list of HDict.
    fn all(&self, py: Python<'_>) -> PyResult<Vec<Py<PyAny>>> {
        self.inner
            .all_entities()
            .into_iter()
            .map(|d| {
                Ok(PyHDict::from_core(&d)
                    .into_pyobject(py)?
                    .into_any()
                    .unbind())
            })
            .collect()
    }

    /// Return True if the graph contains no entities.
    fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Return True if an entity with the given ref exists.
    fn contains(&self, ref_val: &str) -> bool {
        self.inner.contains(ref_val)
    }

    fn __len__(&self) -> usize {
        self.inner.len()
    }

    #[getter]
    fn version(&self) -> u64 {
        self.inner.version()
    }

    /// Get ref values that the given entity points to via ref tags.
    #[pyo3(signature = (ref_val, ref_type = None))]
    fn refs_from(&self, ref_val: &str, ref_type: Option<&str>) -> Vec<String> {
        self.inner.refs_from(ref_val, ref_type)
    }

    /// Get ref values of entities pointing to the given entity.
    #[pyo3(signature = (ref_val, ref_type = None))]
    fn refs_to(&self, ref_val: &str, ref_type: Option<&str>) -> Vec<String> {
        self.inner.refs_to(ref_val, ref_type)
    }

    /// Return all edges as list of (source, ref_tag, target) tuples.
    fn all_edges(&self) -> Vec<(String, String, String)> {
        self.inner.all_edges()
    }

    /// BFS neighborhood: entities and edges within `hops` of `ref_val`.
    ///
    /// Returns (entities, edges) where edges are (source, ref_tag, target) tuples.
    #[pyo3(signature = (ref_val, hops = 1, ref_types = None))]
    fn neighbors(
        &self,
        py: Python<'_>,
        ref_val: &str,
        hops: usize,
        ref_types: Option<Vec<String>>,
    ) -> PyResult<(Vec<Py<PyAny>>, Vec<(String, String, String)>)> {
        let refs: Option<Vec<&str>> = ref_types
            .as_ref()
            .map(|v| v.iter().map(|s| s.as_str()).collect());
        let (entities, edges) = self.inner.neighbors(ref_val, hops, refs.as_deref());
        let py_entities: Vec<Py<PyAny>> = entities
            .into_iter()
            .map(|d| {
                Ok(PyHDict::from_core(&d)
                    .into_pyobject(py)?
                    .into_any()
                    .unbind())
            })
            .collect::<PyResult<_>>()?;
        Ok((py_entities, edges))
    }

    /// BFS shortest path from `from_ref` to `to_ref`. Returns list of ref strings.
    fn shortest_path(&self, from_ref: &str, to_ref: &str) -> Vec<String> {
        self.inner.shortest_path(from_ref, to_ref)
    }

    /// Return the subtree rooted at `root` up to `max_depth` levels.
    ///
    /// Returns list of (entity_dict, depth) tuples.
    #[pyo3(signature = (root, max_depth = 10))]
    fn subtree(
        &self,
        py: Python<'_>,
        root: &str,
        max_depth: usize,
    ) -> PyResult<Vec<(Py<PyAny>, usize)>> {
        self.inner
            .subtree(root, max_depth)
            .into_iter()
            .map(|(e, d)| {
                Ok((
                    PyHDict::from_core(&e)
                        .into_pyobject(py)?
                        .into_any()
                        .unbind(),
                    d,
                ))
            })
            .collect()
    }

    /// Return changelog entries since a given graph version.
    fn changes_since(&self, version: u64) -> Vec<PyGraphDiff> {
        self.inner
            .changes_since(version)
            .iter()
            .map(PyGraphDiff::from_core)
            .collect()
    }

    /// Return entities that structurally fit a given ontology spec.
    fn entities_fitting(&self, py: Python<'_>, spec_name: &str) -> PyResult<Vec<Py<PyAny>>> {
        self.inner
            .entities_fitting(spec_name)
            .into_iter()
            .map(|d| {
                Ok(PyHDict::from_core(&d)
                    .into_pyobject(py)?
                    .into_any()
                    .unbind())
            })
            .collect()
    }

    /// Validate all entities against the attached namespace. Returns issue strings.
    fn validate(&self) -> Vec<String> {
        self.inner
            .validate()
            .iter()
            .map(|issue| issue.to_string())
            .collect()
    }

    fn __repr__(&self) -> String {
        format!(
            "SharedGraph(len={}, version={})",
            self.inner.len(),
            self.inner.version()
        )
    }
}

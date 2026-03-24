// Graph bindings — EntityGraph with CRUD, query, and ref traversal.
// Also SharedGraph (thread-safe) and GraphDiff/DiffOp change tracking.

use pyo3::prelude::*;
use pyo3::types::PyDict;

use haystack_core::graph::{DiffOp, EntityGraph, GraphDiff, HierarchyNode, SharedGraph};

use crate::data::{PyHDict, PyHGrid};
use crate::exceptions;
use crate::ontology::PyDefNamespace;

/// Convert a HierarchyNode tree to a nested Python dict.
fn hierarchy_node_to_py(py: Python<'_>, node: &HierarchyNode) -> PyResult<Py<PyAny>> {
    let dict = PyDict::new(py);
    let entity = PyHDict::from_core(&node.entity)
        .into_pyobject(py)?
        .into_any()
        .unbind();
    dict.set_item("entity", entity)?;
    dict.set_item("depth", node.depth)?;
    let children: Vec<Py<PyAny>> = node
        .children
        .iter()
        .map(|c| hierarchy_node_to_py(py, c))
        .collect::<PyResult<_>>()?;
    dict.set_item("children", children)?;
    Ok(dict.into_any().unbind())
}

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
    pub timestamp: i64,
    #[pyo3(get)]
    pub op: PyDiffOp,
    #[pyo3(get)]
    pub ref_val: String,
    old: Option<haystack_core::data::HDict>,
    new: Option<haystack_core::data::HDict>,
    changed_tags: Option<haystack_core::data::HDict>,
    previous_tags: Option<haystack_core::data::HDict>,
}

#[pymethods]
impl PyGraphDiff {
    /// Entity state before the mutation (Some for Remove; None for Add/Update).
    #[getter]
    fn old(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        match &self.old {
            Some(d) => Ok(Some(
                PyHDict::from_core(d).into_pyobject(py)?.into_any().unbind(),
            )),
            None => Ok(None),
        }
    }

    /// Entity state after the mutation (Some for Add; None for Remove/Update).
    #[getter]
    fn new(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        match &self.new {
            Some(d) => Ok(Some(
                PyHDict::from_core(d).into_pyobject(py)?.into_any().unbind(),
            )),
            None => Ok(None),
        }
    }

    /// For Update: tags that changed with their new values.
    #[getter]
    fn changed_tags(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        match &self.changed_tags {
            Some(d) => Ok(Some(
                PyHDict::from_core(d).into_pyobject(py)?.into_any().unbind(),
            )),
            None => Ok(None),
        }
    }

    /// For Update: tags that changed with their previous values.
    #[getter]
    fn previous_tags(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        match &self.previous_tags {
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
            timestamp: d.timestamp,
            op: PyDiffOp::from_core(&d.op),
            ref_val: d.ref_val.clone(),
            old: d.old.clone(),
            new: d.new.clone(),
            changed_tags: d.changed_tags.clone(),
            previous_tags: d.previous_tags.clone(),
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

    /// Return changelog entries since a given graph version.
    ///
    /// Raises RuntimeError if the subscriber has fallen behind (changelog gap).
    fn changes_since(&self, version: u64) -> PyResult<Vec<PyGraphDiff>> {
        self.inner
            .changes_since(version)
            .map(|refs| refs.iter().map(|d| PyGraphDiff::from_core(d)).collect())
            .map_err(|gap| {
                pyo3::exceptions::PyRuntimeError::new_err(format!(
                    "changelog gap: requested version {}, floor is {}",
                    gap.subscriber_version, gap.floor_version
                ))
            })
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

    // ── Traversal methods ──

    /// Walk the ref chain from an entity following the given ref tags in order.
    ///
    /// Returns a list of HDict entities along the chain.
    fn ref_chain(
        &self,
        py: Python<'_>,
        ref_val: &str,
        ref_tags: Vec<String>,
    ) -> PyResult<Vec<Py<PyAny>>> {
        let tags: Vec<&str> = ref_tags.iter().map(|s| s.as_str()).collect();
        self.inner
            .ref_chain(ref_val, &tags)
            .into_iter()
            .map(|d| Ok(PyHDict::from_core(d).into_pyobject(py)?.into_any().unbind()))
            .collect()
    }

    /// Find the site entity for any entity by walking up the ref chain.
    fn site_for(&self, py: Python<'_>, ref_val: &str) -> PyResult<Option<Py<PyAny>>> {
        match self.inner.site_for(ref_val) {
            Some(d) => Ok(Some(
                PyHDict::from_core(d).into_pyobject(py)?.into_any().unbind(),
            )),
            None => Ok(None),
        }
    }

    /// Get all direct children of an entity (entities whose xxxRef points to it).
    fn children_of(&self, py: Python<'_>, ref_val: &str) -> PyResult<Vec<Py<PyAny>>> {
        self.inner
            .children(ref_val)
            .into_iter()
            .map(|d| Ok(PyHDict::from_core(d).into_pyobject(py)?.into_any().unbind()))
            .collect()
    }

    /// Get all points for an equip, optionally filtered.
    #[pyo3(signature = (equip_ref, filter = None))]
    fn equip_points(
        &self,
        py: Python<'_>,
        equip_ref: &str,
        filter: Option<&str>,
    ) -> PyResult<Vec<Py<PyAny>>> {
        self.inner
            .equip_points(equip_ref, filter)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?
            .into_iter()
            .map(|d| Ok(PyHDict::from_core(d).into_pyobject(py)?.into_any().unbind()))
            .collect()
    }

    /// Build a hierarchy tree from a root entity.
    ///
    /// Returns a nested dict: {"entity": HDict, "depth": int, "children": [...]},
    /// or None if the root is not found.
    #[pyo3(signature = (root, max_depth = 10))]
    fn hierarchy_tree(
        &self,
        py: Python<'_>,
        root: &str,
        max_depth: usize,
    ) -> PyResult<Option<Py<PyAny>>> {
        match self.inner.hierarchy_tree(root, max_depth) {
            Some(node) => Ok(Some(hierarchy_node_to_py(py, &node)?)),
            None => Ok(None),
        }
    }

    /// Classify an entity by its most specific type tag.
    fn classify(&self, ref_val: &str) -> Option<String> {
        self.inner.classify(ref_val)
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
    fn read(&self, py: Python<'_>, filter_expr: &str, limit: usize) -> PyResult<PyHGrid> {
        let filter = filter_expr.to_string();
        let inner = self.inner.clone();
        let grid = py
            .detach(move || inner.read_filter(&filter, limit))
            .map_err(|e| PyErr::new::<exceptions::GraphError, _>(e.to_string()))?;
        Ok(PyHGrid::from_core(&grid))
    }

    /// Return all entities as a list of HDict.
    fn all(&self, py: Python<'_>) -> PyResult<Vec<Py<PyAny>>> {
        let inner = self.inner.clone();
        let entities = py.detach(move || inner.all_entities());
        entities
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

    /// Return changelog entries since a given graph version.
    ///
    /// Raises RuntimeError if the subscriber has fallen behind (changelog gap).
    fn changes_since(&self, version: u64) -> PyResult<Vec<PyGraphDiff>> {
        self.inner
            .changes_since(version)
            .map(|refs| refs.iter().map(PyGraphDiff::from_core).collect())
            .map_err(|gap| {
                pyo3::exceptions::PyRuntimeError::new_err(format!(
                    "changelog gap: requested version {}, floor is {}",
                    gap.subscriber_version, gap.floor_version
                ))
            })
    }

    /// Validate all entities against the attached namespace. Returns issue strings.
    fn validate(&self) -> Vec<String> {
        self.inner
            .validate()
            .iter()
            .map(|issue| issue.to_string())
            .collect()
    }

    /// Number of active broadcast subscribers.
    #[getter]
    fn subscriber_count(&self) -> usize {
        self.inner.subscriber_count()
    }

    fn __repr__(&self) -> String {
        format!(
            "SharedGraph(len={}, version={})",
            self.inner.len(),
            self.inner.version()
        )
    }

    // ── Traversal methods ──

    /// Walk the ref chain from an entity following the given ref tags in order.
    fn ref_chain(
        &self,
        py: Python<'_>,
        ref_val: &str,
        ref_tags: Vec<String>,
    ) -> PyResult<Vec<Py<PyAny>>> {
        let tags: Vec<&str> = ref_tags.iter().map(|s| s.as_str()).collect();
        self.inner
            .ref_chain(ref_val, &tags)
            .into_iter()
            .map(|d| {
                Ok(PyHDict::from_core(&d)
                    .into_pyobject(py)?
                    .into_any()
                    .unbind())
            })
            .collect()
    }

    /// Find the site entity for any entity by walking up the ref chain.
    fn site_for(&self, py: Python<'_>, ref_val: &str) -> PyResult<Option<Py<PyAny>>> {
        match self.inner.site_for(ref_val) {
            Some(d) => Ok(Some(
                PyHDict::from_core(&d)
                    .into_pyobject(py)?
                    .into_any()
                    .unbind(),
            )),
            None => Ok(None),
        }
    }

    /// Get all direct children of an entity.
    fn children_of(&self, py: Python<'_>, ref_val: &str) -> PyResult<Vec<Py<PyAny>>> {
        self.inner
            .children(ref_val)
            .into_iter()
            .map(|d| {
                Ok(PyHDict::from_core(&d)
                    .into_pyobject(py)?
                    .into_any()
                    .unbind())
            })
            .collect()
    }

    /// Get all points for an equip, optionally filtered.
    #[pyo3(signature = (equip_ref, filter = None))]
    fn equip_points(
        &self,
        py: Python<'_>,
        equip_ref: &str,
        filter: Option<&str>,
    ) -> PyResult<Vec<Py<PyAny>>> {
        self.inner
            .equip_points(equip_ref, filter)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?
            .into_iter()
            .map(|d| Ok(PyHDict::from_core(&d).into_pyobject(py)?.into_any().unbind()))
            .collect()
    }

    /// Build a hierarchy tree from a root entity.
    ///
    /// Returns a nested dict or None if the root is not found.
    #[pyo3(signature = (root, max_depth = 10))]
    fn hierarchy_tree(
        &self,
        py: Python<'_>,
        root: &str,
        max_depth: usize,
    ) -> PyResult<Option<Py<PyAny>>> {
        match self.inner.hierarchy_tree(root, max_depth) {
            Some(node) => Ok(Some(hierarchy_node_to_py(py, &node)?)),
            None => Ok(None),
        }
    }

    /// Classify an entity by its most specific type tag.
    fn classify(&self, ref_val: &str) -> Option<String> {
        self.inner.classify(ref_val)
    }
}

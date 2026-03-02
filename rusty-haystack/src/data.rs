// Python wrappers for Haystack data structures: HDict, HGrid, HList, HCol.

use pyo3::class::basic::CompareOp;
use pyo3::prelude::*;
use pyo3::types::PyDict;

use haystack_core::data;
use haystack_core::kinds::Kind;

use crate::convert::{kind_to_py, py_to_kind};
use crate::kinds::PyRef_;

// ── HDict ──

/// Haystack Dict — a collection of name/value tag pairs.
///
/// The fundamental data type for representing entities and records.
/// Tags are string keys mapping to Haystack Kind values.
///
/// Examples:
///     d = HDict({"dis": "Main Site", "area": Number(5000, "ft²")})
///     d.get("dis")  # "Main Site"
///     d.has("area")  # True
#[pyclass(name = "HDict")]
pub struct PyHDict {
    pub(crate) inner: data::HDict,
}

#[pymethods]
impl PyHDict {
    /// Create a new HDict, optionally populated from a Python dict.
    #[new]
    #[pyo3(signature = (tags = None))]
    fn new(tags: Option<&Bound<'_, PyDict>>) -> PyResult<Self> {
        let mut inner = data::HDict::new();
        if let Some(py_dict) = tags {
            for (key, value) in py_dict.iter() {
                let name: String = key.extract()?;
                let kind = py_to_kind(&value)?;
                inner.set(name, kind);
            }
        }
        Ok(Self { inner })
    }

    /// Return True if the tag is present in this dict.
    fn has(&self, name: &str) -> bool {
        self.inner.has(name)
    }

    /// Get a tag value by name, returning default (None) if missing.
    #[pyo3(signature = (name, default = None))]
    fn get(&self, py: Python<'_>, name: &str, default: Option<Py<PyAny>>) -> PyResult<Py<PyAny>> {
        match self.inner.get(name) {
            Some(kind) => kind_to_py(py, kind),
            None => Ok(default.unwrap_or_else(|| py.None())),
        }
    }

    /// Return True if the tag is absent from this dict.
    fn missing(&self, name: &str) -> bool {
        self.inner.missing(name)
    }

    /// Return the 'id' Ref tag value, or None if not present.
    fn id(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        match self.inner.id() {
            Some(r) => {
                let py_ref = PyRef_::from_core(r);
                Ok(Some(py_ref.into_pyobject(py)?.into_any().unbind()))
            }
            None => Ok(None),
        }
    }

    /// Return the display string ('dis' tag), or None.
    fn dis(&self) -> Option<&str> {
        self.inner.dis()
    }

    /// Return True if the dict has no tags.
    fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Set a tag value. Overwrites existing value if present.
    fn set(&mut self, py: Python<'_>, name: String, val: &Bound<'_, PyAny>) -> PyResult<()> {
        let _ = py;
        let kind = py_to_kind(val)?;
        self.inner.set(name, kind);
        Ok(())
    }

    /// Merge tags from another HDict into this one. Existing tags are overwritten.
    fn merge(&mut self, other: &PyHDict) {
        self.inner.merge(&other.inner);
    }

    /// Create an HDict from a Python dict of string keys to Haystack values.
    #[staticmethod]
    fn from_dict(d: &Bound<'_, PyDict>) -> PyResult<Self> {
        let mut inner = data::HDict::new();
        for (key, value) in d.iter() {
            let name: String = key.extract()?;
            let kind = py_to_kind(&value)?;
            inner.set(name, kind);
        }
        Ok(Self { inner })
    }

    /// Convert to a Python dict with string keys.
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        for (k, v) in self.inner.iter() {
            dict.set_item(k, kind_to_py(py, v)?)?;
        }
        Ok(dict)
    }

    /// Return tag names in sorted order.
    fn sorted_keys(&self) -> Vec<String> {
        self.inner
            .sorted_iter()
            .into_iter()
            .map(|(k, _)| k.to_string())
            .collect()
    }

    /// Return a shallow copy of this dict.
    fn copy(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }

    /// Return the ref value string of the 'id' tag, or None.
    #[getter]
    fn ref_val(&self) -> Option<String> {
        self.inner.id().map(|r| r.val.clone())
    }

    /// Return all tag names as a list of strings.
    fn tag_names(&self) -> Vec<String> {
        self.inner.tag_names().map(|s| s.to_string()).collect()
    }

    /// Return tag names (alias for tag_names()).
    fn keys(&self) -> Vec<String> {
        self.inner.tag_names().map(|s| s.to_string()).collect()
    }

    /// Return all tag values as a list.
    fn values(&self, py: Python<'_>) -> PyResult<Vec<Py<PyAny>>> {
        self.inner.iter().map(|(_, v)| kind_to_py(py, v)).collect()
    }

    /// Return (name, value) pairs as a list of tuples.
    fn items(&self, py: Python<'_>) -> PyResult<Vec<(String, Py<PyAny>)>> {
        self.inner
            .iter()
            .map(|(k, v)| Ok((k.to_string(), kind_to_py(py, v)?)))
            .collect()
    }

    fn __getitem__(&self, py: Python<'_>, key: &str) -> PyResult<Py<PyAny>> {
        match self.inner.get(key) {
            Some(kind) => kind_to_py(py, kind),
            None => Err(PyErr::new::<pyo3::exceptions::PyKeyError, _>(
                key.to_string(),
            )),
        }
    }

    fn __setitem__(&mut self, key: String, val: &Bound<'_, PyAny>) -> PyResult<()> {
        let kind = py_to_kind(val)?;
        self.inner.set(key, kind);
        Ok(())
    }

    fn __delitem__(&mut self, key: &str) -> PyResult<()> {
        match self.inner.remove_tag(key) {
            Some(_) => Ok(()),
            None => Err(PyErr::new::<pyo3::exceptions::PyKeyError, _>(
                key.to_string(),
            )),
        }
    }

    fn __contains__(&self, key: &str) -> bool {
        self.inner.has(key)
    }

    fn __len__(&self) -> usize {
        self.inner.len()
    }

    fn __iter__(&self) -> PyDictKeyIter {
        let keys: Vec<String> = self.inner.tag_names().map(|s| s.to_string()).collect();
        PyDictKeyIter { keys, index: 0 }
    }

    fn __repr__(&self) -> String {
        self.inner.to_string()
    }

    fn __str__(&self) -> String {
        self.inner.to_string()
    }

    fn __richcmp__(&self, other: &Self, op: CompareOp) -> PyResult<bool> {
        match op {
            CompareOp::Eq => Ok(self.inner == other.inner),
            CompareOp::Ne => Ok(self.inner != other.inner),
            _ => Err(PyErr::new::<pyo3::exceptions::PyTypeError, _>(
                "HDict only supports == and != comparison",
            )),
        }
    }
}

impl PyHDict {
    pub fn from_core(d: &data::HDict) -> Self {
        Self { inner: d.clone() }
    }

    pub fn to_core(&self) -> data::HDict {
        self.inner.clone()
    }
}

// ── Dict key iterator ──

#[pyclass]
pub struct PyDictKeyIter {
    keys: Vec<String>,
    index: usize,
}

#[pymethods]
impl PyDictKeyIter {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__(&mut self) -> Option<String> {
        if self.index < self.keys.len() {
            let key = self.keys[self.index].clone();
            self.index += 1;
            Some(key)
        } else {
            None
        }
    }
}

// ── HCol ──

/// Haystack Column definition with name and optional metadata dict.
#[pyclass(name = "HCol", frozen, from_py_object)]
#[derive(Clone)]
pub struct PyHCol {
    #[pyo3(get)]
    pub name: String,
    pub(crate) meta: data::HDict,
}

#[pymethods]
impl PyHCol {
    /// Create a column with a name and optional metadata dict.
    #[new]
    #[pyo3(signature = (name, meta = None))]
    fn new(name: String, meta: Option<&PyHDict>) -> Self {
        Self {
            name,
            meta: meta.map(|m| m.inner.clone()).unwrap_or_default(),
        }
    }

    /// Column metadata as an HDict.
    #[getter]
    fn meta(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        Ok(PyHDict::from_core(&self.meta)
            .into_pyobject(py)?
            .into_any()
            .unbind())
    }

    fn __repr__(&self) -> String {
        format!("HCol('{}')", self.name)
    }

    fn __str__(&self) -> String {
        self.name.clone()
    }
}

impl PyHCol {
    pub fn from_core(c: &data::HCol) -> Self {
        Self {
            name: c.name.clone(),
            meta: c.meta.clone(),
        }
    }
}

// ── HGrid ──

/// Haystack Grid — a two-dimensional table of tagged data.
///
/// Grids have metadata, named columns, and rows of HDict values.
/// This is the primary data exchange format in the Haystack protocol.
///
/// Examples:
///     g = HGrid()
///     g = HGrid.from_parts(meta, cols, rows)
///     g.add_row(HDict({"dis": "Room 101"}))
#[pyclass(name = "HGrid")]
pub struct PyHGrid {
    pub(crate) inner: data::HGrid,
}

#[pymethods]
impl PyHGrid {
    /// Create an empty grid with no columns or rows.
    #[new]
    fn new() -> Self {
        Self {
            inner: data::HGrid::new(),
        }
    }

    /// Construct a grid from meta, columns, and rows.
    #[staticmethod]
    fn from_parts(meta: &PyHDict, cols: Vec<PyHCol>, rows: Vec<PyRef<'_, PyHDict>>) -> Self {
        let core_cols: Vec<data::HCol> = cols
            .into_iter()
            .map(|c| data::HCol::with_meta(c.name.clone(), c.meta.clone()))
            .collect();
        let core_rows: Vec<data::HDict> = rows.iter().map(|r| r.inner.clone()).collect();
        Self {
            inner: data::HGrid::from_parts(meta.inner.clone(), core_cols, core_rows),
        }
    }

    /// Add a row (HDict) to the grid.
    fn add_row(&mut self, row: &PyHDict) {
        self.inner.rows.push(row.inner.clone());
    }

    /// Set the grid metadata.
    fn set_meta(&mut self, meta: &PyHDict) {
        self.inner.meta = meta.inner.clone();
    }

    /// Add a column to the grid.
    #[pyo3(signature = (name, meta = None))]
    fn add_col(&mut self, name: String, meta: Option<&PyHDict>) {
        let col = match meta {
            Some(m) => data::HCol::with_meta(name, m.inner.clone()),
            None => data::HCol::new(name),
        };
        self.inner.cols.push(col);
    }

    /// Return all rows as a list of HDict.
    fn rows(&self, py: Python<'_>) -> PyResult<Vec<Py<PyAny>>> {
        self.inner
            .rows
            .iter()
            .map(|r| Ok(PyHDict::from_core(r).into_pyobject(py)?.into_any().unbind()))
            .collect()
    }

    /// Return all columns as a list of HCol.
    fn cols(&self) -> Vec<PyHCol> {
        self.inner.cols.iter().map(PyHCol::from_core).collect()
    }

    /// Return True if the grid has no rows.
    fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Return True if this is an error grid.
    fn is_err(&self) -> bool {
        self.inner.is_err()
    }

    /// Look up a column by name. Returns HCol or None.
    #[pyo3(signature = (name,))]
    fn col(&self, name: &str) -> Option<PyHCol> {
        self.inner.col(name).map(PyHCol::from_core)
    }

    /// Return column names as a list of strings.
    fn col_names(&self) -> Vec<String> {
        self.inner.col_names().map(|s| s.to_string()).collect()
    }

    /// Return the number of columns.
    fn num_cols(&self) -> usize {
        self.inner.num_cols()
    }

    /// Return the grid metadata as an HDict.
    fn meta(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        Ok(PyHDict::from_core(&self.inner.meta)
            .into_pyobject(py)?
            .into_any()
            .unbind())
    }

    fn __len__(&self) -> usize {
        self.inner.len()
    }

    fn __getitem__(&self, py: Python<'_>, index: isize) -> PyResult<Py<PyAny>> {
        let len = self.inner.len() as isize;
        let idx = if index < 0 { len + index } else { index };
        if idx < 0 || idx >= len {
            return Err(PyErr::new::<pyo3::exceptions::PyIndexError, _>(
                "grid row index out of range",
            ));
        }
        match self.inner.row(idx as usize) {
            Some(row) => Ok(PyHDict::from_core(row)
                .into_pyobject(py)?
                .into_any()
                .unbind()),
            None => Err(PyErr::new::<pyo3::exceptions::PyIndexError, _>(
                "grid row index out of range",
            )),
        }
    }

    fn __iter__(&self) -> PyGridRowIter {
        let rows: Vec<data::HDict> = self.inner.rows.clone();
        PyGridRowIter { rows, index: 0 }
    }

    fn __repr__(&self) -> String {
        self.inner.to_string()
    }

    fn __str__(&self) -> String {
        self.inner.to_string()
    }

    fn __richcmp__(&self, other: &Self, op: CompareOp) -> PyResult<bool> {
        match op {
            CompareOp::Eq => Ok(self.inner == other.inner),
            CompareOp::Ne => Ok(self.inner != other.inner),
            _ => Err(PyErr::new::<pyo3::exceptions::PyTypeError, _>(
                "HGrid only supports == and != comparison",
            )),
        }
    }
}

impl PyHGrid {
    pub fn from_core(g: &data::HGrid) -> Self {
        Self { inner: g.clone() }
    }

    pub fn to_core(&self) -> data::HGrid {
        self.inner.clone()
    }
}

// ── Grid row iterator ──

#[pyclass]
pub struct PyGridRowIter {
    rows: Vec<data::HDict>,
    index: usize,
}

#[pymethods]
impl PyGridRowIter {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__(&mut self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        if self.index < self.rows.len() {
            let row = &self.rows[self.index];
            self.index += 1;
            Ok(Some(
                PyHDict::from_core(row)
                    .into_pyobject(py)?
                    .into_any()
                    .unbind(),
            ))
        } else {
            Ok(None)
        }
    }
}

// ── HList ──

/// Haystack List — an ordered collection of Haystack values.
///
/// Supports Python sequence protocol (__len__, __getitem__, __setitem__, __delitem__).
#[pyclass(name = "HList")]
pub struct PyHList {
    pub(crate) inner: data::HList,
}

#[pymethods]
impl PyHList {
    /// Create a new list, optionally populated from Python values.
    #[new]
    #[pyo3(signature = (items = None))]
    fn new(items: Option<Vec<Bound<'_, pyo3::PyAny>>>) -> PyResult<Self> {
        let mut inner = data::HList::new();
        if let Some(items) = items {
            for item in items {
                let kind = py_to_kind(&item)?;
                inner.push(kind);
            }
        }
        Ok(Self { inner })
    }

    /// Append a single value to the list.
    fn push(&mut self, val: &Bound<'_, pyo3::PyAny>) -> PyResult<()> {
        let kind = py_to_kind(val)?;
        self.inner.push(kind);
        Ok(())
    }

    /// Extend this list with items from another HList.
    fn extend(&mut self, other: &PyHList) {
        self.inner.0.extend(other.inner.0.iter().cloned());
    }

    /// Remove all items from the list.
    fn clear(&mut self) {
        self.inner.0.clear();
    }

    /// Set the value at the given index.
    fn __setitem__(&mut self, index: isize, val: &Bound<'_, pyo3::PyAny>) -> PyResult<()> {
        let len = self.inner.len() as isize;
        let idx = if index < 0 { len + index } else { index };
        if idx < 0 || idx >= len {
            return Err(PyErr::new::<pyo3::exceptions::PyIndexError, _>(
                "list index out of range",
            ));
        }
        let kind = py_to_kind(val)?;
        self.inner.0[idx as usize] = kind;
        Ok(())
    }

    /// Delete the value at the given index.
    fn __delitem__(&mut self, index: isize) -> PyResult<()> {
        let len = self.inner.len() as isize;
        let idx = if index < 0 { len + index } else { index };
        if idx < 0 || idx >= len {
            return Err(PyErr::new::<pyo3::exceptions::PyIndexError, _>(
                "list index out of range",
            ));
        }
        self.inner.0.remove(idx as usize);
        Ok(())
    }

    /// Number of items in the list.
    #[getter]
    fn len(&self) -> usize {
        self.inner.len()
    }

    /// Return True if the list is empty.
    fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    fn __len__(&self) -> usize {
        self.inner.len()
    }

    fn __getitem__(&self, py: Python<'_>, index: isize) -> PyResult<Py<PyAny>> {
        let len = self.inner.len() as isize;
        let idx = if index < 0 { len + index } else { index };
        if idx < 0 || idx >= len {
            return Err(PyErr::new::<pyo3::exceptions::PyIndexError, _>(
                "list index out of range",
            ));
        }
        match self.inner.get(idx as usize) {
            Some(kind) => kind_to_py(py, kind),
            None => Err(PyErr::new::<pyo3::exceptions::PyIndexError, _>(
                "list index out of range",
            )),
        }
    }

    fn __iter__(&self) -> PyListIter {
        let items: Vec<Kind> = self.inner.0.clone();
        PyListIter { items, index: 0 }
    }

    fn __repr__(&self) -> String {
        self.inner.to_string()
    }

    fn __str__(&self) -> String {
        self.inner.to_string()
    }

    fn __richcmp__(&self, other: &Self, op: CompareOp) -> PyResult<bool> {
        match op {
            CompareOp::Eq => Ok(self.inner == other.inner),
            CompareOp::Ne => Ok(self.inner != other.inner),
            _ => Err(PyErr::new::<pyo3::exceptions::PyTypeError, _>(
                "HList only supports == and != comparison",
            )),
        }
    }
}

impl PyHList {
    pub fn from_core(l: &data::HList) -> Self {
        Self { inner: l.clone() }
    }

    pub fn to_core(&self) -> data::HList {
        self.inner.clone()
    }
}

// ── List iterator ──

#[pyclass]
pub struct PyListIter {
    items: Vec<Kind>,
    index: usize,
}

#[pymethods]
impl PyListIter {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__(&mut self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        if self.index < self.items.len() {
            let item = &self.items[self.index];
            self.index += 1;
            Ok(Some(kind_to_py(py, item)?))
        } else {
            Ok(None)
        }
    }
}

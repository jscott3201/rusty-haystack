// Python wrappers for Haystack data structures: HDict, HGrid, HList, HCol.

use pyo3::prelude::*;
use pyo3::class::basic::CompareOp;
use pyo3::types::PyDict;

use haystack_core::data;
use haystack_core::kinds::Kind;

use crate::convert::{kind_to_py, py_to_kind};
use crate::kinds::PyRef_;

// ── HDict ──

#[pyclass(name = "HDict")]
pub struct PyHDict {
    pub(crate) inner: data::HDict,
}

#[pymethods]
impl PyHDict {
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

    fn has(&self, name: &str) -> bool {
        self.inner.has(name)
    }

    #[pyo3(signature = (name, default = None))]
    fn get(&self, py: Python<'_>, name: &str, default: Option<PyObject>) -> PyResult<PyObject> {
        match self.inner.get(name) {
            Some(kind) => kind_to_py(py, kind),
            None => Ok(default.unwrap_or_else(|| py.None())),
        }
    }

    fn missing(&self, name: &str) -> bool {
        self.inner.missing(name)
    }

    fn id(&self, py: Python<'_>) -> PyResult<Option<PyObject>> {
        match self.inner.id() {
            Some(r) => {
                let py_ref = PyRef_::from_core(r);
                Ok(Some(py_ref.into_pyobject(py)?.into_any().unbind()))
            }
            None => Ok(None),
        }
    }

    fn dis(&self) -> Option<&str> {
        self.inner.dis()
    }

    fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    fn set(&mut self, py: Python<'_>, name: String, val: &Bound<'_, PyAny>) -> PyResult<()> {
        let _ = py;
        let kind = py_to_kind(val)?;
        self.inner.set(name, kind);
        Ok(())
    }

    fn merge(&mut self, other: &PyHDict) {
        self.inner.merge(&other.inner);
    }

    fn keys(&self) -> Vec<String> {
        self.inner.tag_names().map(|s| s.to_string()).collect()
    }

    fn values(&self, py: Python<'_>) -> PyResult<Vec<PyObject>> {
        self.inner
            .iter()
            .map(|(_, v)| kind_to_py(py, v))
            .collect()
    }

    fn items(&self, py: Python<'_>) -> PyResult<Vec<(String, PyObject)>> {
        self.inner
            .iter()
            .map(|(k, v)| Ok((k.to_string(), kind_to_py(py, v)?)))
            .collect()
    }

    fn __getitem__(&self, py: Python<'_>, key: &str) -> PyResult<PyObject> {
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
        Self {
            inner: d.clone(),
        }
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

#[pyclass(name = "HCol", frozen)]
#[derive(Clone)]
pub struct PyHCol {
    #[pyo3(get)]
    pub name: String,
}

#[pymethods]
impl PyHCol {
    #[new]
    fn new(name: String) -> Self {
        Self { name }
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
        }
    }
}

// ── HGrid ──

#[pyclass(name = "HGrid")]
pub struct PyHGrid {
    pub(crate) inner: data::HGrid,
}

#[pymethods]
impl PyHGrid {
    #[new]
    fn new() -> Self {
        Self {
            inner: data::HGrid::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    fn is_err(&self) -> bool {
        self.inner.is_err()
    }

    #[pyo3(signature = (name,))]
    fn col(&self, name: &str) -> Option<PyHCol> {
        self.inner.col(name).map(PyHCol::from_core)
    }

    fn col_names(&self) -> Vec<String> {
        self.inner.col_names().map(|s| s.to_string()).collect()
    }

    fn num_cols(&self) -> usize {
        self.inner.num_cols()
    }

    fn meta(&self, py: Python<'_>) -> PyResult<PyObject> {
        Ok(PyHDict::from_core(&self.inner.meta)
            .into_pyobject(py)?
            .into_any()
            .unbind())
    }

    fn __len__(&self) -> usize {
        self.inner.len()
    }

    fn __getitem__(&self, py: Python<'_>, index: isize) -> PyResult<PyObject> {
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
        Self {
            inner: g.clone(),
        }
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

    fn __next__(&mut self, py: Python<'_>) -> PyResult<Option<PyObject>> {
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

#[pyclass(name = "HList")]
pub struct PyHList {
    pub(crate) inner: data::HList,
}

#[pymethods]
impl PyHList {
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

    fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    fn __len__(&self) -> usize {
        self.inner.len()
    }

    fn __getitem__(&self, py: Python<'_>, index: isize) -> PyResult<PyObject> {
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
        Self {
            inner: l.clone(),
        }
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

    fn __next__(&mut self, py: Python<'_>) -> PyResult<Option<PyObject>> {
        if self.index < self.items.len() {
            let item = &self.items[self.index];
            self.index += 1;
            Ok(Some(kind_to_py(py, item)?))
        } else {
            Ok(None)
        }
    }
}

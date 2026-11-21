//! Explicit Python wrappers for the shared typed value variants.

use std::hash::{Hash, Hasher};

use haystack_core::kinds::{Float, NominalScalar};
use pyo3::class::basic::CompareOp;
use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyBytes, PyInt};

fn identity_hash(value: &impl Hash) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn equality(equal: bool, op: CompareOp) -> PyResult<bool> {
    match op {
        CompareOp::Eq => Ok(equal),
        CompareOp::Ne => Ok(!equal),
        _ => Err(PyTypeError::new_err("typed values only support == and !=")),
    }
}

#[pyclass(name = "Int", frozen, from_py_object)]
#[derive(Clone)]
pub struct PyIntValue {
    #[pyo3(get)]
    pub value: i64,
}

#[pymethods]
impl PyIntValue {
    #[new]
    fn new(value: &Bound<'_, PyAny>) -> PyResult<Self> {
        if !value.is_instance_of::<PyInt>() || value.is_instance_of::<PyBool>() {
            return Err(PyTypeError::new_err("Int requires a Python int"));
        }
        Ok(Self {
            value: value.extract()?,
        })
    }
    fn __int__(&self) -> i64 {
        self.value
    }
    fn __repr__(&self) -> String {
        format!("Int({})", self.value)
    }
    fn __hash__(&self) -> u64 {
        identity_hash(&self.value)
    }
    fn __richcmp__(&self, other: &Self, op: CompareOp) -> PyResult<bool> {
        equality(self.value == other.value, op)
    }
}

#[pyclass(name = "Float", frozen, from_py_object)]
#[derive(Clone)]
pub struct PyFloatValue {
    pub(crate) inner: Float,
}

#[pymethods]
impl PyFloatValue {
    #[new]
    fn new(value: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            inner: Float::new(crate::convert::exact_number(value)?),
        })
    }
    #[staticmethod]
    fn from_bits(bits: &Bound<'_, PyAny>) -> PyResult<Self> {
        if !bits.is_instance_of::<PyInt>() || bits.is_instance_of::<PyBool>() {
            return Err(PyTypeError::new_err(
                "Float.from_bits requires a Python int",
            ));
        }
        Ok(Self {
            inner: Float::from_bits(bits.extract()?),
        })
    }
    #[getter]
    fn bits(&self) -> u64 {
        self.inner.bits()
    }
    #[getter]
    fn value(&self) -> f64 {
        self.inner.value()
    }
    fn __float__(&self) -> f64 {
        self.inner.value()
    }
    fn __repr__(&self) -> String {
        format!("Float.from_bits(0x{:016x})", self.inner.bits())
    }
    fn __hash__(&self) -> u64 {
        identity_hash(&self.inner)
    }
    fn __richcmp__(&self, other: &Self, op: CompareOp) -> PyResult<bool> {
        equality(self.inner == other.inner, op)
    }
}

#[pyclass(name = "NoneValue", frozen, from_py_object)]
#[derive(Clone)]
pub struct PyNoneValue;

#[pymethods]
impl PyNoneValue {
    #[new]
    pub fn new() -> Self {
        Self
    }
    fn __repr__(&self) -> &'static str {
        "NoneValue()"
    }
    fn __hash__(&self) -> u64 {
        3
    }
    fn __richcmp__(&self, _other: &Self, op: CompareOp) -> PyResult<bool> {
        equality(true, op)
    }
}

#[pyclass(name = "Buf", frozen, from_py_object)]
#[derive(Clone)]
pub struct PyBuf {
    pub(crate) inner: Vec<u8>,
}

#[pymethods]
impl PyBuf {
    #[new]
    fn new(data: &Bound<'_, PyBytes>) -> Self {
        Self {
            inner: data.as_bytes().to_vec(),
        }
    }
    #[getter]
    fn data<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &self.inner)
    }
    fn __bytes__<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        self.data(py)
    }
    fn __repr__(&self) -> String {
        format!("Buf({:?})", self.inner)
    }
    fn __hash__(&self) -> u64 {
        identity_hash(&self.inner)
    }
    fn __richcmp__(&self, other: &Self, op: CompareOp) -> PyResult<bool> {
        equality(self.inner == other.inner, op)
    }
}

#[pyclass(name = "Nominal", frozen, from_py_object)]
#[derive(Clone)]
pub struct PyNominal {
    pub(crate) inner: NominalScalar,
}

#[pymethods]
impl PyNominal {
    #[new]
    fn new(
        type_name: String,
        catalog_id: String,
        schema_id: String,
        value: String,
    ) -> PyResult<Self> {
        let inner = NominalScalar::new(type_name, catalog_id, schema_id, value)
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        Ok(Self { inner })
    }
    #[getter]
    fn type_name(&self) -> &str {
        self.inner.spec()
    }
    #[getter]
    fn catalog_id(&self) -> &str {
        self.inner.catalog()
    }
    #[getter]
    fn schema_id(&self) -> &str {
        self.inner.revision()
    }
    #[getter]
    fn value(&self) -> &str {
        self.inner.text()
    }
    fn __repr__(&self) -> String {
        format!(
            "Nominal({:?}, {:?}, {:?}, {:?})",
            self.type_name(),
            self.catalog_id(),
            self.schema_id(),
            self.value()
        )
    }
    fn __hash__(&self) -> u64 {
        identity_hash(&self.inner)
    }
    fn __richcmp__(&self, other: &Self, op: CompareOp) -> PyResult<bool> {
        equality(self.inner == other.inner, op)
    }
}

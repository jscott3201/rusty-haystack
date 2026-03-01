// Codec bindings — encode/decode grids and scalars via Haystack wire formats.

use pyo3::prelude::*;

use haystack_core::codecs;

use crate::convert::{kind_to_py, py_to_kind};
use crate::data::PyHGrid;

#[pyfunction]
pub fn encode_grid(codec_name: &str, grid: &PyHGrid) -> PyResult<String> {
    let codec = codecs::codec_for(codec_name).ok_or_else(|| {
        PyErr::new::<pyo3::exceptions::PyKeyError, _>(format!(
            "unknown codec: '{}' (try 'text/zinc', 'application/json', 'text/trio')",
            codec_name
        ))
    })?;
    codec
        .encode_grid(&grid.inner)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(e.to_string()))
}

#[pyfunction]
pub fn decode_grid(codec_name: &str, data: &str) -> PyResult<PyHGrid> {
    let codec = codecs::codec_for(codec_name).ok_or_else(|| {
        PyErr::new::<pyo3::exceptions::PyKeyError, _>(format!(
            "unknown codec: '{}' (try 'text/zinc', 'application/json', 'text/trio')",
            codec_name
        ))
    })?;
    let grid = codec
        .decode_grid(data)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(e.to_string()))?;
    Ok(PyHGrid::from_core(&grid))
}

#[pyfunction]
pub fn encode_scalar(codec_name: &str, val: &Bound<'_, PyAny>) -> PyResult<String> {
    let codec = codecs::codec_for(codec_name).ok_or_else(|| {
        PyErr::new::<pyo3::exceptions::PyKeyError, _>(format!(
            "unknown codec: '{}'",
            codec_name
        ))
    })?;
    let kind = py_to_kind(val)?;
    codec
        .encode_scalar(&kind)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(e.to_string()))
}

#[pyfunction]
pub fn decode_scalar(py: Python<'_>, codec_name: &str, data: &str) -> PyResult<PyObject> {
    let codec = codecs::codec_for(codec_name).ok_or_else(|| {
        PyErr::new::<pyo3::exceptions::PyKeyError, _>(format!(
            "unknown codec: '{}'",
            codec_name
        ))
    })?;
    let kind = codec
        .decode_scalar(data)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(e.to_string()))?;
    kind_to_py(py, &kind)
}

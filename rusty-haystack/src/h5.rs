//! Deliberate selection of the pinned, bounded H5/Jeto read profile.
use pyo3::{exceptions::PyValueError, prelude::*};

#[pyclass(name = "H5Config", module = "rusty_haystack.client", frozen)]
pub(crate) struct PyH5Config;

#[pymethods]
impl PyH5Config {
    #[new]
    #[pyo3(signature = (*, version="5", format="jeto", boxing="all"))]
    fn new(version: &str, format: &str, boxing: &str) -> PyResult<Self> {
        if version != "5" || format != "jeto" || boxing != "all" {
            return Err(PyValueError::new_err(
                "H5Config supports only version='5', format='jeto', boxing='all'",
            ));
        }
        Ok(Self)
    }
    #[getter]
    fn version(&self) -> &'static str {
        "5"
    }
    #[getter]
    fn format(&self) -> &'static str {
        "jeto"
    }
    #[getter]
    fn boxing(&self) -> &'static str {
        "all"
    }
    fn __repr__(&self) -> &'static str {
        "H5Config(version='5', format='jeto', boxing='all')"
    }
}

pub(crate) fn error(error: haystack_client::h5::ApiError) -> PyErr {
    crate::async_bridge::with_python(|py| {
        let exception = crate::exceptions::H5Error::new_err(error.to_string());
        let value = exception.value(py);
        value.setattr("status", error.status)?;
        value.setattr("spec", error.spec)?;
        value.setattr(
            "payload",
            Py::new(py, crate::data::PyHDict::from_core(&error.payload)?)?,
        )?;
        Ok(exception)
    })
    .unwrap_or_else(|error| error)
}

// Ontology bindings — DefNamespace with taxonomy, fits, and validation.
// Also exposes Xeto Spec/Slot types and library management.

use pyo3::prelude::*;

use haystack_core::ontology::DefNamespace;

use crate::data::PyHDict;

// ---------------------------------------------------------------------------
// Xeto Slot / Spec wrappers
// ---------------------------------------------------------------------------

/// A resolved Xeto slot exposed to Python.
#[pyclass(name = "Slot")]
pub struct PySlot {
    inner: haystack_core::xeto::Slot,
}

#[pymethods]
impl PySlot {
    #[getter]
    fn name(&self) -> &str {
        &self.inner.name
    }

    #[getter]
    fn type_ref(&self) -> Option<&str> {
        self.inner.type_ref.as_deref()
    }

    #[getter]
    fn is_marker(&self) -> bool {
        self.inner.is_marker
    }

    #[getter]
    fn is_query(&self) -> bool {
        self.inner.is_query
    }

    #[getter]
    fn is_maybe(&self) -> bool {
        self.inner.is_maybe()
    }

    fn __repr__(&self) -> String {
        if self.inner.is_marker {
            format!("Slot(name='{}', marker)", self.inner.name)
        } else {
            format!(
                "Slot(name='{}', type='{}')",
                self.inner.name,
                self.inner.type_ref.as_deref().unwrap_or("?")
            )
        }
    }
}

/// A resolved Xeto spec exposed to Python.
#[pyclass(name = "Spec")]
pub struct PySpec {
    inner: haystack_core::xeto::Spec,
}

#[pymethods]
impl PySpec {
    #[getter]
    fn qname(&self) -> &str {
        &self.inner.qname
    }

    #[getter]
    fn name(&self) -> &str {
        &self.inner.name
    }

    #[getter]
    fn lib(&self) -> &str {
        &self.inner.lib
    }

    #[getter]
    fn base(&self) -> Option<&str> {
        self.inner.base.as_deref()
    }

    #[getter]
    fn doc(&self) -> &str {
        &self.inner.doc
    }

    #[getter]
    fn is_abstract(&self) -> bool {
        self.inner.is_abstract
    }

    #[getter]
    fn slots(&self) -> Vec<PySlot> {
        self.inner
            .slots
            .iter()
            .map(|s| PySlot { inner: s.clone() })
            .collect()
    }

    /// Marker slot names.
    fn markers(&self) -> Vec<String> {
        self.inner
            .markers()
            .into_iter()
            .map(|s| s.to_string())
            .collect()
    }

    /// Mandatory (non-maybe) marker slot names.
    fn mandatory_markers(&self) -> Vec<String> {
        self.inner
            .mandatory_markers()
            .into_iter()
            .map(|s| s.to_string())
            .collect()
    }

    fn __repr__(&self) -> String {
        format!(
            "Spec(qname='{}', slots={})",
            self.inner.qname,
            self.inner.slots.len()
        )
    }
}

// ---------------------------------------------------------------------------
// DefNamespace
// ---------------------------------------------------------------------------

#[pyclass(name = "DefNamespace")]
pub struct PyDefNamespace {
    inner: DefNamespace,
}

#[pymethods]
impl PyDefNamespace {
    #[new]
    fn new() -> Self {
        Self {
            inner: DefNamespace::new(),
        }
    }

    /// Load the bundled standard Haystack 4 defs (ph, phScience, phIoT, phIct).
    #[staticmethod]
    fn load_standard() -> PyResult<Self> {
        let inner = DefNamespace::load_standard()
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(e.to_string()))?;
        Ok(Self { inner })
    }

    /// Check nominal subtype relationship: is `name` a subtype of `supertype`?
    fn is_a(&self, name: &str, supertype: &str) -> bool {
        self.inner.is_a(name, supertype)
    }

    /// Check if an entity structurally fits a type (has all mandatory markers).
    fn fits(&self, entity: &PyHDict, type_name: &str) -> bool {
        self.inner.fits(&entity.inner, type_name)
    }

    /// Validate a single entity and return a list of issue description strings.
    fn validate_entity(&self, entity: &PyHDict) -> Vec<String> {
        self.inner
            .validate_entity(&entity.inner)
            .iter()
            .map(|issue| issue.to_string())
            .collect()
    }

    /// Direct subtypes of a type.
    fn subtypes(&self, name: &str) -> Vec<String> {
        self.inner.subtypes(name)
    }

    /// Full supertype chain (transitive, breadth-first).
    fn supertypes(&self, name: &str) -> Vec<String> {
        self.inner.supertypes(name)
    }

    /// Check if a def name is registered.
    fn contains(&self, name: &str) -> bool {
        self.inner.contains(name)
    }

    /// Number of registered defs.
    fn __len__(&self) -> usize {
        self.inner.len()
    }

    fn __repr__(&self) -> String {
        format!("DefNamespace(defs={})", self.inner.len())
    }

    // -- Xeto library management ------------------------------------------

    /// Load a Xeto library from source text. Returns list of spec qnames.
    fn load_xeto(&mut self, source: &str, lib_name: &str) -> PyResult<Vec<String>> {
        self.inner
            .load_xeto_str(source, lib_name)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(e.to_string()))
    }

    /// Unload a library by name.
    fn unload_lib(&mut self, name: &str) -> PyResult<()> {
        self.inner
            .unload_lib(name)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(e))
    }

    /// Get a spec by qualified name (e.g. "ph::Ahu").
    fn get_spec(&self, qname: &str) -> Option<PySpec> {
        self.inner
            .get_spec(qname)
            .map(|s| PySpec { inner: s.clone() })
    }

    /// List all specs, optionally filtered by library name.
    fn specs(&self, lib: Option<&str>) -> Vec<PySpec> {
        self.inner
            .specs(lib)
            .into_iter()
            .map(|s| PySpec { inner: s.clone() })
            .collect()
    }

    /// Export a library to Xeto source text.
    fn export_lib_xeto(&self, name: &str) -> PyResult<String> {
        self.inner
            .export_lib_xeto(name)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(e))
    }
}

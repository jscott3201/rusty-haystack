// Ontology bindings — DefNamespace with taxonomy, fits, and validation.
// Also exposes Xeto Spec/Slot types, Def/Lib types, and library management.

use pyo3::prelude::*;

use haystack_core::ontology::{Def, DefKind, DefNamespace, Lib};

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
// DefKind enumeration
// ---------------------------------------------------------------------------

/// Classification of a Haystack definition (Marker, Val, Entity, etc.).
#[pyclass(name = "DefKind", frozen, eq, from_py_object)]
#[derive(Clone, PartialEq)]
pub enum PyDefKind {
    Marker,
    Val,
    Entity,
    Feature,
    Conjunct,
    Choice,
    Lib,
}

#[pymethods]
impl PyDefKind {
    fn __repr__(&self) -> &str {
        match self {
            PyDefKind::Marker => "DefKind.Marker",
            PyDefKind::Val => "DefKind.Val",
            PyDefKind::Entity => "DefKind.Entity",
            PyDefKind::Feature => "DefKind.Feature",
            PyDefKind::Conjunct => "DefKind.Conjunct",
            PyDefKind::Choice => "DefKind.Choice",
            PyDefKind::Lib => "DefKind.Lib",
        }
    }
}

impl PyDefKind {
    fn from_core(k: &DefKind) -> Self {
        match k {
            DefKind::Marker => PyDefKind::Marker,
            DefKind::Val => PyDefKind::Val,
            DefKind::Entity => PyDefKind::Entity,
            DefKind::Feature => PyDefKind::Feature,
            DefKind::Conjunct => PyDefKind::Conjunct,
            DefKind::Choice => PyDefKind::Choice,
            DefKind::Lib => PyDefKind::Lib,
        }
    }
}

// ---------------------------------------------------------------------------
// Def (read-only definition record)
// ---------------------------------------------------------------------------

/// A Haystack definition record. Read-only view of a def's symbol, supertype chain, tags, and doc.
#[pyclass(name = "Def", frozen)]
pub struct PyDef {
    inner: Def,
}

#[pymethods]
impl PyDef {
    #[getter]
    fn symbol(&self) -> &str {
        &self.inner.symbol
    }

    #[getter]
    fn lib(&self) -> &str {
        &self.inner.lib
    }

    /// Supertype symbols from the `is` tag.
    #[getter]
    fn is_(&self) -> Vec<String> {
        self.inner.is_.clone()
    }

    /// Entity types this tag applies to (`tagOn`).
    #[getter]
    fn tag_on(&self) -> Vec<String> {
        self.inner.tag_on.clone()
    }

    /// Target type for refs/choices (`of` tag).
    #[getter]
    fn of(&self) -> Option<&str> {
        self.inner.of.as_deref()
    }

    #[getter]
    fn mandatory(&self) -> bool {
        self.inner.mandatory
    }

    #[getter]
    fn doc(&self) -> &str {
        &self.inner.doc
    }

    /// Full meta tags as HDict.
    #[getter]
    fn tags(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        Ok(PyHDict::from_core(&self.inner.tags)
            .into_pyobject(py)?
            .into_any()
            .unbind())
    }

    /// Derived DefKind (Marker, Val, Entity, etc.).
    #[getter]
    fn kind(&self) -> PyDefKind {
        PyDefKind::from_core(&self.inner.kind())
    }

    fn __repr__(&self) -> String {
        format!(
            "Def(symbol='{}', lib='{}', kind={})",
            self.inner.symbol,
            self.inner.lib,
            self.kind().__repr__()
        )
    }
}

// ---------------------------------------------------------------------------
// Lib (read-only library record)
// ---------------------------------------------------------------------------

/// A Haystack library containing definitions. Read-only view with name, version, and defs.
#[pyclass(name = "Lib", frozen)]
pub struct PyLib {
    inner: Lib,
}

#[pymethods]
impl PyLib {
    #[getter]
    fn name(&self) -> &str {
        &self.inner.name
    }

    #[getter]
    fn version(&self) -> &str {
        &self.inner.version
    }

    #[getter]
    fn doc(&self) -> &str {
        &self.inner.doc
    }

    #[getter]
    fn depends(&self) -> Vec<String> {
        self.inner.depends.clone()
    }

    /// All defs in this library, as a list of Def objects.
    fn defs(&self) -> Vec<PyDef> {
        self.inner
            .defs
            .values()
            .map(|d| PyDef { inner: d.clone() })
            .collect()
    }

    /// Get a specific def from this library by symbol.
    fn get_def(&self, symbol: &str) -> Option<PyDef> {
        self.inner
            .defs
            .get(symbol)
            .map(|d| PyDef { inner: d.clone() })
    }

    fn __len__(&self) -> usize {
        self.inner.defs.len()
    }

    fn __repr__(&self) -> String {
        format!(
            "Lib(name='{}', defs={})",
            self.inner.name,
            self.inner.defs.len()
        )
    }
}

// ---------------------------------------------------------------------------
// DefNamespace
// ---------------------------------------------------------------------------

/// Haystack definition namespace — container for defs and libs with taxonomy resolution.
///
/// Provides fits checking, mandatory tag computation, and taxonomy traversal
/// for the Haystack 4 ontology.
///
/// Examples:
///     ns = DefNamespace()
///     ns.load_lib("phIoT")
///     ns.fits("ahu", "equip")  # True
#[pyclass(name = "DefNamespace")]
pub struct PyDefNamespace {
    pub(crate) inner: DefNamespace,
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

    /// Get a def by symbol name.
    fn get_def(&self, symbol: &str) -> Option<PyDef> {
        self.inner
            .get_def(symbol)
            .map(|d| PyDef { inner: d.clone() })
    }

    /// Tags that are mandatory for a given entity type.
    fn mandatory_tags(&self, name: &str) -> Vec<String> {
        self.inner.mandatory_tags(name).into_iter().collect()
    }

    /// All tags (mandatory + optional) for a given entity type.
    fn tags_for(&self, name: &str) -> Vec<String> {
        self.inner.tags_for(name).into_iter().collect()
    }

    /// Get valid choices for a choice type.
    fn choices(&self, choice_name: &str) -> Vec<String> {
        self.inner.choices(choice_name)
    }

    /// Explain why an entity does or doesn't fit a type. Returns list of issue strings.
    fn fits_explain(&self, entity: &PyHDict, type_name: &str) -> Vec<String> {
        self.inner
            .fits_explain(&entity.inner, type_name)
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
            .map_err(PyErr::new::<pyo3::exceptions::PyValueError, _>)
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
            .map_err(PyErr::new::<pyo3::exceptions::PyValueError, _>)
    }

    /// All registered defs as a list.
    fn defs(&self) -> Vec<PyDef> {
        self.inner
            .defs()
            .values()
            .map(|d| PyDef { inner: d.clone() })
            .collect()
    }

    /// All loaded libraries as a list.
    fn libs(&self) -> Vec<PyLib> {
        self.inner
            .libs()
            .values()
            .map(|l| PyLib { inner: l.clone() })
            .collect()
    }

    /// Get a library by name.
    fn get_lib(&self, name: &str) -> Option<PyLib> {
        self.inner
            .libs()
            .get(name)
            .map(|l| PyLib { inner: l.clone() })
    }
}

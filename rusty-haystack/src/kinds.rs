// Python Kind type wrappers for Haystack scalar types.

use pyo3::class::basic::CompareOp;
use pyo3::prelude::*;
use pyo3::types::{PyDateTime, PyInt, PyTzInfo};
use std::hash::{Hash, Hasher};

use haystack_core::kinds;

// ── Marker ──

/// Haystack Marker tag value. Represents presence of a tag with no value.
#[pyclass(name = "Marker", frozen, from_py_object)]
#[derive(Clone)]
pub struct PyMarker;

#[pymethods]
impl PyMarker {
    /// Create a Marker value.
    #[new]
    pub fn new() -> Self {
        PyMarker
    }

    fn __repr__(&self) -> &str {
        "Marker()"
    }

    fn __str__(&self) -> &str {
        "\u{2713}"
    }

    fn __hash__(&self) -> u64 {
        0
    }

    fn __richcmp__(&self, other: &Bound<'_, PyAny>, op: CompareOp) -> PyResult<bool> {
        if other.extract::<PyMarker>().is_ok() {
            match op {
                CompareOp::Eq => Ok(true),
                CompareOp::Ne => Ok(false),
                _ => Ok(false),
            }
        } else {
            match op {
                CompareOp::Eq => Ok(false),
                CompareOp::Ne => Ok(true),
                _ => Ok(false),
            }
        }
    }

    fn __bool__(&self) -> bool {
        true
    }
}

// ── NA ──

/// Haystack NA (not available) value. Indicates missing or unavailable data.
#[pyclass(name = "NA", frozen, from_py_object)]
#[derive(Clone)]
pub struct PyNA;

#[pymethods]
impl PyNA {
    /// Create an NA (not available) value.
    #[new]
    pub fn new() -> Self {
        PyNA
    }

    fn __repr__(&self) -> &str {
        "NA()"
    }

    fn __str__(&self) -> &str {
        "NA"
    }

    fn __hash__(&self) -> u64 {
        1
    }

    fn __richcmp__(&self, other: &Bound<'_, PyAny>, op: CompareOp) -> PyResult<bool> {
        if other.extract::<PyNA>().is_ok() {
            match op {
                CompareOp::Eq => Ok(true),
                CompareOp::Ne => Ok(false),
                _ => Ok(false),
            }
        } else {
            match op {
                CompareOp::Eq => Ok(false),
                CompareOp::Ne => Ok(true),
                _ => Ok(false),
            }
        }
    }

    fn __bool__(&self) -> bool {
        false
    }
}

// ── Remove ──

/// Haystack Remove marker. Used in diffs to indicate tag removal.
#[pyclass(name = "Remove", frozen, from_py_object)]
#[derive(Clone)]
pub struct PyRemove;

#[pymethods]
impl PyRemove {
    /// Create a Remove marker for tag removal in diffs.
    #[new]
    pub fn new() -> Self {
        PyRemove
    }

    fn __repr__(&self) -> &str {
        "Remove()"
    }

    fn __str__(&self) -> &str {
        "remove"
    }

    fn __hash__(&self) -> u64 {
        2
    }

    fn __richcmp__(&self, other: &Bound<'_, PyAny>, op: CompareOp) -> PyResult<bool> {
        if other.extract::<PyRemove>().is_ok() {
            match op {
                CompareOp::Eq => Ok(true),
                CompareOp::Ne => Ok(false),
                _ => Ok(false),
            }
        } else {
            match op {
                CompareOp::Eq => Ok(false),
                CompareOp::Ne => Ok(true),
                _ => Ok(false),
            }
        }
    }

    fn __bool__(&self) -> bool {
        false
    }
}

// ── Number ──

/// Haystack Number with optional unit. Wraps a float value and unit string.
///
/// Examples:
///     Number(72, "°F")
///     Number(100)
#[pyclass(name = "Number", frozen, from_py_object)]
#[derive(Clone)]
pub struct PyNumber {
    #[pyo3(get)]
    pub val: f64,
    #[pyo3(get)]
    pub unit: Option<String>,
}

#[pymethods]
impl PyNumber {
    /// Create a Number with a float value and optional unit string.
    #[new]
    #[pyo3(signature = (val, unit = None))]
    pub fn new(val: &Bound<'_, PyAny>, unit: Option<String>) -> PyResult<Self> {
        Ok(Self {
            val: crate::convert::exact_number(val)?,
            unit,
        })
    }

    fn __repr__(&self) -> String {
        match &self.unit {
            Some(u) => format!("Number({}, '{}')", self.val, u),
            None => format!("Number({})", self.val),
        }
    }

    fn __str__(&self) -> String {
        let core = self.to_core();
        core.to_string()
    }

    fn __hash__(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.to_core().hash(&mut hasher);
        hasher.finish()
    }

    fn __richcmp__(&self, other: &Self, op: CompareOp) -> PyResult<bool> {
        match op {
            CompareOp::Eq => Ok(self.to_core() == other.to_core()),
            CompareOp::Ne => Ok(self.to_core() != other.to_core()),
            CompareOp::Lt => {
                if self.unit != other.unit {
                    return Ok(false);
                }
                Ok(self.val < other.val)
            }
            CompareOp::Le => {
                if self.unit != other.unit {
                    return Ok(false);
                }
                Ok(self.val <= other.val)
            }
            CompareOp::Gt => {
                if self.unit != other.unit {
                    return Ok(false);
                }
                Ok(self.val > other.val)
            }
            CompareOp::Ge => {
                if self.unit != other.unit {
                    return Ok(false);
                }
                Ok(self.val >= other.val)
            }
        }
    }

    fn __float__(&self) -> f64 {
        self.val
    }

    fn __int__(&self) -> PyResult<i64> {
        if !self.val.is_finite() || self.val >= -(i64::MIN as f64) || self.val < i64::MIN as f64 {
            return Err(pyo3::exceptions::PyOverflowError::new_err(
                "Number value out of range for integer conversion",
            ));
        }
        Ok(self.val as i64)
    }
}

impl PyNumber {
    pub fn from_core(n: &kinds::Number) -> Self {
        Self {
            val: n.val,
            unit: n.unit.clone(),
        }
    }

    pub fn to_core(&self) -> kinds::Number {
        kinds::Number::new(self.val, self.unit.clone())
    }
}

// ── Ref ──

/// Haystack Ref identifier. References entities by string ID with optional display name.
///
/// Examples:
///     Ref("site-1")
///     Ref("site-1", "Main Campus")
#[pyclass(name = "Ref", frozen, from_py_object)]
#[derive(Clone)]
pub struct PyRef_ {
    #[pyo3(get)]
    pub val: String,
    #[pyo3(get)]
    pub dis: Option<String>,
}

#[pymethods]
impl PyRef_ {
    /// Create a Ref with an ID string and optional display name.
    #[new]
    #[pyo3(signature = (val, dis = None))]
    pub fn new(val: String, dis: Option<String>) -> Self {
        Self { val, dis }
    }

    fn __repr__(&self) -> String {
        match &self.dis {
            Some(d) => format!("Ref('{}', '{}')", self.val, d),
            None => format!("Ref('{}')", self.val),
        }
    }

    fn __str__(&self) -> String {
        let core = self.to_core();
        core.to_string()
    }

    fn __hash__(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.val.hash(&mut hasher);
        hasher.finish()
    }

    fn __richcmp__(&self, other: &Self, op: CompareOp) -> PyResult<bool> {
        // Equality is by val only (matching Rust HRef)
        match op {
            CompareOp::Eq => Ok(self.val == other.val),
            CompareOp::Ne => Ok(self.val != other.val),
            _ => Ok(false),
        }
    }
}

impl PyRef_ {
    pub fn from_core(r: &kinds::HRef) -> Self {
        Self {
            val: r.val.clone(),
            dis: r.dis.clone(),
        }
    }

    pub fn to_core(&self) -> kinds::HRef {
        kinds::HRef::new(self.val.clone(), self.dis.clone())
    }
}

// ── Uri ──

/// Haystack URI value. Wraps a string URI/URL.
#[pyclass(name = "Uri", frozen, from_py_object)]
#[derive(Clone)]
pub struct PyUri {
    #[pyo3(get)]
    pub val: String,
}

#[pymethods]
impl PyUri {
    /// Create a URI from a string value.
    #[new]
    pub fn new(val: String) -> Self {
        Self { val }
    }

    fn __repr__(&self) -> String {
        format!("Uri('{}')", self.val)
    }

    fn __str__(&self) -> String {
        self.val.clone()
    }

    fn __hash__(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.val.hash(&mut hasher);
        hasher.finish()
    }

    fn __richcmp__(&self, other: &Self, op: CompareOp) -> PyResult<bool> {
        match op {
            CompareOp::Eq => Ok(self.val == other.val),
            CompareOp::Ne => Ok(self.val != other.val),
            _ => Ok(false),
        }
    }
}

impl PyUri {
    pub fn from_core(u: &kinds::Uri) -> Self {
        Self {
            val: u.val().to_string(),
        }
    }

    pub fn to_core(&self) -> kinds::Uri {
        kinds::Uri::new(self.val.clone())
    }
}

// ── Symbol ──

/// Haystack Symbol value. Represents a def symbol reference (e.g., ^hot-water).
#[pyclass(name = "Symbol", frozen, from_py_object)]
#[derive(Clone)]
pub struct PySymbol {
    #[pyo3(get)]
    pub val: String,
}

#[pymethods]
impl PySymbol {
    /// Create a Symbol from a def name string (without ^ prefix).
    #[new]
    pub fn new(val: String) -> Self {
        Self { val }
    }

    fn __repr__(&self) -> String {
        format!("Symbol('{}')", self.val)
    }

    fn __str__(&self) -> String {
        format!("^{}", self.val)
    }

    fn __hash__(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.val.hash(&mut hasher);
        hasher.finish()
    }

    fn __richcmp__(&self, other: &Self, op: CompareOp) -> PyResult<bool> {
        match op {
            CompareOp::Eq => Ok(self.val == other.val),
            CompareOp::Ne => Ok(self.val != other.val),
            _ => Ok(false),
        }
    }
}

impl PySymbol {
    pub fn from_core(s: &kinds::Symbol) -> Self {
        Self {
            val: s.val().to_string(),
        }
    }

    pub fn to_core(&self) -> kinds::Symbol {
        kinds::Symbol::new(self.val.clone())
    }
}

// ── XStr ──

/// Haystack XStr (extended string). Type-qualified string value like Bin("text/plain").
#[pyclass(name = "XStr", frozen, from_py_object)]
#[derive(Clone)]
pub struct PyXStr {
    #[pyo3(get)]
    pub type_name: String,
    #[pyo3(get)]
    pub val: String,
}

#[pymethods]
impl PyXStr {
    /// Create an XStr with a type qualifier and string value.
    #[new]
    pub fn new(type_name: String, val: String) -> Self {
        Self { type_name, val }
    }

    fn __repr__(&self) -> String {
        format!("XStr('{}', '{}')", self.type_name, self.val)
    }

    fn __str__(&self) -> String {
        format!("{}(\"{}\")", self.type_name, self.val)
    }

    fn __hash__(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.type_name.hash(&mut hasher);
        self.val.hash(&mut hasher);
        hasher.finish()
    }

    fn __richcmp__(&self, other: &Self, op: CompareOp) -> PyResult<bool> {
        match op {
            CompareOp::Eq => Ok(self.type_name == other.type_name && self.val == other.val),
            CompareOp::Ne => Ok(self.type_name != other.type_name || self.val != other.val),
            _ => Ok(false),
        }
    }
}

impl PyXStr {
    pub fn from_core(x: &kinds::XStr) -> Self {
        Self {
            type_name: x.type_name.clone(),
            val: x.val.clone(),
        }
    }

    pub fn to_core(&self) -> kinds::XStr {
        kinds::XStr::new(self.type_name.clone(), self.val.clone())
    }
}

// ── Coord ──

/// Haystack geographic Coordinate with latitude and longitude.
#[pyclass(name = "Coord", frozen, from_py_object)]
#[derive(Clone)]
pub struct PyCoord {
    #[pyo3(get)]
    pub lat: f64,
    #[pyo3(get)]
    pub lng: f64,
}

#[pymethods]
impl PyCoord {
    /// Create a geographic Coordinate from latitude and longitude in degrees.
    #[new]
    pub fn new(lat: &Bound<'_, PyAny>, lng: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            lat: crate::convert::exact_number(lat)?,
            lng: crate::convert::exact_number(lng)?,
        })
    }

    fn __repr__(&self) -> String {
        format!("Coord({}, {})", self.lat, self.lng)
    }

    fn __str__(&self) -> String {
        format!("C({},{})", self.lat, self.lng)
    }

    fn __hash__(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.to_core().hash(&mut hasher);
        hasher.finish()
    }

    fn __richcmp__(&self, other: &Self, op: CompareOp) -> PyResult<bool> {
        match op {
            CompareOp::Eq => Ok(self.to_core() == other.to_core()),
            CompareOp::Ne => Ok(self.to_core() != other.to_core()),
            _ => Ok(false),
        }
    }
}

impl PyCoord {
    pub fn from_core(c: &kinds::Coord) -> Self {
        Self {
            lat: c.lat,
            lng: c.lng,
        }
    }

    pub fn to_core(&self) -> kinds::Coord {
        kinds::Coord::new(self.lat, self.lng)
    }
}

// ── HDateTime ──

/// Haystack DateTime with timezone. Wraps an ISO 8601 datetime and timezone name.
#[pyclass(name = "HDateTime", frozen, from_py_object)]
#[derive(Clone)]
pub struct PyHDateTime {
    inner: kinds::HDateTime,
}

#[pymethods]
impl PyHDateTime {
    /// Create a Haystack DateTime from components.
    ///
    /// Args:
    ///     year, month, day, hour, minute, second: Calendar/time components.
    ///     offset_seconds: UTC offset in seconds (e.g., -18000 for EST).
    ///     tz_name: Haystack timezone name (e.g., 'New_York').
    #[new]
    #[pyo3(signature = (year, month, day, hour, minute, second, offset_seconds, tz_name))]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        year: &Bound<'_, PyAny>,
        month: &Bound<'_, PyAny>,
        day: &Bound<'_, PyAny>,
        hour: &Bound<'_, PyAny>,
        minute: &Bound<'_, PyAny>,
        second: &Bound<'_, PyAny>,
        offset_seconds: &Bound<'_, PyAny>,
        tz_name: String,
    ) -> PyResult<Self> {
        use chrono::{FixedOffset, TimeZone};
        // Preserve primitive range errors and bool-as-int, without admitting
        // arbitrary __index__ implementations for calendar components.
        macro_rules! integer {
            ($value:ident, $ty:ty) => {{
                if !$value.is_instance_of::<PyInt>() {
                    return Err(pyo3::exceptions::PyTypeError::new_err(concat!(
                        stringify!($value),
                        " must be a Python int"
                    )));
                }
                $value.extract::<$ty>()?
            }};
        }
        let year = integer!(year, i32);
        let month = integer!(month, u32);
        let day = integer!(day, u32);
        let hour = integer!(hour, u32);
        let minute = integer!(minute, u32);
        let second = integer!(second, u32);
        let offset_seconds = integer!(offset_seconds, i32);
        let offset = FixedOffset::east_opt(offset_seconds).ok_or_else(|| {
            PyErr::new::<pyo3::exceptions::PyValueError, _>("invalid timezone offset")
        })?;
        let dt = offset
            .with_ymd_and_hms(year, month, day, hour, minute, second)
            .single()
            .ok_or_else(|| PyErr::new::<pyo3::exceptions::PyValueError, _>("invalid datetime"))?;
        Ok(Self {
            inner: kinds::HDateTime::new(dt, tz_name),
        })
    }

    /// Return a Python datetime.datetime object.
    fn dt<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDateTime>> {
        let dt = &self.inner.dt;
        let local = dt
            .naive_utc()
            .checked_add_offset(*dt.offset())
            .ok_or_else(|| {
                pyo3::exceptions::PyValueError::new_err("local datetime is outside calendar range")
            })?;
        crate::convert::ensure_date(&local.date())?;
        crate::convert::ensure_time(&local.time())?;
        let offset_secs = dt.offset().local_minus_utc();
        // Use datetime module to create timezone
        let datetime_mod = py.import("datetime")?;
        let timedelta = datetime_mod.getattr("timedelta")?;
        let timezone = datetime_mod.getattr("timezone")?;
        let td = timedelta.call1((0, offset_secs))?;
        let tz_obj: Bound<'py, PyTzInfo> = timezone.call1((td,))?.cast_into()?;

        PyDateTime::new(
            py,
            chrono::Datelike::year(&local),
            chrono::Datelike::month(&local) as u8,
            chrono::Datelike::day(&local) as u8,
            chrono::Timelike::hour(&local) as u8,
            chrono::Timelike::minute(&local) as u8,
            chrono::Timelike::second(&local) as u8,
            chrono::Timelike::nanosecond(&local) / 1000,
            Some(&tz_obj),
        )
    }

    /// The Haystack timezone name.
    #[getter]
    fn tz_name(&self) -> &str {
        &self.inner.tz_name
    }

    fn __repr__(&self) -> String {
        format!("HDateTime({})", self.inner)
    }

    fn __str__(&self) -> String {
        self.inner.to_string()
    }

    fn __hash__(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.inner.hash(&mut hasher);
        hasher.finish()
    }

    fn __richcmp__(&self, other: &Self, op: CompareOp) -> PyResult<bool> {
        match op {
            CompareOp::Eq => Ok(self.inner == other.inner),
            CompareOp::Ne => Ok(self.inner != other.inner),
            CompareOp::Lt => Ok(self.inner.dt < other.inner.dt),
            CompareOp::Le => Ok(self.inner.dt <= other.inner.dt),
            CompareOp::Gt => Ok(self.inner.dt > other.inner.dt),
            CompareOp::Ge => Ok(self.inner.dt >= other.inner.dt),
        }
    }
}

impl PyHDateTime {
    pub fn from_core(dt: &kinds::HDateTime) -> Self {
        Self { inner: dt.clone() }
    }

    pub fn to_core(&self) -> kinds::HDateTime {
        self.inner.clone()
    }
}

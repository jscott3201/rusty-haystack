// Python Kind type wrappers for Haystack scalar types.

use pyo3::class::basic::CompareOp;
use pyo3::prelude::*;
use pyo3::types::{PyDateTime, PyTzInfo};
use std::hash::{Hash, Hasher};

use haystack_core::kinds;

// ── Marker ──

#[pyclass(name = "Marker", frozen)]
#[derive(Clone)]
pub struct PyMarker;

#[pymethods]
impl PyMarker {
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

#[pyclass(name = "NA", frozen)]
#[derive(Clone)]
pub struct PyNA;

#[pymethods]
impl PyNA {
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

#[pyclass(name = "Remove", frozen)]
#[derive(Clone)]
pub struct PyRemove;

#[pymethods]
impl PyRemove {
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

#[pyclass(name = "Number", frozen)]
#[derive(Clone)]
pub struct PyNumber {
    #[pyo3(get)]
    pub val: f64,
    #[pyo3(get)]
    pub unit: Option<String>,
}

#[pymethods]
impl PyNumber {
    #[new]
    #[pyo3(signature = (val, unit = None))]
    pub fn new(val: f64, unit: Option<String>) -> Self {
        Self { val, unit }
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
        self.val.to_bits().hash(&mut hasher);
        self.unit.hash(&mut hasher);
        hasher.finish()
    }

    fn __richcmp__(&self, other: &Self, op: CompareOp) -> PyResult<bool> {
        match op {
            CompareOp::Eq => Ok(self.val == other.val && self.unit == other.unit),
            CompareOp::Ne => Ok(self.val != other.val || self.unit != other.unit),
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
        if !self.val.is_finite() || self.val > i64::MAX as f64 || self.val < i64::MIN as f64 {
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

#[pyclass(name = "Ref", frozen)]
#[derive(Clone)]
pub struct PyRef_ {
    #[pyo3(get)]
    pub val: String,
    #[pyo3(get)]
    pub dis: Option<String>,
}

#[pymethods]
impl PyRef_ {
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

#[pyclass(name = "Uri", frozen)]
#[derive(Clone)]
pub struct PyUri {
    #[pyo3(get)]
    pub val: String,
}

#[pymethods]
impl PyUri {
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

#[pyclass(name = "Symbol", frozen)]
#[derive(Clone)]
pub struct PySymbol {
    #[pyo3(get)]
    pub val: String,
}

#[pymethods]
impl PySymbol {
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

#[pyclass(name = "XStr", frozen)]
#[derive(Clone)]
pub struct PyXStr {
    #[pyo3(get)]
    pub type_name: String,
    #[pyo3(get)]
    pub val: String,
}

#[pymethods]
impl PyXStr {
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

#[pyclass(name = "Coord", frozen)]
#[derive(Clone)]
pub struct PyCoord {
    #[pyo3(get)]
    pub lat: f64,
    #[pyo3(get)]
    pub lng: f64,
}

#[pymethods]
impl PyCoord {
    #[new]
    pub fn new(lat: f64, lng: f64) -> Self {
        Self { lat, lng }
    }

    fn __repr__(&self) -> String {
        format!("Coord({}, {})", self.lat, self.lng)
    }

    fn __str__(&self) -> String {
        format!("C({},{})", self.lat, self.lng)
    }

    fn __hash__(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.lat.to_bits().hash(&mut hasher);
        self.lng.to_bits().hash(&mut hasher);
        hasher.finish()
    }

    fn __richcmp__(&self, other: &Self, op: CompareOp) -> PyResult<bool> {
        match op {
            CompareOp::Eq => Ok(self.lat == other.lat && self.lng == other.lng),
            CompareOp::Ne => Ok(self.lat != other.lat || self.lng != other.lng),
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

#[pyclass(name = "HDateTime", frozen)]
#[derive(Clone)]
pub struct PyHDateTime {
    inner: kinds::HDateTime,
}

#[pymethods]
impl PyHDateTime {
    #[new]
    #[pyo3(signature = (year, month, day, hour, minute, second, offset_seconds, tz_name))]
    pub fn new(
        year: i32,
        month: u32,
        day: u32,
        hour: u32,
        minute: u32,
        second: u32,
        offset_seconds: i32,
        tz_name: String,
    ) -> PyResult<Self> {
        use chrono::{FixedOffset, TimeZone};
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
        let offset_secs = dt.offset().local_minus_utc();
        // Use datetime module to create timezone
        let datetime_mod = py.import("datetime")?;
        let timedelta = datetime_mod.getattr("timedelta")?;
        let timezone = datetime_mod.getattr("timezone")?;
        let td = timedelta.call1((0, offset_secs))?;
        let tz_obj: Bound<'py, PyTzInfo> = timezone.call1((td,))?.downcast_into()?;

        PyDateTime::new(
            py,
            chrono::Datelike::year(dt),
            chrono::Datelike::month(dt) as u8,
            chrono::Datelike::day(dt) as u8,
            chrono::Timelike::hour(dt) as u8,
            chrono::Timelike::minute(dt) as u8,
            chrono::Timelike::second(dt) as u8,
            (chrono::Timelike::nanosecond(dt) / 1000) as u32,
            Some(&tz_obj),
        )
    }

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

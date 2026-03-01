// Central conversion layer between Rust Kind and Python objects.

use pyo3::prelude::*;
use pyo3::types::*;

use haystack_core::kinds::Kind;

use crate::data::{PyHDict, PyHGrid, PyHList};
use crate::kinds::{
    PyCoord, PyHDateTime, PyMarker, PyNA, PyNumber, PyRef_, PyRemove, PySymbol, PyUri, PyXStr,
};

/// Convert a Rust Kind to a Python object.
pub fn kind_to_py(py: Python<'_>, kind: &Kind) -> PyResult<PyObject> {
    match kind {
        Kind::Null => Ok(py.None()),
        Kind::Marker => Ok(PyMarker::new().into_pyobject(py)?.into_any().unbind()),
        Kind::NA => Ok(PyNA::new().into_pyobject(py)?.into_any().unbind()),
        Kind::Remove => Ok(PyRemove::new().into_pyobject(py)?.into_any().unbind()),
        Kind::Bool(b) => {
            let py_bool = b.into_pyobject(py)?;
            Ok(py_bool.to_owned().into_any().unbind())
        }
        Kind::Number(n) => Ok(PyNumber::from_core(n)
            .into_pyobject(py)?
            .into_any()
            .unbind()),
        Kind::Str(s) => Ok(s.into_pyobject(py)?.into_any().unbind()),
        Kind::Ref(r) => Ok(PyRef_::from_core(r).into_pyobject(py)?.into_any().unbind()),
        Kind::Date(d) => {
            let py_date = PyDate::new(
                py,
                chrono::Datelike::year(d),
                chrono::Datelike::month(d) as u8,
                chrono::Datelike::day(d) as u8,
            )?;
            Ok(py_date.into_any().unbind())
        }
        Kind::Time(t) => {
            let py_time = PyTime::new(
                py,
                chrono::Timelike::hour(t) as u8,
                chrono::Timelike::minute(t) as u8,
                chrono::Timelike::second(t) as u8,
                chrono::Timelike::nanosecond(t) / 1000, // microseconds
                None,
            )?;
            Ok(py_time.into_any().unbind())
        }
        Kind::DateTime(dt) => Ok(PyHDateTime::from_core(dt)
            .into_pyobject(py)?
            .into_any()
            .unbind()),
        Kind::Uri(u) => Ok(PyUri::from_core(u).into_pyobject(py)?.into_any().unbind()),
        Kind::Symbol(s) => Ok(PySymbol::from_core(s)
            .into_pyobject(py)?
            .into_any()
            .unbind()),
        Kind::Coord(c) => Ok(PyCoord::from_core(c).into_pyobject(py)?.into_any().unbind()),
        Kind::XStr(x) => Ok(PyXStr::from_core(x).into_pyobject(py)?.into_any().unbind()),
        Kind::List(items) => {
            let hlist = haystack_core::data::HList::from_vec(items.clone());
            Ok(PyHList::from_core(&hlist)
                .into_pyobject(py)?
                .into_any()
                .unbind())
        }
        Kind::Dict(d) => Ok(PyHDict::from_core(d).into_pyobject(py)?.into_any().unbind()),
        Kind::Grid(g) => Ok(PyHGrid::from_core(g).into_pyobject(py)?.into_any().unbind()),
    }
}

/// Convert a Python object to a Rust Kind.
pub fn py_to_kind(obj: &Bound<'_, PyAny>) -> PyResult<Kind> {
    // None -> Null
    if obj.is_none() {
        return Ok(Kind::Null);
    }

    // Bool must be checked before int, since Python bool is a subclass of int
    if obj.is_instance_of::<pyo3::types::PyBool>() {
        let b: bool = obj.extract()?;
        return Ok(Kind::Bool(b));
    }

    // PyMarker
    if let Ok(_m) = obj.extract::<PyMarker>() {
        return Ok(Kind::Marker);
    }

    // PyNA
    if let Ok(_n) = obj.extract::<PyNA>() {
        return Ok(Kind::NA);
    }

    // PyRemove
    if let Ok(_r) = obj.extract::<PyRemove>() {
        return Ok(Kind::Remove);
    }

    // PyNumber
    if let Ok(n) = obj.extract::<PyNumber>() {
        return Ok(Kind::Number(n.to_core()));
    }

    // str -> Kind::Str
    if let Ok(s) = obj.extract::<String>() {
        return Ok(Kind::Str(s));
    }

    // PyRef_
    if let Ok(r) = obj.extract::<PyRef_>() {
        return Ok(Kind::Ref(r.to_core()));
    }

    // PyUri
    if let Ok(u) = obj.extract::<PyUri>() {
        return Ok(Kind::Uri(u.to_core()));
    }

    // PySymbol
    if let Ok(s) = obj.extract::<PySymbol>() {
        return Ok(Kind::Symbol(s.to_core()));
    }

    // PyCoord
    if let Ok(c) = obj.extract::<PyCoord>() {
        return Ok(Kind::Coord(c.to_core()));
    }

    // PyXStr
    if let Ok(x) = obj.extract::<PyXStr>() {
        return Ok(Kind::XStr(x.to_core()));
    }

    // PyHDateTime
    if let Ok(dt) = obj.extract::<PyHDateTime>() {
        return Ok(Kind::DateTime(dt.to_core()));
    }

    // Python datetime.date (not datetime.datetime) -> Kind::Date
    // Check datetime first since datetime is a subclass of date
    if obj.is_instance_of::<PyDateTime>() {
        // If this is a raw Python datetime without our wrapper, we cannot
        // convert it without tz_name. Return an error.
        return Err(PyErr::new::<pyo3::exceptions::PyTypeError, _>(
            "Use HDateTime for datetime values, not Python datetime directly",
        ));
    }
    if obj.is_instance_of::<PyDate>() {
        let year: i32 = obj.getattr("year")?.extract()?;
        let month: u32 = obj.getattr("month")?.extract()?;
        let day: u32 = obj.getattr("day")?.extract()?;
        let date = chrono::NaiveDate::from_ymd_opt(year, month, day)
            .ok_or_else(|| PyErr::new::<pyo3::exceptions::PyValueError, _>("invalid date"))?;
        return Ok(Kind::Date(date));
    }

    // Python datetime.time -> Kind::Time
    if obj.is_instance_of::<PyTime>() {
        let hour: u32 = obj.getattr("hour")?.extract()?;
        let minute: u32 = obj.getattr("minute")?.extract()?;
        let second: u32 = obj.getattr("second")?.extract()?;
        let micro: u32 = obj.getattr("microsecond")?.extract()?;
        let time = chrono::NaiveTime::from_hms_micro_opt(hour, minute, second, micro)
            .ok_or_else(|| PyErr::new::<pyo3::exceptions::PyValueError, _>("invalid time"))?;
        return Ok(Kind::Time(time));
    }

    // PyHDict
    if let Ok(d) = obj.extract::<PyRef<'_, PyHDict>>() {
        return Ok(Kind::Dict(Box::new(d.to_core())));
    }

    // PyHGrid
    if let Ok(g) = obj.extract::<PyRef<'_, PyHGrid>>() {
        return Ok(Kind::Grid(Box::new(g.to_core())));
    }

    // PyHList
    if let Ok(l) = obj.extract::<PyRef<'_, PyHList>>() {
        return Ok(Kind::List(l.to_core().0));
    }

    // int or float -> Number (fallback for plain Python numerics)
    if let Ok(v) = obj.extract::<f64>() {
        return Ok(Kind::Number(haystack_core::kinds::Number::unitless(v)));
    }

    Err(PyErr::new::<pyo3::exceptions::PyTypeError, _>(format!(
        "Cannot convert Python type '{}' to Haystack Kind",
        obj.get_type().name()?
    )))
}

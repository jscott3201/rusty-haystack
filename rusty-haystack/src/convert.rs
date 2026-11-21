// Central conversion layer between Rust Kind and Python objects.

use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::*;

use haystack_core::data::{HDict, HGrid};
use haystack_core::kinds::Kind;

use crate::data::{PyHDict, PyHGrid, PyHList};
use crate::kinds::{
    PyCoord, PyHDateTime, PyMarker, PyNA, PyNumber, PyRef_, PyRemove, PySymbol, PyUri, PyXStr,
};
use crate::rich_kinds::{PyBuf, PyFloatValue, PyIntValue, PyNominal, PyNoneValue};

/// Native numerics retain the H4 Number mapping without implicit rounding or
/// calls to user-defined numeric protocols. Integer overflow remains OverflowError.
pub(crate) fn exact_number(obj: &Bound<'_, PyAny>) -> PyResult<f64> {
    if obj.is_instance_of::<PyBool>() {
        return Err(PyTypeError::new_err("expected int or float, not bool"));
    }
    if obj.is_instance_of::<PyInt>() {
        let value: i64 = obj.extract()?;
        let float = value as f64;
        if float as i128 != i128::from(value) {
            return Err(PyValueError::new_err(
                "integer is not exactly representable as binary64; use Int",
            ));
        }
        return Ok(float);
    }
    if obj.is_instance_of::<PyFloat>() {
        return obj.extract();
    }
    Err(PyTypeError::new_err("expected a Python int or float"))
}

pub(crate) fn ensure_date(date: &chrono::NaiveDate) -> PyResult<()> {
    if !(1..=9999).contains(&chrono::Datelike::year(date)) {
        return Err(PyValueError::new_err(
            "date year is outside Python's 1..9999 range",
        ));
    }
    Ok(())
}

pub(crate) fn ensure_time(time: &chrono::NaiveTime) -> PyResult<()> {
    let nanos = chrono::Timelike::nanosecond(time);
    if nanos >= 1_000_000_000 {
        return Err(PyValueError::new_err(
            "Python time cannot represent a leap second",
        ));
    }
    if !nanos.is_multiple_of(1000) {
        return Err(PyValueError::new_err(
            "Python time requires exact microsecond precision",
        ));
    }
    Ok(())
}

/// Pure Rust preflight: safe to run before graph removal while holding its lock.
/// HDateTime remains an exact wrapper; its native datetime projection is separate.
pub(crate) fn ensure_python(value: &Kind) -> PyResult<()> {
    visit(value, 0)
}

pub(crate) fn ensure_python_dict(dict: &HDict) -> PyResult<()> {
    visit_dict(dict, 0)
}

pub(crate) fn ensure_python_grid(grid: &HGrid) -> PyResult<()> {
    visit_grid(grid, 0)
}

pub(crate) fn ensure_python_child(value: &Kind) -> PyResult<()> {
    visit(value, 1)
}

pub(crate) fn ensure_python_grid_dict(dict: &HDict) -> PyResult<()> {
    visit_dict(dict, 1)
}

fn ensure_depth(depth: usize) -> PyResult<()> {
    if depth > 64 {
        return Err(PyValueError::new_err("Python value nesting exceeds 64"));
    }
    Ok(())
}

fn visit_dict(dict: &HDict, depth: usize) -> PyResult<()> {
    ensure_depth(depth)?;
    for (_, value) in dict.iter() {
        visit(value, depth + 1)?;
    }
    Ok(())
}

fn visit_grid(grid: &HGrid, depth: usize) -> PyResult<()> {
    ensure_depth(depth)?;
    visit_dict(&grid.meta, depth + 1)?;
    for col in &grid.cols {
        visit_dict(&col.meta, depth + 1)?;
    }
    for row in &grid.rows {
        visit_dict(row, depth + 1)?;
    }
    Ok(())
}

fn visit(value: &Kind, depth: usize) -> PyResult<()> {
    ensure_depth(depth)?;
    match value {
        Kind::Date(date) => ensure_date(date),
        Kind::Time(time) => ensure_time(time),
        Kind::List(items) => {
            for item in items {
                visit(item, depth + 1)?;
            }
            Ok(())
        }
        Kind::Dict(dict) => visit_dict(dict, depth),
        Kind::Grid(grid) => visit_grid(grid, depth),
        _ => Ok(()),
    }
}

/// Convert a Rust Kind to a Python object.
pub fn kind_to_py(py: Python<'_>, kind: &Kind) -> PyResult<Py<PyAny>> {
    ensure_python(kind)?;
    match kind {
        Kind::Int(value) => Ok(PyIntValue { value: *value }
            .into_pyobject(py)?
            .into_any()
            .unbind()),
        Kind::Float(inner) => Ok(PyFloatValue { inner: *inner }
            .into_pyobject(py)?
            .into_any()
            .unbind()),
        Kind::None => Ok(PyNoneValue::new().into_pyobject(py)?.into_any().unbind()),
        Kind::Buf(inner) => Ok(PyBuf {
            inner: inner.clone(),
        }
        .into_pyobject(py)?
        .into_any()
        .unbind()),
        Kind::Nominal(inner) => Ok(PyNominal {
            inner: inner.clone(),
        }
        .into_pyobject(py)?
        .into_any()
        .unbind()),
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
            Ok(PyHList::from_core(&hlist)?
                .into_pyobject(py)?
                .into_any()
                .unbind())
        }
        Kind::Dict(d) => Ok(PyHDict::from_core(d)?
            .into_pyobject(py)?
            .into_any()
            .unbind()),
        Kind::Grid(g) => Ok(PyHGrid::from_core(g)?
            .into_pyobject(py)?
            .into_any()
            .unbind()),
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

    // Type admission is separate from extraction: accepted-type errors must
    // propagate (notably UnicodeEncodeError and mutable wrapper borrow errors).
    macro_rules! scalar {
        ($class:ty, $value:ident, $kind:expr) => {
            if obj.is_instance_of::<$class>() {
                let $value = obj.extract::<$class>()?;
                return Ok($kind);
            }
        };
    }
    scalar!(PyMarker, _value, Kind::Marker);
    scalar!(PyNA, _value, Kind::NA);
    scalar!(PyRemove, _value, Kind::Remove);
    scalar!(PyNoneValue, _value, Kind::None);
    scalar!(PyIntValue, value, Kind::Int(value.value));
    scalar!(PyFloatValue, value, Kind::Float(value.inner));
    scalar!(PyBuf, value, Kind::Buf(value.inner));
    scalar!(PyNominal, value, Kind::Nominal(value.inner));
    scalar!(PyNumber, value, Kind::Number(value.to_core()));
    scalar!(PyRef_, value, Kind::Ref(value.to_core()));
    scalar!(PyUri, value, Kind::Uri(value.to_core()));
    scalar!(PySymbol, value, Kind::Symbol(value.to_core()));
    scalar!(PyCoord, value, Kind::Coord(value.to_core()));
    scalar!(PyXStr, value, Kind::XStr(value.to_core()));
    scalar!(PyHDateTime, value, Kind::DateTime(value.to_core()));
    if obj.is_instance_of::<PyString>() {
        return Ok(Kind::Str(obj.extract()?));
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
        // Access native storage, not overridable subclass properties or hooks.
        let value = obj.cast::<PyDate>()?;
        let date = chrono::NaiveDate::from_ymd_opt(
            value.get_year(),
            u32::from(value.get_month()),
            u32::from(value.get_day()),
        )
        .ok_or_else(|| PyValueError::new_err("invalid date"))?;
        ensure_date(&date)?;
        return Ok(Kind::Date(date));
    }

    // Python datetime.time -> Kind::Time
    if obj.is_instance_of::<PyTime>() {
        let value = obj.cast::<PyTime>()?;
        if value.get_tzinfo().is_some() || value.get_fold() {
            return Err(PyValueError::new_err(
                "Haystack Time requires tzinfo=None and fold=0",
            ));
        }
        let time = chrono::NaiveTime::from_hms_micro_opt(
            u32::from(value.get_hour()),
            u32::from(value.get_minute()),
            u32::from(value.get_second()),
            value.get_microsecond(),
        )
        .ok_or_else(|| PyValueError::new_err("invalid time"))?;
        ensure_time(&time)?;
        return Ok(Kind::Time(time));
    }

    if obj.is_instance_of::<PyHDict>() {
        let value = obj.extract::<PyRef<'_, PyHDict>>()?;
        ensure_python_dict(&value.inner)?;
        return Ok(Kind::Dict(Box::new(value.to_core())));
    }
    if obj.is_instance_of::<PyHGrid>() {
        let value = obj.extract::<PyRef<'_, PyHGrid>>()?;
        ensure_python_grid(&value.inner)?;
        return Ok(Kind::Grid(Box::new(value.to_core())));
    }
    if obj.is_instance_of::<PyHList>() {
        let value = obj.extract::<PyRef<'_, PyHList>>()?;
        for item in &value.inner.0 {
            visit(item, 1)?;
        }
        return Ok(Kind::List(value.to_core().0));
    }
    if obj.is_instance_of::<PyInt>() || obj.is_instance_of::<PyFloat>() {
        return Ok(Kind::Number(haystack_core::kinds::Number::unitless(
            exact_number(obj)?,
        )));
    }

    Err(PyErr::new::<pyo3::exceptions::PyTypeError, _>(format!(
        "Cannot convert Python type '{}' to Haystack Kind",
        obj.get_type().name()?
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::PyHCol;
    use haystack_core::data::{HCol, HDict, HGrid, HList};
    use haystack_core::kinds::{Float, NominalScalar};

    #[test]
    fn rich_values_roundtrip_all_python_container_boundaries() {
        Python::initialize();
        Python::attach(|py| {
            for value in [
                Kind::Int(i64::MAX),
                Kind::Float(Float::from_bits(0x7ff8000000000001)),
                Kind::None,
                Kind::Buf(vec![0, 255]),
                Kind::Nominal(NominalScalar::new("example::Serial", "A", "r1", "00042").unwrap()),
            ] {
                let output = kind_to_py(py, &value).unwrap();
                assert_eq!(py_to_kind(output.bind(py)).unwrap(), value);
                let mut dict = HDict::new();
                dict.set("nested", Kind::List(vec![value.clone()]));
                assert_eq!(PyHDict::from_core(&dict).unwrap().to_core(), dict);
                let list = HList::from_vec(vec![value]);
                assert_eq!(PyHList::from_core(&list).unwrap().to_core(), list);
                assert!(PyHCol::from_core(&HCol::with_meta("v", dict.clone())).is_ok());
                for grid in [
                    HGrid::from_parts(dict.clone(), vec![], vec![]),
                    HGrid::from_parts(
                        HDict::new(),
                        vec![HCol::with_meta("v", dict.clone())],
                        vec![],
                    ),
                    HGrid::from_parts(HDict::new(), vec![], vec![dict.clone()]),
                ] {
                    assert_eq!(PyHGrid::from_core(&grid).unwrap().to_core(), grid);
                    let kind = Kind::Grid(Box::new(grid));
                    let output = kind_to_py(py, &kind).unwrap();
                    assert_eq!(py_to_kind(output.bind(py)).unwrap(), kind);
                }
            }
        });
    }

    #[test]
    fn unrepresentable_values_fail_at_each_container_boundary() {
        Python::initialize();
        Python::attach(|py| {
            let mut deep = Kind::Null;
            for _ in 0..65 {
                deep = Kind::List(vec![deep]);
            }
            for value in [
                Kind::Time(chrono::NaiveTime::from_hms_nano_opt(0, 0, 0, 1).unwrap()),
                Kind::Time(chrono::NaiveTime::from_hms_nano_opt(0, 0, 59, 1_000_000_000).unwrap()),
                Kind::Date(chrono::NaiveDate::from_ymd_opt(0, 1, 1).unwrap()),
                deep,
            ] {
                let check = |error: PyErr| assert!(error.is_instance_of::<PyValueError>(py));
                check(kind_to_py(py, &value).err().unwrap());
                let mut dict = HDict::new();
                dict.set("nested", Kind::List(vec![value.clone()]));
                check(PyHDict::from_core(&dict).err().unwrap());
                check(
                    PyHList::from_core(&HList::from_vec(vec![value]))
                        .err()
                        .unwrap(),
                );
                check(
                    PyHCol::from_core(&HCol::with_meta("v", dict.clone()))
                        .err()
                        .unwrap(),
                );
                for grid in [
                    HGrid::from_parts(dict.clone(), vec![], vec![]),
                    HGrid::from_parts(
                        HDict::new(),
                        vec![HCol::with_meta("v", dict.clone())],
                        vec![],
                    ),
                    HGrid::from_parts(HDict::new(), vec![], vec![dict.clone()]),
                ] {
                    check(PyHGrid::from_core(&grid).err().unwrap());
                }
            }
        });
    }
}

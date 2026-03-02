// Unit conversion bindings — convert values, check compatibility, and look up quantities.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

use haystack_core::kinds;

/// Convert a numeric value between compatible units.
///
/// Args:
///     val: The numeric value to convert.
///     from_unit: Source unit name or symbol (e.g., "°F", "kW").
///     to_unit: Target unit name or symbol (e.g., "°C", "W").
///
/// Returns:
///     The converted value as a float.
///
/// Raises:
///     ValueError: If either unit is unknown or the units are incompatible.
#[pyfunction]
pub fn convert(val: f64, from_unit: &str, to_unit: &str) -> PyResult<f64> {
    kinds::convert(val, from_unit, to_unit)
        .map_err(|e: kinds::UnitError| PyValueError::new_err(e.to_string()))
}

/// Check if two units are compatible (measure the same quantity).
///
/// Args:
///     a: First unit name or symbol.
///     b: Second unit name or symbol.
///
/// Returns:
///     True if the units can be converted between each other.
#[pyfunction]
pub fn compatible(a: &str, b: &str) -> bool {
    kinds::compatible(a, b)
}

/// Get the quantity name for a unit (e.g., "°F" → "temperature").
///
/// Args:
///     unit: Unit name or symbol.
///
/// Returns:
///     The quantity string, or None if the unit is unknown.
#[pyfunction]
pub fn quantity(unit: &str) -> Option<String> {
    kinds::quantity(unit).map(|s: &str| s.to_string())
}

/// Get the base SI unit for a quantity (e.g., "temperature" → "K").
///
/// Args:
///     qty: Quantity name (e.g., "temperature", "power", "length").
///
/// Returns:
///     The base unit string, or None if the quantity is unknown.
#[pyfunction]
pub fn base_unit(qty: &str) -> Option<String> {
    kinds::base_unit(qty).map(|s: &str| s.to_string())
}

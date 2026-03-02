use crate::codecs::shared;
use crate::kinds::units::{UnitError, convert, unit_for};
use std::fmt;
use std::hash::{Hash, Hasher};

/// Haystack Number — a 64-bit float with optional unit string.
///
/// Equality requires both `val` and `unit` to match.
/// NaN != NaN (IEEE 754 semantics).
/// Display uses compact format: no trailing zeros, unit appended directly.
#[derive(Debug, Clone)]
pub struct Number {
    pub val: f64,
    pub unit: Option<String>,
}

impl Number {
    pub fn new(val: f64, unit: Option<String>) -> Self {
        Self { val, unit }
    }

    pub fn unitless(val: f64) -> Self {
        Self { val, unit: None }
    }

    /// Convert this number to a different unit.
    ///
    /// The target may be a unit name (`"celsius"`) or symbol (`"°C"`).
    /// Returns a new `Number` with the converted value and the target unit's
    /// canonical name.
    pub fn convert_to(&self, target_unit: &str) -> Result<Number, UnitError> {
        let from = self
            .unit
            .as_deref()
            .ok_or_else(|| UnitError::UnknownUnit("(none)".to_string()))?;
        let converted = convert(self.val, from, target_unit)?;
        let target_name = unit_for(target_unit)
            .map(|u| u.name.clone())
            .unwrap_or_else(|| target_unit.to_string());
        Ok(Number::new(converted, Some(target_name)))
    }
}

impl PartialEq for Number {
    fn eq(&self, other: &Self) -> bool {
        self.val == other.val && self.unit == other.unit
    }
}

impl Eq for Number {}

impl Hash for Number {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.val.to_bits().hash(state);
        self.unit.hash(state);
    }
}

impl fmt::Display for Number {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", shared::format_number_val(self.val))?;
        if let Some(ref u) = self.unit {
            write!(f, "{u}")?;
        }
        Ok(())
    }
}

impl PartialOrd for Number {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        if self.unit != other.unit {
            return None;
        }
        self.val.partial_cmp(&other.val)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_unitless() {
        let n = Number::unitless(72.5);
        assert_eq!(n.val, 72.5);
        assert_eq!(n.unit, None);
        assert_eq!(n.to_string(), "72.5");
    }

    #[test]
    fn number_with_unit() {
        let n = Number::new(72.5, Some("°F".into()));
        assert_eq!(n.to_string(), "72.5°F");
    }

    #[test]
    fn number_integer_display() {
        let n = Number::unitless(42.0);
        assert_eq!(n.to_string(), "42");
    }

    #[test]
    fn number_zero() {
        let n = Number::unitless(0.0);
        assert_eq!(n.to_string(), "0");
    }

    #[test]
    fn number_negative() {
        let n = Number::new(-23.45, Some("m²".into()));
        assert_eq!(n.to_string(), "-23.45m²");
    }

    #[test]
    fn number_scientific() {
        let n = Number::new(5.4e8, Some("kW".into()));
        // Rust's default Display for large floats
        let s = n.to_string();
        assert!(s.contains("kW"));
    }

    #[test]
    fn number_special_inf() {
        assert_eq!(Number::unitless(f64::INFINITY).to_string(), "INF");
    }

    #[test]
    fn number_special_neg_inf() {
        assert_eq!(Number::unitless(f64::NEG_INFINITY).to_string(), "-INF");
    }

    #[test]
    fn number_special_nan() {
        assert_eq!(Number::unitless(f64::NAN).to_string(), "NaN");
    }

    #[test]
    fn number_equality() {
        let a = Number::new(72.5, Some("°F".into()));
        let b = Number::new(72.5, Some("°F".into()));
        let c = Number::new(72.5, Some("°C".into()));
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn number_nan_inequality() {
        let a = Number::unitless(f64::NAN);
        let b = Number::unitless(f64::NAN);
        assert_ne!(a, b);
    }

    #[test]
    fn number_ordering_same_unit() {
        let a = Number::new(10.0, Some("°F".into()));
        let b = Number::new(20.0, Some("°F".into()));
        assert!(a < b);
    }

    #[test]
    fn number_ordering_different_unit() {
        let a = Number::new(10.0, Some("°F".into()));
        let b = Number::new(20.0, Some("°C".into()));
        assert_eq!(a.partial_cmp(&b), None);
    }

    #[test]
    fn number_hashable() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(Number::unitless(42.0));
        assert!(set.contains(&Number::unitless(42.0)));
    }

    // --- convert_to tests ---

    #[test]
    fn number_convert_to_celsius() {
        let n = Number::new(212.0, Some("fahrenheit".into()));
        let c = n.convert_to("celsius").unwrap();
        assert!((c.val - 100.0).abs() < 0.01);
        assert_eq!(c.unit.as_deref(), Some("celsius"));
    }

    #[test]
    fn number_convert_to_by_symbol() {
        let n = Number::new(0.0, Some("°C".into()));
        let f = n.convert_to("°F").unwrap();
        assert!((f.val - 32.0).abs() < 0.01);
        assert_eq!(f.unit.as_deref(), Some("fahrenheit"));
    }

    #[test]
    fn number_convert_to_unitless_error() {
        let n = Number::unitless(42.0);
        let err = n.convert_to("celsius").unwrap_err();
        assert!(matches!(err, crate::kinds::units::UnitError::UnknownUnit(ref s) if s == "(none)"));
    }

    #[test]
    fn number_convert_to_incompatible() {
        let n = Number::new(100.0, Some("celsius".into()));
        let err = n.convert_to("meter").unwrap_err();
        assert!(matches!(
            err,
            crate::kinds::units::UnitError::IncompatibleUnits(_, _)
        ));
    }
}

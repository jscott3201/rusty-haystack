use crate::codecs::shared;
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
}

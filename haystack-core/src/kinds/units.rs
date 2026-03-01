use std::collections::HashMap;
use std::sync::LazyLock;

/// A Haystack unit definition.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Unit {
    pub name: String,
    pub symbols: Vec<String>,
    pub quantity: String,
}

struct UnitsRegistry {
    by_name: HashMap<String, Unit>,
    by_symbol: HashMap<String, Unit>,
}

static UNITS: LazyLock<UnitsRegistry> = LazyLock::new(|| {
    let data = include_str!("../../data/units.txt");
    parse_units(data)
});

fn parse_units(data: &str) -> UnitsRegistry {
    let mut by_name = HashMap::new();
    let mut by_symbol = HashMap::new();
    let mut current_quantity = String::new();

    for line in data.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        if line.starts_with("-- ") && line.ends_with(" --") {
            current_quantity = line[3..line.len() - 3].to_string();
            continue;
        }
        let parts: Vec<&str> = line.split(',').collect();
        if parts.is_empty() {
            continue;
        }
        let name = parts[0].trim().to_string();
        let symbols: Vec<String> = parts[1..]
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let unit = Unit {
            name: name.clone(),
            symbols: symbols.clone(),
            quantity: current_quantity.clone(),
        };

        by_name.insert(name, unit.clone());
        for sym in &symbols {
            by_symbol.insert(sym.clone(), unit.clone());
        }
    }

    UnitsRegistry { by_name, by_symbol }
}

/// Look up a unit by name or symbol.
pub fn unit_for(s: &str) -> Option<&'static Unit> {
    UNITS.by_name.get(s).or_else(|| UNITS.by_symbol.get(s))
}

/// Get all units indexed by name.
pub fn units_by_name() -> &'static HashMap<String, Unit> {
    &UNITS.by_name
}

/// Get all units indexed by symbol.
pub fn units_by_symbol() -> &'static HashMap<String, Unit> {
    &UNITS.by_symbol
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_loaded() {
        let by_name = units_by_name();
        assert!(!by_name.is_empty(), "units should be loaded from units.txt");
        // Should have hundreds of units
        assert!(
            by_name.len() > 100,
            "expected 100+ units, got {}",
            by_name.len()
        );
    }

    #[test]
    fn unit_lookup_by_name() {
        let u = unit_for("fahrenheit");
        assert!(u.is_some(), "fahrenheit should exist");
        let u = u.unwrap();
        assert_eq!(u.name, "fahrenheit");
        assert!(u.symbols.contains(&"°F".to_string()));
    }

    #[test]
    fn unit_lookup_by_symbol() {
        let u = unit_for("°F");
        assert!(u.is_some(), "°F should resolve");
        assert_eq!(u.unwrap().name, "fahrenheit");
    }

    #[test]
    fn unit_lookup_celsius() {
        let u = unit_for("celsius");
        assert!(u.is_some());
        assert!(u.unwrap().symbols.contains(&"°C".to_string()));
    }

    #[test]
    fn unit_not_found() {
        assert!(unit_for("nonexistent_unit_xyz").is_none());
    }

    #[test]
    fn unit_has_quantity() {
        let u = unit_for("fahrenheit").unwrap();
        assert!(
            !u.quantity.is_empty(),
            "unit should have a quantity category"
        );
    }
}

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

// ---------------------------------------------------------------------------
// Unit conversion
// ---------------------------------------------------------------------------

/// A conversion factor for a unit.
///
/// Formula to convert to the SI base unit for the quantity:
/// `si_value = input * scale + offset`
#[derive(Debug, Clone, Copy)]
pub struct ConversionFactor {
    /// Multiply by this to get SI base unit value.
    pub scale: f64,
    /// Add this after multiplying to get SI base unit value (for affine transforms like °F→°C).
    pub offset: f64,
}

/// Error type for unit conversion failures.
#[derive(Debug, Clone)]
pub enum UnitError {
    UnknownUnit(String),
    IncompatibleUnits(String, String),
}

impl std::fmt::Display for UnitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UnitError::UnknownUnit(u) => write!(f, "unknown unit: {u}"),
            UnitError::IncompatibleUnits(a, b) => write!(f, "incompatible units: {a} and {b}"),
        }
    }
}

impl std::error::Error for UnitError {}

static CONVERSION_FACTORS: LazyLock<HashMap<&'static str, ConversionFactor>> =
    LazyLock::new(|| {
        let entries: &[(&str, f64, f64)] = &[
            // temperature (SI base: celsius)
            ("fahrenheit", 5.0 / 9.0, -32.0 * 5.0 / 9.0),
            ("celsius", 1.0, 0.0),
            ("kelvin", 1.0, -273.15),
            // length (SI base: meter)
            ("meter", 1.0, 0.0),
            ("kilometer", 1000.0, 0.0),
            ("centimeter", 0.01, 0.0),
            ("millimeter", 0.001, 0.0),
            ("foot", 0.3048, 0.0),
            ("inch", 0.0254, 0.0),
            ("mile", 1609.344, 0.0),
            ("yard", 0.9144, 0.0),
            // pressure (SI base: pascal)
            ("pascal", 1.0, 0.0),
            ("kilopascal", 1000.0, 0.0),
            ("bar", 100_000.0, 0.0),
            ("millibar", 100.0, 0.0),
            ("pounds_per_square_inch", 6894.757, 0.0),
            ("inches_of_water", 248.84, 0.0),
            ("inches_of_mercury", 3386.389, 0.0),
            ("atmosphere", 101_325.0, 0.0),
            ("hectopascal", 100.0, 0.0),
            // energy (SI base: joule)
            ("joule", 1.0, 0.0),
            ("kilojoule", 1000.0, 0.0),
            ("megajoule", 1e6, 0.0),
            ("kilowatt_hour", 3.6e6, 0.0),
            ("watt_hour", 3600.0, 0.0),
            ("btu", 1055.06, 0.0),
            ("megabtu", 1.055_06e9, 0.0),
            ("therm", 1.055_06e8, 0.0),
            ("tons_refrigeration_hour", 1.2661e7, 0.0),
            ("kilobtu", 1.055_06e6, 0.0),
            // power (SI base: watt)
            ("watt", 1.0, 0.0),
            ("kilowatt", 1000.0, 0.0),
            ("megawatt", 1e6, 0.0),
            ("horsepower", 745.7, 0.0),
            ("btus_per_hour", 0.293_07, 0.0),
            ("tons_refrigeration", 3516.85, 0.0),
            ("kilobtus_per_hour", 293.07, 0.0),
            // volume (SI base: cubic_meter)
            ("cubic_meter", 1.0, 0.0),
            ("liter", 0.001, 0.0),
            ("milliliter", 1e-6, 0.0),
            ("gallon", 0.003_785, 0.0),
            ("quart", 0.000_946_353, 0.0),
            ("pint", 0.000_473_176, 0.0),
            ("fluid_ounce", 2.957e-5, 0.0),
            ("cubic_foot", 0.028_317, 0.0),
            ("imperial_gallon", 0.004_546, 0.0),
            // volumetric flow (SI base: cubic_meters_per_second)
            ("cubic_meters_per_second", 1.0, 0.0),
            ("liters_per_second", 0.001, 0.0),
            ("liters_per_minute", 1.667e-5, 0.0),
            ("cubic_feet_per_minute", 0.000_472, 0.0),
            ("gallons_per_minute", 6.309e-5, 0.0),
            ("cubic_meters_per_hour", 2.778e-4, 0.0),
            ("liters_per_hour", 2.778e-7, 0.0),
            // area (SI base: square_meter)
            ("square_meter", 1.0, 0.0),
            ("square_foot", 0.0929, 0.0),
            ("square_kilometer", 1e6, 0.0),
            ("square_mile", 2.59e6, 0.0),
            ("acre", 4046.86, 0.0),
            ("square_centimeter", 1e-4, 0.0),
            ("square_inch", 6.452e-4, 0.0),
            // mass (SI base: kilogram)
            ("kilogram", 1.0, 0.0),
            ("gram", 0.001, 0.0),
            ("milligram", 1e-6, 0.0),
            ("metric_ton", 1000.0, 0.0),
            ("pound", 0.4536, 0.0),
            ("ounce", 0.028_35, 0.0),
            ("short_ton", 907.185, 0.0),
            // time (SI base: second)
            ("second", 1.0, 0.0),
            ("millisecond", 0.001, 0.0),
            ("minute", 60.0, 0.0),
            ("hour", 3600.0, 0.0),
            ("day", 86400.0, 0.0),
            ("week", 604_800.0, 0.0),
            ("julian_month", 2_629_800.0, 0.0),
            ("year", 31_557_600.0, 0.0),
            // electric current (SI base: ampere)
            ("ampere", 1.0, 0.0),
            ("milliampere", 0.001, 0.0),
            // electric potential (SI base: volt)
            ("volt", 1.0, 0.0),
            ("millivolt", 0.001, 0.0),
            ("kilovolt", 1000.0, 0.0),
            ("megavolt", 1e6, 0.0),
            // frequency (SI base: hertz)
            ("hertz", 1.0, 0.0),
            ("kilohertz", 1000.0, 0.0),
            ("megahertz", 1e6, 0.0),
            ("per_minute", 1.0 / 60.0, 0.0),
            ("per_hour", 1.0 / 3600.0, 0.0),
            ("per_second", 1.0, 0.0),
            // illuminance (SI base: lux)
            ("lux", 1.0, 0.0),
            ("footcandle", 10.764, 0.0),
            ("phot", 10000.0, 0.0),
            // luminous flux (SI base: lumen)
            ("lumen", 1.0, 0.0),
        ];
        entries
            .iter()
            .map(|&(name, scale, offset)| (name, ConversionFactor { scale, offset }))
            .collect()
    });

static BASE_UNITS: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    let entries: &[(&str, &str)] = &[
        ("temperature", "celsius"),
        ("length", "meter"),
        ("pressure", "pascal"),
        ("energy", "joule"),
        ("power", "watt"),
        ("volume", "cubic_meter"),
        ("volumetric flow", "cubic_meters_per_second"),
        ("area", "square_meter"),
        ("mass", "kilogram"),
        ("time", "second"),
        ("electric current", "ampere"),
        ("electric potential", "volt"),
        ("frequency", "hertz"),
        ("illuminance", "lux"),
        ("luminous flux", "lumen"),
    ];
    entries.iter().copied().collect()
});

/// Resolve a unit string (name or symbol) to its registry entry and conversion factor.
fn resolve(s: &str) -> Result<(&'static Unit, &'static ConversionFactor), UnitError> {
    let unit = unit_for(s).ok_or_else(|| UnitError::UnknownUnit(s.to_string()))?;
    let cf = CONVERSION_FACTORS
        .get(unit.name.as_str())
        .ok_or_else(|| UnitError::UnknownUnit(s.to_string()))?;
    Ok((unit, cf))
}

/// Convert a value from one unit to another.
///
/// Both units must belong to the same quantity (e.g. both are lengths).
/// Units can be specified by name (`"fahrenheit"`) or symbol (`"°F"`).
///
/// # Precision
///
/// Conversions use IEEE 754 double-precision arithmetic. Chained conversions
/// (e.g., °F → °C → K → °F) may accumulate floating-point error on the order
/// of ±1e-10. For exact round-trip fidelity, convert directly between source
/// and target units rather than through intermediaries.
pub fn convert(val: f64, from: &str, to: &str) -> Result<f64, UnitError> {
    let (from_unit, from_cf) = resolve(from)?;
    let (to_unit, to_cf) = resolve(to)?;

    if from_unit.quantity != to_unit.quantity {
        return Err(UnitError::IncompatibleUnits(
            from_unit.name.clone(),
            to_unit.name.clone(),
        ));
    }

    Ok((val * from_cf.scale + from_cf.offset - to_cf.offset) / to_cf.scale)
}

/// Check if two units are compatible (same quantity).
pub fn compatible(a: &str, b: &str) -> bool {
    let (ua, ub) = match (unit_for(a), unit_for(b)) {
        (Some(ua), Some(ub)) => (ua, ub),
        _ => return false,
    };
    // Both must also have conversion factors registered
    if !CONVERSION_FACTORS.contains_key(ua.name.as_str())
        || !CONVERSION_FACTORS.contains_key(ub.name.as_str())
    {
        return false;
    }
    ua.quantity == ub.quantity
}

/// Get the quantity name for a unit.
pub fn quantity(unit: &str) -> Option<&'static str> {
    let u = unit_for(unit)?;
    // Return a &'static str by looking up in BASE_UNITS keys
    BASE_UNITS.keys().find(|&&q| q == u.quantity).copied()
}

/// Get the SI base unit name for a quantity.
pub fn base_unit(qty: &str) -> Option<&'static str> {
    BASE_UNITS.get(qty).copied()
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

    // --- conversion tests ---

    #[test]
    fn unit_convert_f_to_c() {
        let c = convert(212.0, "fahrenheit", "celsius").unwrap();
        assert!((c - 100.0).abs() < 0.01, "212°F = 100°C, got {c}");
    }

    #[test]
    fn unit_convert_c_to_f() {
        let f = convert(0.0, "celsius", "fahrenheit").unwrap();
        assert!((f - 32.0).abs() < 0.01, "0°C = 32°F, got {f}");
    }

    #[test]
    fn unit_convert_f_to_k() {
        // 32°F = 0°C = 273.15 K
        let k = convert(32.0, "fahrenheit", "kelvin").unwrap();
        assert!((k - 273.15).abs() < 0.01, "32°F = 273.15K, got {k}");
    }

    #[test]
    fn unit_convert_k_to_c() {
        let c = convert(373.15, "kelvin", "celsius").unwrap();
        assert!((c - 100.0).abs() < 0.01, "373.15K = 100°C, got {c}");
    }

    #[test]
    fn unit_convert_c_to_k() {
        let k = convert(100.0, "celsius", "kelvin").unwrap();
        assert!((k - 373.15).abs() < 0.01, "100°C = 373.15K, got {k}");
    }

    #[test]
    fn unit_convert_by_symbol() {
        let c = convert(212.0, "°F", "°C").unwrap();
        assert!((c - 100.0).abs() < 0.01);
    }

    #[test]
    fn unit_convert_psi_to_kpa() {
        // 1 psi ≈ 6.895 kPa
        let kpa = convert(1.0, "psi", "kPa").unwrap();
        assert!(
            (kpa - 6.894_757).abs() < 0.01,
            "1 psi ≈ 6.895 kPa, got {kpa}"
        );
    }

    #[test]
    fn unit_convert_bar_to_psi() {
        // 1 bar ≈ 14.504 psi
        let psi = convert(1.0, "bar", "psi").unwrap();
        assert!((psi - 14.504).abs() < 0.1, "1 bar ≈ 14.504 psi, got {psi}");
    }

    #[test]
    fn unit_convert_feet_to_meters() {
        let m = convert(1.0, "foot", "meter").unwrap();
        assert!((m - 0.3048).abs() < 0.0001);
    }

    #[test]
    fn unit_convert_miles_to_km() {
        let km = convert(1.0, "mile", "kilometer").unwrap();
        assert!((km - 1.609_344).abs() < 0.001);
    }

    #[test]
    fn unit_convert_kwh_to_btu() {
        // 1 kWh ≈ 3412.14 BTU
        let btu = convert(1.0, "kilowatt_hour", "btu").unwrap();
        assert!((btu - 3412.14).abs() < 1.0, "1 kWh ≈ 3412 BTU, got {btu}");
    }

    #[test]
    fn unit_convert_gallons_to_liters() {
        // 1 gal ≈ 3.785 L
        let l = convert(1.0, "gallon", "liter").unwrap();
        assert!((l - 3.785).abs() < 0.01);
    }

    #[test]
    fn unit_convert_hours_to_seconds() {
        let s = convert(1.0, "hour", "second").unwrap();
        assert!((s - 3600.0).abs() < 0.01);
    }

    #[test]
    fn unit_convert_identity() {
        let v = convert(42.0, "celsius", "celsius").unwrap();
        assert!((v - 42.0).abs() < 1e-10);
    }

    #[test]
    fn unit_convert_incompatible() {
        let err = convert(1.0, "celsius", "meter").unwrap_err();
        assert!(matches!(err, UnitError::IncompatibleUnits(_, _)));
    }

    #[test]
    fn unit_convert_unknown() {
        let err = convert(1.0, "nonexistent_xyz", "celsius").unwrap_err();
        assert!(matches!(err, UnitError::UnknownUnit(_)));
    }

    #[test]
    fn unit_compatible_same_quantity() {
        assert!(compatible("fahrenheit", "celsius"));
        assert!(compatible("°F", "°C"));
        assert!(compatible("meter", "foot"));
        assert!(compatible("psi", "bar"));
    }

    #[test]
    fn unit_compatible_different_quantity() {
        assert!(!compatible("celsius", "meter"));
        assert!(!compatible("watt", "joule"));
    }

    #[test]
    fn unit_compatible_unknown() {
        assert!(!compatible("nonexistent_xyz", "celsius"));
    }

    #[test]
    fn unit_quantity_lookup() {
        assert_eq!(quantity("fahrenheit"), Some("temperature"));
        assert_eq!(quantity("meter"), Some("length"));
        assert_eq!(quantity("psi"), Some("pressure"));
        assert_eq!(quantity("nonexistent_xyz"), None);
    }

    #[test]
    fn unit_base_unit_lookup() {
        assert_eq!(base_unit("temperature"), Some("celsius"));
        assert_eq!(base_unit("length"), Some("meter"));
        assert_eq!(base_unit("pressure"), Some("pascal"));
        assert_eq!(base_unit("energy"), Some("joule"));
        assert_eq!(base_unit("power"), Some("watt"));
        assert_eq!(base_unit("volume"), Some("cubic_meter"));
        assert_eq!(
            base_unit("volumetric flow"),
            Some("cubic_meters_per_second")
        );
        assert_eq!(base_unit("area"), Some("square_meter"));
        assert_eq!(base_unit("mass"), Some("kilogram"));
        assert_eq!(base_unit("time"), Some("second"));
        assert_eq!(base_unit("electric current"), Some("ampere"));
        assert_eq!(base_unit("electric potential"), Some("volt"));
        assert_eq!(base_unit("frequency"), Some("hertz"));
        assert_eq!(base_unit("illuminance"), Some("lux"));
        assert_eq!(base_unit("luminous flux"), Some("lumen"));
        assert_eq!(base_unit("nonexistent"), None);
    }

    #[test]
    fn unit_error_display() {
        let e = UnitError::UnknownUnit("bogus".into());
        assert_eq!(e.to_string(), "unknown unit: bogus");
        let e = UnitError::IncompatibleUnits("celsius".into(), "meter".into());
        assert_eq!(e.to_string(), "incompatible units: celsius and meter");
    }
}

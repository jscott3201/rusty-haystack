//! Shared formatting helpers used by multiple codecs.

use chrono::{NaiveTime, Timelike};

/// Format a float value compactly: no trailing ".0" for integers.
///
/// Handles INF, -INF, NaN as strings.
pub fn format_number_val(val: f64) -> String {
    if val.is_infinite() {
        return if val > 0.0 {
            "INF".to_string()
        } else {
            "-INF".to_string()
        };
    }
    if val.is_nan() {
        return "NaN".to_string();
    }
    let s = format!("{val}");
    if s.ends_with(".0") && !s.contains('e') && !s.contains('E') {
        s[..s.len() - 2].to_string()
    } else {
        s
    }
}

/// Format a NaiveTime as `HH:MM:SS[.frac]`, trimming trailing fractional zeros.
pub fn format_time(t: &NaiveTime) -> String {
    let nanos = t.nanosecond();
    if nanos == 0 {
        t.format("%H:%M:%S").to_string()
    } else {
        let base = t.format("%H:%M:%S").to_string();
        let frac = format!(".{:09}", nanos);
        let trimmed = frac.trim_end_matches('0');
        format!("{base}{trimmed}")
    }
}

/// Format fractional seconds from nanoseconds, or empty string if zero.
pub fn format_frac_seconds(nanos: u32) -> String {
    if nanos > 0 {
        let s = format!(".{:09}", nanos);
        s.trim_end_matches('0').to_string()
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_number_val_integer() {
        assert_eq!(format_number_val(42.0), "42");
    }

    #[test]
    fn format_number_val_zero() {
        assert_eq!(format_number_val(0.0), "0");
    }

    #[test]
    fn format_number_val_float() {
        assert_eq!(format_number_val(72.5), "72.5");
    }

    #[test]
    fn format_number_val_negative() {
        assert_eq!(format_number_val(-23.45), "-23.45");
    }

    #[test]
    fn format_number_val_inf() {
        assert_eq!(format_number_val(f64::INFINITY), "INF");
    }

    #[test]
    fn format_number_val_neg_inf() {
        assert_eq!(format_number_val(f64::NEG_INFINITY), "-INF");
    }

    #[test]
    fn format_number_val_nan() {
        assert_eq!(format_number_val(f64::NAN), "NaN");
    }

    #[test]
    fn format_time_no_frac() {
        let t = NaiveTime::from_hms_opt(8, 12, 5).unwrap();
        assert_eq!(format_time(&t), "08:12:05");
    }

    #[test]
    fn format_time_with_millis() {
        let t = NaiveTime::from_hms_milli_opt(14, 30, 0, 123).unwrap();
        assert_eq!(format_time(&t), "14:30:00.123");
    }

    #[test]
    fn format_frac_seconds_zero() {
        assert_eq!(format_frac_seconds(0), "");
    }

    #[test]
    fn format_frac_seconds_with_nanos() {
        assert_eq!(format_frac_seconds(123000000), ".123");
    }

    #[test]
    fn format_frac_seconds_fine_precision() {
        assert_eq!(format_frac_seconds(123456789), ".123456789");
    }
}

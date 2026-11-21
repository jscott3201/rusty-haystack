use super::context::{Class, Scalar};
use super::*;
use crate::kinds::{Float, Number};
/// Length of one strict JSON numeric prefix. Unit parsing may inspect the tail.
pub(super) fn number_prefix(text: &str) -> Option<usize> {
    let b = text.as_bytes();
    let mut at = usize::from(b.first() == Some(&b'-'));
    match b.get(at) {
        Some(b'0') => at += 1,
        Some(b'1'..=b'9') => {
            at += 1;
            while b.get(at).is_some_and(u8::is_ascii_digit) {
                at += 1;
            }
        }
        _ => return None,
    }
    if b.get(at) == Some(&b'.') {
        at += 1;
        let start = at;
        while b.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
        }
        if at == start {
            return None;
        }
    }
    if matches!(b.get(at), Some(b'e' | b'E')) {
        at += 1;
        if matches!(b.get(at), Some(b'+' | b'-')) {
            at += 1;
        }
        let start = at;
        while b.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
        }
        if at == start {
            return None;
        }
    }
    Some(at)
}
// Exact decimal/exponent-to-i64 conversion without a binary64 intermediate or
// arbitrary-precision allocation. Scan bounded source digits and reject every
// nonzero fractional tail before checked magnitude accumulation.
fn integer(text: &str) -> Option<i64> {
    let negative = text.starts_with('-');
    let text = text.strip_prefix('-').unwrap_or(text);
    let (mantissa, exponent) = text.split_once(['e', 'E']).unwrap_or((text, "0"));
    let exp_negative = exponent.starts_with('-');
    let exponent_digits = exponent.strip_prefix(['-', '+']).unwrap_or(exponent);
    let mut exponent = 0_i64;
    for b in exponent_digits.bytes() {
        exponent = exponent
            .saturating_mul(10)
            .saturating_add(i64::from(b - b'0'))
            .min(2_000_000_000);
    }
    if exp_negative {
        exponent = -exponent;
    }
    let fraction = mantissa.split_once('.').map_or(0, |(_, tail)| tail.len());
    let mut zeros = 0usize;
    let mut significant = 0usize;
    let mut started = false;
    for byte in mantissa.bytes().filter(|b| *b != b'.') {
        if !started && byte == b'0' {
            continue;
        }
        started = true;
        significant += 1;
        if byte == b'0' {
            zeros += 1;
        } else {
            zeros = 0;
        }
    }
    if !started {
        return Some(0);
    }
    let shift = exponent - i64::try_from(fraction).ok()?;
    let trim = if shift < 0 {
        usize::try_from(-shift).ok()?
    } else {
        0
    };
    if trim > zeros {
        return None;
    }
    let digits = significant
        .checked_sub(trim)?
        .checked_add(usize::try_from(shift.max(0)).ok()?)?;
    if digits > 19 {
        return None;
    }
    let mut magnitude = 0_u64;
    let mut seen = 0usize;
    let mut started = false;
    for byte in mantissa.bytes().filter(|b| *b != b'.') {
        if !started && byte == b'0' {
            continue;
        }
        started = true;
        if seen >= significant - trim {
            break;
        }
        magnitude = magnitude
            .checked_mul(10)?
            .checked_add(u64::from(byte - b'0'))?;
        seen += 1;
    }
    for _ in 0..shift.max(0) {
        magnitude = magnitude.checked_mul(10)?;
    }
    if negative {
        if magnitude == 1_u64 << 63 {
            Some(i64::MIN)
        } else {
            i64::try_from(magnitude).ok()?.checked_neg()
        }
    } else {
        i64::try_from(magnitude).ok()
    }
}
pub(super) fn numeric<E>(text: &str, expected: Option<&Class>) -> Result<Kind, Error<E>> {
    if number_prefix(text) != Some(text.len()) {
        return Err(invalid("invalid numeric grammar"));
    }
    let target = match expected {
        Some(Class::Scalar(s @ (Scalar::Int | Scalar::Float | Scalar::Number))) => *s,
        _ if !text.contains(['.', 'e', 'E']) => Scalar::Int,
        _ => Scalar::Float,
    };
    if target == Scalar::Int {
        return integer(text)
            .map(Kind::Int)
            .ok_or_else(|| invalid("fractional or out-of-range Int"));
    }
    let number: f64 = text
        .parse()
        .map_err(|_| invalid("invalid floating-point number"))?;
    if !number.is_finite() {
        return Err(invalid("floating-point number outside binary64 range"));
    }
    Ok(if target == Scalar::Number {
        Kind::Number(Number::unitless(number))
    } else {
        Kind::Float(Float::new(number))
    })
}

fn special(text: &str) -> Option<f64> {
    match text {
        "NaN" => Some(f64::from_bits(0x7ff8000000000000)),
        "INF" => Some(f64::INFINITY),
        "-INF" => Some(f64::NEG_INFINITY),
        _ => None,
    }
}
fn valid_unit(unit: &str) -> bool {
    !unit.is_empty()
        && unit
            .chars()
            .all(|c| !c.is_ascii() || c.is_ascii_alphabetic() || "%_/$".contains(c))
}
fn valid_ref(text: &str) -> bool {
    text.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"._~:-".contains(&b))
}
fn number_text<E>(text: &str) -> Result<Kind, Error<E>> {
    if let Some(value) = special(text) {
        return Ok(Kind::Number(Number::unitless(value)));
    }
    // e/E may begin a unit when they do not form an exponent (e.g. 1eV).
    let end = number_prefix(text)
        .or_else(|| {
            let at = text.find(['e', 'E'])?;
            (number_prefix(&text[..at]) == Some(at)).then_some(at)
        })
        .ok_or_else(|| invalid("invalid Number text"))?;
    let unit = &text[end..];
    if !unit.is_empty() && !valid_unit(unit) {
        return Err(invalid("invalid Number unit"));
    }
    let val: f64 = text[..end].parse().map_err(|_| invalid("invalid Number"))?;
    if !val.is_finite() {
        return Err(invalid("Number outside binary64 range"));
    }
    Ok(Kind::Number(Number::new(
        val,
        (!unit.is_empty()).then(|| unit.to_owned()),
    )))
}
fn time_grammar(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() >= 8
        && b[2] == b':'
        && b[5] == b':'
        && b[..8]
            .iter()
            .enumerate()
            .all(|(i, b)| matches!(i, 2 | 5) || b.is_ascii_digit())
        && (b.len() == 8
            || (b[8] == b'.'
                && (10..=18).contains(&b.len())
                && b[9..].iter().all(u8::is_ascii_digit)))
}
fn date_grammar(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, b)| matches!(i, 4 | 7) || b.is_ascii_digit())
}
fn zone_grammar(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_+-".contains(&b))
}
pub(super) fn string<E>(
    text: &str,
    context: &Context,
    expected_name: Option<&str>,
    expected: Option<&Class>,
) -> Result<Kind, Error<E>> {
    use crate::kinds::{HDateTime, HRef, NominalScalar, Uri};
    use base64::Engine;
    Ok(match expected {
        None | Some(Class::Any | Class::Scalar(Scalar::Str)) => Kind::Str(text.into()),
        Some(Class::Scalar(Scalar::Int)) => {
            if text.contains(['.', 'e', 'E']) {
                return Err(invalid("Int text requires integer grammar"));
            }
            numeric(text, expected)?
        }
        Some(Class::Scalar(Scalar::Float)) => {
            if let Some(value) = special(text) {
                Kind::Float(Float::new(value))
            } else {
                numeric(text, expected)?
            }
        }
        Some(Class::Scalar(Scalar::Number)) => number_text(text)?,
        Some(Class::Scalar(Scalar::Bool)) => Kind::Bool(match text {
            "true" => true,
            "false" => false,
            _ => return Err(invalid("invalid Bool text")),
        }),
        Some(Class::Scalar(Scalar::Ref)) if valid_ref(text) => Kind::Ref(HRef::from_val(text)),
        Some(Class::Scalar(Scalar::Marker)) if text == "✓" => Kind::Marker,
        Some(Class::Scalar(Scalar::None)) if text == "∅" => Kind::None,
        Some(Class::Scalar(Scalar::NA)) if text == "NA" => Kind::NA,
        Some(Class::Scalar(Scalar::Uri)) => Kind::Uri(Uri::new(text)),
        Some(Class::Scalar(Scalar::Buf)) => Kind::Buf(
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(text)
                .map_err(|_| invalid("invalid canonical base64url"))?,
        ),
        Some(Class::Scalar(Scalar::Date)) if date_grammar(text) => Kind::Date(
            chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d")
                .map_err(|_| invalid("invalid Date"))?,
        ),
        Some(Class::Scalar(Scalar::Time)) if time_grammar(text) => Kind::Time(
            chrono::NaiveTime::parse_from_str(text, "%H:%M:%S%.f")
                .map_err(|_| invalid("invalid Time"))?,
        ),
        Some(Class::Scalar(Scalar::DateTime)) => {
            let (iso, zone) = text
                .split_once(' ')
                .map_or((text, None), |(iso, zone)| (iso, Some(zone)));
            if !iso.is_ascii()
                || iso.len() < 20
                || iso.as_bytes()[10] != b'T'
                || !date_grammar(&iso[..10])
            {
                return Err(invalid("invalid DateTime grammar"));
            }
            let (clock, offset) = if let Some(clock) = iso[11..].strip_suffix('Z') {
                (clock, "Z")
            } else {
                let at = iso
                    .len()
                    .checked_sub(6)
                    .ok_or_else(|| invalid("invalid DateTime offset"))?;
                if at < 11 {
                    return Err(invalid("invalid DateTime offset"));
                }
                let offset = &iso[at..];
                let b = offset.as_bytes();
                if !matches!(b[0], b'+' | b'-')
                    || b[3] != b':'
                    || !b
                        .iter()
                        .enumerate()
                        .all(|(i, b)| matches!(i, 0 | 3) || b.is_ascii_digit())
                {
                    return Err(invalid("invalid DateTime offset"));
                }
                (&iso[11..at], offset)
            };
            if !time_grammar(clock)
                || zone.is_some_and(|z| !zone_grammar(z))
                || zone.is_none() && offset != "Z"
            {
                return Err(invalid("invalid DateTime clock or timezone"));
            }
            let value = chrono::DateTime::parse_from_rfc3339(iso)
                .map_err(|_| invalid("invalid DateTime"))?;
            Kind::DateTime(HDateTime::new(value, zone.unwrap_or("UTC")))
        }
        Some(Class::Enum(keys)) => {
            if keys.binary_search_by(|key| key.as_str().cmp(text)).is_err() {
                return Err(invalid("unknown enum key"));
            }
            Kind::Nominal(
                NominalScalar::new(
                    expected_name.ok_or(Error::UnknownSpec)?,
                    context.catalog(),
                    context.revision(),
                    text,
                )
                .map_err(|_| invalid("invalid nominal identity"))?,
            )
        }
        Some(Class::Nominal(pattern)) => {
            if !pattern
                .is_match(text)
                .map_err(|_| invalid("nominal matcher failed"))?
            {
                return Err(invalid("invalid nominal grammar"));
            }
            Kind::Nominal(
                NominalScalar::new(
                    expected_name.ok_or(Error::UnknownSpec)?,
                    context.catalog(),
                    context.revision(),
                    text,
                )
                .map_err(|_| invalid("invalid nominal identity"))?,
            )
        }
        _ => return Err(invalid("invalid scalar grammar or expected container")),
    })
}
#[derive(Clone, Copy)]
pub(super) enum Form {
    Bool(bool),
    Number,
    String,
}
pub(super) struct Plain<'a> {
    pub spec: &'a str,
    pub text: String,
    pub form: Form,
}
fn float_text(value: f64) -> String {
    if value.is_nan() {
        "NaN".into()
    } else if value == f64::INFINITY {
        "INF".into()
    } else if value == f64::NEG_INFINITY {
        "-INF".into()
    } else {
        format!("{value:?}")
    }
}
fn supported_bits(value: f64) -> bool {
    !value.is_nan() || value.to_bits() == 0x7ff8000000000000
}
pub(super) fn source_bytes(value: &Kind) -> usize {
    match value {
        Kind::Str(s) => s.len(),
        Kind::Ref(r) => r
            .val
            .len()
            .saturating_add(r.dis.as_ref().map_or(0, String::len)),
        Kind::Number(n) => n.unit.as_ref().map_or(0, String::len),
        Kind::Buf(b) => b.len(),
        Kind::Uri(u) => u.val().len(),
        Kind::DateTime(t) => t.tz_name.len(),
        Kind::Nominal(n) => n
            .text()
            .len()
            .saturating_add(n.spec().len())
            .saturating_add(n.catalog().len())
            .saturating_add(n.revision().len()),
        _ => 0,
    }
}
pub(super) fn representation<'a>(value: &'a Kind, context: &Context) -> Result<Plain<'a>, Reason> {
    use base64::Engine;
    use chrono::{Datelike, Timelike};
    let (spec, text, form) = match value {
        Kind::Bool(b) => ("sys::Bool", b.to_string(), Form::Bool(*b)),
        Kind::Int(v) => ("sys::Int", v.to_string(), Form::Number),
        Kind::Float(v) if supported_bits(v.value()) => (
            "sys::Float",
            float_text(v.value()),
            if v.value().is_finite() {
                Form::Number
            } else {
                Form::String
            },
        ),
        Kind::Number(n)
            if supported_bits(n.val)
                && (n.unit.is_none()
                    || n.val.is_finite() && n.unit.as_deref().is_some_and(valid_unit)) =>
        {
            let mut text = float_text(n.val);
            if let Some(unit) = &n.unit {
                text.push_str(unit);
            }
            (
                "sys::Number",
                text,
                if n.val.is_finite() && n.unit.is_none() {
                    Form::Number
                } else {
                    Form::String
                },
            )
        }
        Kind::Str(s) => ("sys::Str", s.clone(), Form::String),
        Kind::Ref(r) if valid_ref(&r.val) => ("sys::Ref", r.val.clone(), Form::String),
        Kind::Marker => ("sys::Marker", "✓".into(), Form::String),
        Kind::None => ("sys::None", "∅".into(), Form::String),
        Kind::NA => ("sys::NA", "NA".into(), Form::String),
        Kind::Buf(b) => (
            "sys::Buf",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b),
            Form::String,
        ),
        Kind::Uri(u) => ("sys::Uri", u.val().to_owned(), Form::String),
        Kind::Date(d) if (0..=9999).contains(&d.year()) => {
            ("sys::Date", d.to_string(), Form::String)
        }
        Kind::Time(t) if t.nanosecond() < 1_000_000_000 || t.second() == 59 => (
            "sys::Time",
            crate::codecs::shared::format_time(t),
            Form::String,
        ),
        Kind::DateTime(t)
            if crate::codecs::shared::h4_datetime_representable(&t.dt)
                && zone_grammar(&t.tz_name) =>
        {
            let mut text = format!(
                "{}{}",
                t.dt.format("%Y-%m-%dT%H:%M:%S"),
                crate::codecs::shared::format_frac_seconds(t.dt.nanosecond())
            );
            if t.dt.offset().local_minus_utc() == 0 {
                text.push('Z');
            } else {
                text.push_str(&t.dt.format("%:z").to_string());
            }
            text.push(' ');
            text.push_str(&t.tz_name);
            ("sys::DateTime", text, Form::String)
        }
        Kind::Nominal(n) => {
            if n.catalog() != context.catalog() || n.revision() != context.revision() {
                return Err(Reason::CatalogMismatch);
            }
            (n.spec(), n.text().to_owned(), Form::String)
        }
        Kind::Remove | Kind::Symbol(_) | Kind::XStr(_) | Kind::Coord(_) => {
            return Err(Reason::UnsupportedScalar);
        }
        _ => return Err(Reason::InvalidScalar),
    };
    Ok(Plain { spec, text, form })
}
pub(super) fn same(a: &Kind, b: &Kind) -> bool {
    match (a, b) {
        (Kind::Ref(a), Kind::Ref(b)) => a.val == b.val && a.dis == b.dis,
        (Kind::DateTime(a), Kind::DateTime(b)) => {
            a.dt == b.dt && a.dt.offset() == b.dt.offset() && a.tz_name == b.tz_name
        }
        _ => a == b,
    }
}

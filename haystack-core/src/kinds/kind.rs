use super::*;
use chrono::{NaiveDate, NaiveTime};
use std::fmt;

/// The central Haystack value type. Every tag value is a Kind.
#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Null,
    Marker,
    NA,
    Remove,
    Bool(bool),
    Number(Number),
    Str(String),
    Ref(HRef),
    Uri(Uri),
    Symbol(Symbol),
    Date(NaiveDate),
    Time(NaiveTime),
    DateTime(HDateTime),
    Coord(Coord),
    XStr(XStr),
    List(Vec<Kind>),
    Dict(Box<crate::data::HDict>),
    Grid(Box<crate::data::HGrid>),
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Kind::Null => write!(f, "null"),
            Kind::Marker => write!(f, "{}", super::singletons::Marker),
            Kind::NA => write!(f, "{}", super::singletons::NA),
            Kind::Remove => write!(f, "{}", super::singletons::Remove),
            Kind::Bool(v) => write!(f, "{v}"),
            Kind::Number(n) => write!(f, "{n}"),
            Kind::Str(s) => write!(f, "{s}"),
            Kind::Ref(r) => write!(f, "{r}"),
            Kind::Uri(u) => write!(f, "{u}"),
            Kind::Symbol(s) => write!(f, "{s}"),
            Kind::Date(d) => write!(f, "{d}"),
            Kind::Time(t) => write!(f, "{}", t.format("%H:%M:%S")),
            Kind::DateTime(dt) => write!(f, "{dt}"),
            Kind::Coord(c) => write!(f, "{c}"),
            Kind::XStr(x) => write!(f, "{x}"),
            Kind::List(items) => {
                write!(f, "[")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{item}")?;
                }
                write!(f, "]")
            }
            Kind::Dict(d) => write!(f, "{d}"),
            Kind::Grid(g) => write!(f, "{g}"),
        }
    }
}

// Convenience From implementations
impl From<bool> for Kind {
    fn from(v: bool) -> Self {
        Kind::Bool(v)
    }
}

impl From<Number> for Kind {
    fn from(n: Number) -> Self {
        Kind::Number(n)
    }
}

impl From<HRef> for Kind {
    fn from(r: HRef) -> Self {
        Kind::Ref(r)
    }
}

impl From<String> for Kind {
    fn from(s: String) -> Self {
        Kind::Str(s)
    }
}

impl From<&str> for Kind {
    fn from(s: &str) -> Self {
        Kind::Str(s.to_string())
    }
}

impl From<Uri> for Kind {
    fn from(u: Uri) -> Self {
        Kind::Uri(u)
    }
}

impl From<Symbol> for Kind {
    fn from(s: Symbol) -> Self {
        Kind::Symbol(s)
    }
}

impl From<Coord> for Kind {
    fn from(c: Coord) -> Self {
        Kind::Coord(c)
    }
}

impl From<XStr> for Kind {
    fn from(x: XStr) -> Self {
        Kind::XStr(x)
    }
}

impl From<NaiveDate> for Kind {
    fn from(d: NaiveDate) -> Self {
        Kind::Date(d)
    }
}

impl From<NaiveTime> for Kind {
    fn from(t: NaiveTime) -> Self {
        Kind::Time(t)
    }
}

impl From<HDateTime> for Kind {
    fn from(dt: HDateTime) -> Self {
        Kind::DateTime(dt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_null() {
        assert_eq!(Kind::Null.to_string(), "null");
    }

    #[test]
    fn kind_marker() {
        assert_eq!(Kind::Marker.to_string(), "\u{2713}");
    }

    #[test]
    fn kind_bool() {
        assert_eq!(Kind::Bool(true).to_string(), "true");
        assert_eq!(Kind::Bool(false).to_string(), "false");
    }

    #[test]
    fn kind_number() {
        let k = Kind::Number(Number::new(72.5, Some("°F".into())));
        assert_eq!(k.to_string(), "72.5°F");
    }

    #[test]
    fn kind_str() {
        let k = Kind::Str("hello".into());
        assert_eq!(k.to_string(), "hello");
    }

    #[test]
    fn kind_ref() {
        let k = Kind::Ref(HRef::from_val("site-1"));
        assert_eq!(k.to_string(), "@site-1");
    }

    #[test]
    fn kind_list() {
        let k = Kind::List(vec![
            Kind::Number(Number::unitless(1.0)),
            Kind::Str("two".into()),
        ]);
        assert_eq!(k.to_string(), "[1, two]");
    }

    #[test]
    fn kind_equality() {
        assert_eq!(Kind::Marker, Kind::Marker);
        assert_ne!(Kind::Marker, Kind::NA);
        assert_eq!(
            Kind::Number(Number::unitless(42.0)),
            Kind::Number(Number::unitless(42.0))
        );
    }

    #[test]
    fn kind_from_conversions() {
        let _: Kind = true.into();
        let _: Kind = Number::unitless(1.0).into();
        let _: Kind = HRef::from_val("x").into();
        let _: Kind = "hello".into();
        let _: Kind = String::from("hello").into();
    }
}

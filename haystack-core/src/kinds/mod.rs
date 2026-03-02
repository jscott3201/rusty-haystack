//! Haystack type system — the [`Kind`] enum and its 15 scalar types.
//!
//! Every value in the Haystack data model is represented as a [`Kind`] variant:
//!
//! | Variant | Rust Type | Zinc Example |
//! |---------|-----------|--------------|
//! | `Marker` | [`Marker`] | `M` |
//! | `NA` | [`NA`] | `NA` |
//! | `Remove` | [`Remove`] | `R` |
//! | `Bool(bool)` | `bool` | `T` / `F` |
//! | `Number(Number)` | [`Number`] | `72°F`, `100kW` |
//! | `Str(String)` | `String` | `"hello"` |
//! | `Ref(HRef)` | [`HRef`] | `@site-1 "Main Site"` |
//! | `Uri(Uri)` | [`Uri`] | `` `http://example.com` `` |
//! | `Date(NaiveDate)` | `chrono::NaiveDate` | `2024-01-15` |
//! | `Time(NaiveTime)` | `chrono::NaiveTime` | `13:30:00` |
//! | `DateTime(HDateTime)` | [`HDateTime`] | `2024-01-15T13:30:00-05:00 New_York` |
//! | `Coord(Coord)` | [`Coord`] | `C(37.55,-77.45)` |
//! | `Symbol(Symbol)` | [`Symbol`] | `^hot-water` |
//! | `XStr(XStr)` | [`XStr`] | `Type("value")` |
//! | `List(HList)` | [`HList`](crate::data::HList) | `[1, 2, 3]` |
//! | `Dict(Box<HDict>)` | [`HDict`](crate::data::HDict) | `{dis:"Room" area:100}` |
//! | `Grid(Box<HGrid>)` | [`HGrid`](crate::data::HGrid) | Nested grid |
//!
//! The [`units`] submodule provides a database of standard Haystack units with
//! lookup by name ([`unit_for`]) or symbol ([`units_by_symbol`]).

mod singletons;
pub use singletons::{Marker, NA, Remove};

mod number;
pub use number::Number;

mod ref_;
pub use ref_::HRef;

mod coord;
pub use coord::Coord;

mod uri;
pub use uri::Uri;

mod symbol;
pub use symbol::Symbol;

mod xstr;
pub use xstr::XStr;

mod datetime;
pub use datetime::HDateTime;

mod kind;
pub use kind::Kind;

mod units;
pub use units::{Unit, unit_for, units_by_name, units_by_symbol};

mod tz;
pub use tz::{tz_for, tz_map};

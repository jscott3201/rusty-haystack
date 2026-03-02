//! Haystack data collection types.
//!
//! - [`HDict`] — Tag dictionary (ordered map of name → [`Kind`](crate::kinds::Kind) value).
//!   Represents a single entity, grid metadata, or column metadata.
//! - [`HGrid`] — Two-dimensional table of [`HCol`] columns and [`HDict`] rows.
//!   The primary data exchange format in the Haystack protocol.
//! - [`HCol`] — Named column with optional metadata dict.
//! - [`HList`] — Ordered list of [`Kind`](crate::kinds::Kind) values.

mod dict;
mod grid;
mod list;

pub use dict::HDict;
pub use grid::{HCol, HGrid};
pub use list::HList;

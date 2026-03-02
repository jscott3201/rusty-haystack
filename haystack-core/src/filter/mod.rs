//! Haystack filter expression parser and evaluator.
//!
//! Parses filter strings into an AST ([`FilterNode`]) and evaluates them against
//! entities ([`HDict`](crate::data::HDict)). Supports the full Haystack filter grammar:
//!
//! - **Tag presence**: `site`, `not deprecated`
//! - **Comparisons**: `temp > 72`, `area == 5000`, `dis == "Main Site"`
//! - **Boolean logic**: `site and area > 1000`, `ahu or vav`
//! - **Path traversal**: `equipRef->siteRef->area > 1000`
//! - **Wildcards**: `"*"` is accepted as a special-case read-all (not standard Haystack)
//!
//! # Usage
//!
//! ```rust
//! use haystack_core::filter::{parse_filter, matches};
//! use haystack_core::data::HDict;
//! use haystack_core::kinds::{Kind, Number};
//!
//! let ast = parse_filter("site and area > 1000").unwrap();
//! let mut entity = HDict::new();
//! entity.set("site", Kind::Marker);
//! entity.set("area", Kind::Number(Number::unitless(5000.0)));
//! assert!(matches(&ast, &entity, None));
//! ```
//!
//! # Safety Limits
//!
//! The parser enforces a maximum recursion depth of 100 to prevent stack overflow
//! from maliciously nested filter expressions.

mod ast;
mod eval;
mod parser;

pub use ast::{CmpOp, FilterNode, Path};
pub use eval::{matches, matches_with_ns};
pub use parser::{FilterError, parse_filter};

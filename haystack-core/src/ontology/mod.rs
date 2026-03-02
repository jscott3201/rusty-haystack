//! Haystack 4 ontology layer — defs, taxonomy, conjuncts, and validation.
//!
//! Implements the Haystack 4 def system for defining entity types, tags,
//! relationships, and constraints:
//!
//! - [`DefNamespace`] — Root container holding all loaded defs and libs.
//!   Provides lookup, taxonomy traversal, tag requirements, and validation.
//! - [`Def`] — Individual definition (e.g., `site`, `equip`, `temp-sensor`).
//!   Carries symbol, doc, supertypes (`is`), and tag constraints (`tagOn`).
//! - [`DefKind`] — Discriminant for def categories (tag, conjunct, entity, lib, feature, etc.).
//! - [`Lib`] — Named library of defs loaded from Trio source (e.g., `ph`, `phIoT`).
//! - [`TaxonomyTree`] — Subtype hierarchy tree for `is`-based inheritance queries.
//! - [`ConjunctIndex`] — Index of compound entity types (e.g., `hot-water-plant`).
//!
//! ## Loading Defs
//!
//! Defs are loaded from Trio-formatted source via [`DefNamespace::load_trio_str()`]
//! or from pre-registered [`LibSource`] entries. The standard Project Haystack
//! libraries (`ph`, `phScience`, `phIoT`, `phIct`) are available by default.

pub mod conjunct;
pub mod def;
pub mod lib;
pub mod namespace;
pub mod taxonomy;
pub mod trio_loader;
pub mod validation;

pub use conjunct::ConjunctIndex;
pub use def::{Def, DefKind};
pub use lib::Lib;
pub use namespace::{DefNamespace, LibSource};
pub use taxonomy::TaxonomyTree;
pub use trio_loader::load_trio;
pub use validation::{FitIssue, ValidationIssue};

use crate::codecs::CodecError;

/// Errors that can occur during ontology loading or processing.
#[derive(Debug, thiserror::Error)]
pub enum OntologyError {
    /// Error from the Trio/Zinc codec during parsing.
    #[error("codec error: {0}")]
    Codec(#[from] CodecError),
    /// Invalid def record.
    #[error("invalid def: {0}")]
    InvalidDef(String),
    /// General load error.
    #[error("load error: {0}")]
    Load(String),
}

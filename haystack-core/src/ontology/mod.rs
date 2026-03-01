// Haystack 4 ontology layer -- defs, taxonomy, conjuncts, and validation.

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

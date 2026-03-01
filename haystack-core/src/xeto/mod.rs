// Xeto schema language support -- lexer, parser, spec resolution, and fitting.

pub mod lexer;
pub mod ast;
pub mod parser;
pub mod spec;
pub mod resolver;
pub mod fitting;
pub mod loader;
pub mod export;
pub mod bundled;

pub use ast::{LibPragma, SlotDef, SpecDef, XetoFile};
pub use fitting::{fits, fits_explain, EntityResolver};
pub use lexer::{Token, TokenType, XetoLexer};
pub use parser::parse_xeto;
pub use resolver::XetoResolver;
pub use spec::{Slot, Spec};

/// Errors that can occur during Xeto parsing, resolution, or loading.
#[derive(Debug, thiserror::Error)]
pub enum XetoError {
    /// Error during tokenization or parsing.
    #[error("parse error at line {line}, col {col}: {message}")]
    Parse {
        line: usize,
        col: usize,
        message: String,
    },
    /// Error during name resolution.
    #[error("resolve error: {0}")]
    Resolve(String),
    /// Error during library loading.
    #[error("load error: {0}")]
    Load(String),
}

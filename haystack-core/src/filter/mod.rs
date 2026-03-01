// Haystack filter engine — AST, parser, and evaluator.

mod ast;
mod eval;
mod parser;

pub use ast::{CmpOp, FilterNode, Path};
pub use eval::{matches, matches_with_ns};
pub use parser::{parse_filter, FilterError};

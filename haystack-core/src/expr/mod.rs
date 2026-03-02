//! Expression evaluator for arithmetic/logic expressions over entity tags.

pub mod ast;
pub mod eval;
pub mod parser;

pub use ast::{BinOp, CmpOp, ExprNode, LogicOp, MAX_EXPR_DEPTH, MAX_EXPR_SOURCE, UnOp};
pub use eval::{Expr, ExprContext};
pub use parser::{ExprError, parse_expr};

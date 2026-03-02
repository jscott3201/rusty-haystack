//! Expression AST types for arithmetic/logic expressions over entity tags.

use crate::kinds::Kind;

/// Maximum nesting depth allowed when parsing expressions.
pub const MAX_EXPR_DEPTH: usize = 64;

/// Maximum source string length accepted by the parser.
pub const MAX_EXPR_SOURCE: usize = 4096;

/// Binary operators for arithmetic.
#[derive(Debug, Clone, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

/// Unary operators.
#[derive(Debug, Clone, PartialEq)]
pub enum UnOp {
    Neg,
    Not,
}

/// Comparison operators.
#[derive(Debug, Clone, PartialEq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// Logical operators.
#[derive(Debug, Clone, PartialEq)]
pub enum LogicOp {
    And,
    Or,
}

/// Expression AST node.
#[derive(Debug, Clone)]
pub enum ExprNode {
    /// A literal value: `72`, `"hello"`, `true`, `null`.
    Literal(Kind),
    /// A variable reference: `$tagName`.
    Variable(String),
    /// Binary arithmetic: `left op right`.
    BinaryOp {
        left: Box<ExprNode>,
        op: BinOp,
        right: Box<ExprNode>,
    },
    /// Unary operation: `op operand`.
    UnaryOp { op: UnOp, operand: Box<ExprNode> },
    /// Comparison: `left op right`.
    Comparison {
        left: Box<ExprNode>,
        op: CmpOp,
        right: Box<ExprNode>,
    },
    /// Logical connective: `left op right`.
    Logical {
        left: Box<ExprNode>,
        op: LogicOp,
        right: Box<ExprNode>,
    },
    /// Function call: `name(args...)`.
    FnCall { name: String, args: Vec<ExprNode> },
    /// Conditional: `if cond then then_expr else else_expr`.
    Conditional {
        cond: Box<ExprNode>,
        then_expr: Box<ExprNode>,
        else_expr: Box<ExprNode>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kinds::Number;

    #[test]
    fn construct_literal() {
        let node = ExprNode::Literal(Kind::Number(Number::unitless(42.0)));
        assert!(matches!(node, ExprNode::Literal(Kind::Number(_))));
    }

    #[test]
    fn construct_variable() {
        let node = ExprNode::Variable("temp".into());
        if let ExprNode::Variable(name) = &node {
            assert_eq!(name, "temp");
        } else {
            panic!("expected Variable");
        }
    }

    #[test]
    fn construct_binary_op() {
        let node = ExprNode::BinaryOp {
            left: Box::new(ExprNode::Literal(Kind::Number(Number::unitless(1.0)))),
            op: BinOp::Add,
            right: Box::new(ExprNode::Literal(Kind::Number(Number::unitless(2.0)))),
        };
        assert!(matches!(node, ExprNode::BinaryOp { op: BinOp::Add, .. }));
    }

    #[test]
    fn construct_unary_op() {
        let node = ExprNode::UnaryOp {
            op: UnOp::Neg,
            operand: Box::new(ExprNode::Literal(Kind::Number(Number::unitless(5.0)))),
        };
        assert!(matches!(node, ExprNode::UnaryOp { op: UnOp::Neg, .. }));
    }

    #[test]
    fn construct_comparison() {
        let node = ExprNode::Comparison {
            left: Box::new(ExprNode::Variable("x".into())),
            op: CmpOp::Gt,
            right: Box::new(ExprNode::Literal(Kind::Number(Number::unitless(10.0)))),
        };
        assert!(matches!(node, ExprNode::Comparison { op: CmpOp::Gt, .. }));
    }

    #[test]
    fn construct_logical() {
        let node = ExprNode::Logical {
            left: Box::new(ExprNode::Literal(Kind::Bool(true))),
            op: LogicOp::And,
            right: Box::new(ExprNode::Literal(Kind::Bool(false))),
        };
        assert!(matches!(
            node,
            ExprNode::Logical {
                op: LogicOp::And,
                ..
            }
        ));
    }

    #[test]
    fn construct_fn_call() {
        let node = ExprNode::FnCall {
            name: "abs".into(),
            args: vec![ExprNode::Literal(Kind::Number(Number::unitless(-3.0)))],
        };
        if let ExprNode::FnCall { name, args } = &node {
            assert_eq!(name, "abs");
            assert_eq!(args.len(), 1);
        } else {
            panic!("expected FnCall");
        }
    }

    #[test]
    fn construct_conditional() {
        let node = ExprNode::Conditional {
            cond: Box::new(ExprNode::Literal(Kind::Bool(true))),
            then_expr: Box::new(ExprNode::Literal(Kind::Number(Number::unitless(1.0)))),
            else_expr: Box::new(ExprNode::Literal(Kind::Number(Number::unitless(0.0)))),
        };
        assert!(matches!(node, ExprNode::Conditional { .. }));
    }

    #[test]
    fn clone_and_debug() {
        let node = ExprNode::Literal(Kind::Str("test".into()));
        let cloned = node.clone();
        let debug = format!("{:?}", cloned);
        assert!(debug.contains("Literal"));
    }

    #[test]
    fn binop_variants() {
        let ops = [BinOp::Add, BinOp::Sub, BinOp::Mul, BinOp::Div, BinOp::Mod];
        for op in &ops {
            assert_eq!(op.clone(), op.clone());
        }
    }

    #[test]
    fn cmpop_variants() {
        let ops = [
            CmpOp::Eq,
            CmpOp::Ne,
            CmpOp::Lt,
            CmpOp::Le,
            CmpOp::Gt,
            CmpOp::Ge,
        ];
        for op in &ops {
            assert_eq!(op.clone(), op.clone());
        }
    }
}

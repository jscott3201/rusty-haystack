//! Expression evaluator with built-in functions.

use std::collections::HashMap;

use super::ast::*;
use super::parser::{ExprError, parse_expr};
use crate::kinds::{Kind, Number};

/// Context providing variable values for expression evaluation.
pub struct ExprContext {
    vars: HashMap<String, Kind>,
}

impl ExprContext {
    /// Create an empty context.
    pub fn new() -> Self {
        Self {
            vars: HashMap::new(),
        }
    }

    /// Set a variable value.
    pub fn set(&mut self, name: impl Into<String>, val: Kind) {
        self.vars.insert(name.into(), val);
    }

    /// Look up a variable by name.
    pub fn get(&self, name: &str) -> Option<&Kind> {
        self.vars.get(name)
    }
}

impl Default for ExprContext {
    fn default() -> Self {
        Self::new()
    }
}

/// A parsed, ready-to-evaluate expression.
pub struct Expr {
    root: ExprNode,
    variables: Vec<String>,
}

impl Expr {
    /// Parse a source string into a ready-to-evaluate expression.
    pub fn parse(source: &str) -> Result<Self, ExprError> {
        let root = parse_expr(source)?;
        let mut variables = Vec::new();
        collect_variables(&root, &mut variables);
        variables.sort();
        variables.dedup();
        Ok(Self { root, variables })
    }

    /// Evaluate the expression, returning `Kind::NA` on any failure.
    pub fn eval(&self, ctx: &ExprContext) -> Kind {
        eval_node(&self.root, ctx)
    }

    /// Evaluate and extract an f64, returning `NaN` on failure.
    pub fn eval_number(&self, ctx: &ExprContext) -> f64 {
        match self.eval(ctx) {
            Kind::Number(n) => n.val,
            _ => f64::NAN,
        }
    }

    /// Evaluate and extract a bool, returning `false` on failure.
    pub fn eval_bool(&self, ctx: &ExprContext) -> bool {
        match self.eval(ctx) {
            Kind::Bool(b) => b,
            _ => false,
        }
    }

    /// Return the sorted, deduplicated list of variables referenced in the expression.
    pub fn variables(&self) -> &[String] {
        &self.variables
    }
}

/// Walk the AST and collect all `Variable` names.
fn collect_variables(node: &ExprNode, out: &mut Vec<String>) {
    match node {
        ExprNode::Variable(name) => out.push(name.clone()),
        ExprNode::Literal(_) => {}
        ExprNode::BinaryOp { left, right, .. } => {
            collect_variables(left, out);
            collect_variables(right, out);
        }
        ExprNode::UnaryOp { operand, .. } => collect_variables(operand, out),
        ExprNode::Comparison { left, right, .. } => {
            collect_variables(left, out);
            collect_variables(right, out);
        }
        ExprNode::Logical { left, right, .. } => {
            collect_variables(left, out);
            collect_variables(right, out);
        }
        ExprNode::FnCall { args, .. } => {
            for arg in args {
                collect_variables(arg, out);
            }
        }
        ExprNode::Conditional {
            cond,
            then_expr,
            else_expr,
        } => {
            collect_variables(cond, out);
            collect_variables(then_expr, out);
            collect_variables(else_expr, out);
        }
    }
}

/// Extract an f64 from a Kind, or return None.
fn as_f64(k: &Kind) -> Option<f64> {
    match k {
        Kind::Number(n) => Some(n.val),
        _ => None,
    }
}

/// Extract a bool from a Kind, or return None.
fn as_bool(k: &Kind) -> Option<bool> {
    match k {
        Kind::Bool(b) => Some(*b),
        _ => None,
    }
}

/// Wrap an f64 into a `Kind::Number`.
fn num(val: f64) -> Kind {
    Kind::Number(Number::unitless(val))
}

/// Evaluate a single AST node.
fn eval_node(node: &ExprNode, ctx: &ExprContext) -> Kind {
    match node {
        ExprNode::Literal(k) => k.clone(),

        ExprNode::Variable(name) => ctx.get(name).cloned().unwrap_or(Kind::NA),

        ExprNode::BinaryOp { left, op, right } => {
            let lv = eval_node(left, ctx);
            let rv = eval_node(right, ctx);
            let (Some(l), Some(r)) = (as_f64(&lv), as_f64(&rv)) else {
                return Kind::NA;
            };
            let result = match op {
                BinOp::Add => l + r,
                BinOp::Sub => l - r,
                BinOp::Mul => l * r,
                BinOp::Div => {
                    if r == 0.0 {
                        return Kind::NA;
                    }
                    l / r
                }
                BinOp::Mod => {
                    if r == 0.0 {
                        return Kind::NA;
                    }
                    l % r
                }
            };
            num(result)
        }

        ExprNode::UnaryOp { op, operand } => {
            let val = eval_node(operand, ctx);
            match op {
                UnOp::Neg => {
                    if let Some(v) = as_f64(&val) {
                        num(-v)
                    } else {
                        Kind::NA
                    }
                }
                UnOp::Not => {
                    if let Some(b) = as_bool(&val) {
                        Kind::Bool(!b)
                    } else {
                        Kind::NA
                    }
                }
            }
        }

        ExprNode::Comparison { left, op, right } => {
            let lv = eval_node(left, ctx);
            let rv = eval_node(right, ctx);
            let (Some(l), Some(r)) = (as_f64(&lv), as_f64(&rv)) else {
                return Kind::NA;
            };
            let result = match op {
                CmpOp::Eq => l == r,
                CmpOp::Ne => l != r,
                CmpOp::Lt => l < r,
                CmpOp::Le => l <= r,
                CmpOp::Gt => l > r,
                CmpOp::Ge => l >= r,
            };
            Kind::Bool(result)
        }

        ExprNode::Logical { left, op, right } => {
            let lv = eval_node(left, ctx);
            let rv = eval_node(right, ctx);
            let (Some(l), Some(r)) = (as_bool(&lv), as_bool(&rv)) else {
                return Kind::NA;
            };
            let result = match op {
                LogicOp::And => l && r,
                LogicOp::Or => l || r,
            };
            Kind::Bool(result)
        }

        ExprNode::FnCall { name, args } => {
            let evaluated: Vec<Kind> = args.iter().map(|a| eval_node(a, ctx)).collect();
            eval_builtin(name, &evaluated)
        }

        ExprNode::Conditional {
            cond,
            then_expr,
            else_expr,
        } => {
            let cv = eval_node(cond, ctx);
            match as_bool(&cv) {
                Some(true) => eval_node(then_expr, ctx),
                Some(false) => eval_node(else_expr, ctx),
                None => Kind::NA,
            }
        }
    }
}

fn finite_num(val: f64) -> Kind {
    if val.is_finite() {
        Kind::Number(Number::unitless(val))
    } else {
        Kind::NA
    }
}

/// Evaluate one of the 7 built-in functions.
fn eval_builtin(name: &str, args: &[Kind]) -> Kind {
    match name {
        "abs" => {
            if args.len() != 1 {
                return Kind::NA;
            }
            as_f64(&args[0]).map_or(Kind::NA, |v| num(v.abs()))
        }
        "min" => {
            if args.len() != 2 {
                return Kind::NA;
            }
            let (Some(a), Some(b)) = (as_f64(&args[0]), as_f64(&args[1])) else {
                return Kind::NA;
            };
            num(a.min(b))
        }
        "max" => {
            if args.len() != 2 {
                return Kind::NA;
            }
            let (Some(a), Some(b)) = (as_f64(&args[0]), as_f64(&args[1])) else {
                return Kind::NA;
            };
            num(a.max(b))
        }
        "sqrt" => {
            if args.len() != 1 {
                return Kind::NA;
            }
            as_f64(&args[0]).map_or(Kind::NA, |v| finite_num(v.sqrt()))
        }
        "clamp" => {
            if args.len() != 3 {
                return Kind::NA;
            }
            let (Some(x), Some(lo), Some(hi)) =
                (as_f64(&args[0]), as_f64(&args[1]), as_f64(&args[2]))
            else {
                return Kind::NA;
            };
            num(x.clamp(lo, hi))
        }
        "avg" => {
            if args.is_empty() {
                return Kind::NA;
            }
            let mut sum = 0.0;
            for a in args {
                match as_f64(a) {
                    Some(v) => sum += v,
                    None => return Kind::NA,
                }
            }
            num(sum / args.len() as f64)
        }
        "between" => {
            if args.len() != 3 {
                return Kind::NA;
            }
            let (Some(x), Some(lo), Some(hi)) =
                (as_f64(&args[0]), as_f64(&args[1]), as_f64(&args[2]))
            else {
                return Kind::NA;
            };
            Kind::Bool(x >= lo && x <= hi)
        }
        _ => Kind::NA,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_with(pairs: &[(&str, Kind)]) -> ExprContext {
        let mut ctx = ExprContext::new();
        for (k, v) in pairs {
            ctx.set(*k, v.clone());
        }
        ctx
    }

    fn n(v: f64) -> Kind {
        Kind::Number(Number::unitless(v))
    }

    // ── Arithmetic ──────────────────────────────────────────

    #[test]
    fn eval_addition() {
        let expr = Expr::parse("1 + 2").unwrap();
        let result = expr.eval(&ExprContext::new());
        assert!(matches!(result, Kind::Number(ref n) if (n.val - 3.0).abs() < 1e-10));
    }

    #[test]
    fn eval_subtraction() {
        let expr = Expr::parse("10 - 3").unwrap();
        assert!(
            matches!(expr.eval(&ExprContext::new()), Kind::Number(ref n) if (n.val - 7.0).abs() < 1e-10)
        );
    }

    #[test]
    fn eval_multiplication() {
        let expr = Expr::parse("4 * 5").unwrap();
        assert_eq!(expr.eval_number(&ExprContext::new()), 20.0);
    }

    #[test]
    fn eval_division() {
        let expr = Expr::parse("10 / 4").unwrap();
        assert_eq!(expr.eval_number(&ExprContext::new()), 2.5);
    }

    #[test]
    fn eval_modulo() {
        let expr = Expr::parse("10 % 3").unwrap();
        assert_eq!(expr.eval_number(&ExprContext::new()), 1.0);
    }

    #[test]
    fn eval_precedence() {
        let expr = Expr::parse("2 + 3 * 4").unwrap();
        assert_eq!(expr.eval_number(&ExprContext::new()), 14.0);
    }

    #[test]
    fn eval_parentheses() {
        let expr = Expr::parse("(2 + 3) * 4").unwrap();
        assert_eq!(expr.eval_number(&ExprContext::new()), 20.0);
    }

    #[test]
    fn eval_negation() {
        let expr = Expr::parse("-5").unwrap();
        assert_eq!(expr.eval_number(&ExprContext::new()), -5.0);
    }

    // ── Comparison ──────────────────────────────────────────

    #[test]
    fn eval_eq_true() {
        let expr = Expr::parse("5 == 5").unwrap();
        assert!(expr.eval_bool(&ExprContext::new()));
    }

    #[test]
    fn eval_eq_false() {
        let expr = Expr::parse("5 == 6").unwrap();
        assert!(!expr.eval_bool(&ExprContext::new()));
    }

    #[test]
    fn eval_ne() {
        let expr = Expr::parse("5 != 6").unwrap();
        assert!(expr.eval_bool(&ExprContext::new()));
    }

    #[test]
    fn eval_lt() {
        let expr = Expr::parse("3 < 5").unwrap();
        assert!(expr.eval_bool(&ExprContext::new()));
    }

    #[test]
    fn eval_le() {
        let expr = Expr::parse("5 <= 5").unwrap();
        assert!(expr.eval_bool(&ExprContext::new()));
    }

    #[test]
    fn eval_gt() {
        let expr = Expr::parse("10 > 5").unwrap();
        assert!(expr.eval_bool(&ExprContext::new()));
    }

    #[test]
    fn eval_ge() {
        let expr = Expr::parse("5 >= 5").unwrap();
        assert!(expr.eval_bool(&ExprContext::new()));
    }

    // ── Logical ─────────────────────────────────────────────

    #[test]
    fn eval_and_true() {
        let expr = Expr::parse("true and true").unwrap();
        assert!(expr.eval_bool(&ExprContext::new()));
    }

    #[test]
    fn eval_and_false() {
        let expr = Expr::parse("true and false").unwrap();
        assert!(!expr.eval_bool(&ExprContext::new()));
    }

    #[test]
    fn eval_or_true() {
        let expr = Expr::parse("false or true").unwrap();
        assert!(expr.eval_bool(&ExprContext::new()));
    }

    #[test]
    fn eval_or_false() {
        let expr = Expr::parse("false or false").unwrap();
        assert!(!expr.eval_bool(&ExprContext::new()));
    }

    #[test]
    fn eval_not() {
        let expr = Expr::parse("!true").unwrap();
        assert!(!expr.eval_bool(&ExprContext::new()));
    }

    #[test]
    fn eval_not_keyword() {
        let expr = Expr::parse("not false").unwrap();
        assert!(expr.eval_bool(&ExprContext::new()));
    }

    // ── Variables ───────────────────────────────────────────

    #[test]
    fn eval_variable() {
        let expr = Expr::parse("$x + 1").unwrap();
        let ctx = ctx_with(&[("x", n(10.0))]);
        assert_eq!(expr.eval_number(&ctx), 11.0);
    }

    #[test]
    fn eval_missing_variable_returns_na() {
        let expr = Expr::parse("$missing").unwrap();
        assert!(matches!(expr.eval(&ExprContext::new()), Kind::NA));
    }

    #[test]
    fn eval_missing_variable_number_returns_nan() {
        let expr = Expr::parse("$missing").unwrap();
        assert!(expr.eval_number(&ExprContext::new()).is_nan());
    }

    #[test]
    fn eval_missing_variable_bool_returns_false() {
        let expr = Expr::parse("$missing").unwrap();
        assert!(!expr.eval_bool(&ExprContext::new()));
    }

    #[test]
    fn variables_collected() {
        let expr = Expr::parse("$a + $b * $a").unwrap();
        assert_eq!(expr.variables(), &["a", "b"]);
    }

    // ── Conditional ─────────────────────────────────────────

    #[test]
    fn eval_conditional_true() {
        let expr = Expr::parse("if true then 1 else 0").unwrap();
        assert_eq!(expr.eval_number(&ExprContext::new()), 1.0);
    }

    #[test]
    fn eval_conditional_false() {
        let expr = Expr::parse("if false then 1 else 0").unwrap();
        assert_eq!(expr.eval_number(&ExprContext::new()), 0.0);
    }

    #[test]
    fn eval_conditional_with_variable() {
        let expr = Expr::parse("if $flag then $a else $b").unwrap();
        let ctx = ctx_with(&[("flag", Kind::Bool(true)), ("a", n(42.0)), ("b", n(99.0))]);
        assert_eq!(expr.eval_number(&ctx), 42.0);
    }

    // ── Built-in functions ──────────────────────────────────

    #[test]
    fn fn_abs() {
        let expr = Expr::parse("abs(-7)").unwrap();
        assert_eq!(expr.eval_number(&ExprContext::new()), 7.0);
    }

    #[test]
    fn fn_abs_positive() {
        let expr = Expr::parse("abs(3)").unwrap();
        assert_eq!(expr.eval_number(&ExprContext::new()), 3.0);
    }

    #[test]
    fn fn_min() {
        let expr = Expr::parse("min(3, 7)").unwrap();
        assert_eq!(expr.eval_number(&ExprContext::new()), 3.0);
    }

    #[test]
    fn fn_max() {
        let expr = Expr::parse("max(3, 7)").unwrap();
        assert_eq!(expr.eval_number(&ExprContext::new()), 7.0);
    }

    #[test]
    fn fn_sqrt() {
        let expr = Expr::parse("sqrt(16)").unwrap();
        assert_eq!(expr.eval_number(&ExprContext::new()), 4.0);
    }

    #[test]
    fn fn_clamp() {
        let expr = Expr::parse("clamp(15, 0, 10)").unwrap();
        assert_eq!(expr.eval_number(&ExprContext::new()), 10.0);
    }

    #[test]
    fn fn_clamp_within() {
        let expr = Expr::parse("clamp(5, 0, 10)").unwrap();
        assert_eq!(expr.eval_number(&ExprContext::new()), 5.0);
    }

    #[test]
    fn fn_clamp_below() {
        let expr = Expr::parse("clamp(-5, 0, 10)").unwrap();
        assert_eq!(expr.eval_number(&ExprContext::new()), 0.0);
    }

    #[test]
    fn fn_avg_two() {
        let expr = Expr::parse("avg(4, 6)").unwrap();
        assert_eq!(expr.eval_number(&ExprContext::new()), 5.0);
    }

    #[test]
    fn fn_avg_three() {
        let expr = Expr::parse("avg(2, 4, 6)").unwrap();
        assert_eq!(expr.eval_number(&ExprContext::new()), 4.0);
    }

    #[test]
    fn fn_between_inside() {
        let expr = Expr::parse("between(5, 0, 10)").unwrap();
        assert!(expr.eval_bool(&ExprContext::new()));
    }

    #[test]
    fn fn_between_outside() {
        let expr = Expr::parse("between(15, 0, 10)").unwrap();
        assert!(!expr.eval_bool(&ExprContext::new()));
    }

    #[test]
    fn fn_between_boundary() {
        let expr = Expr::parse("between(0, 0, 10)").unwrap();
        assert!(expr.eval_bool(&ExprContext::new()));
    }

    // ── Type mismatch → NA ──────────────────────────────────

    #[test]
    fn type_mismatch_add_str() {
        let expr = Expr::parse(r#""hello" + 1"#).unwrap();
        assert!(matches!(expr.eval(&ExprContext::new()), Kind::NA));
    }

    #[test]
    fn type_mismatch_compare_str() {
        let expr = Expr::parse(r#""hello" > 1"#).unwrap();
        assert!(matches!(expr.eval(&ExprContext::new()), Kind::NA));
    }

    #[test]
    fn type_mismatch_logical_number() {
        let expr = Expr::parse("1 and 2").unwrap();
        assert!(matches!(expr.eval(&ExprContext::new()), Kind::NA));
    }

    #[test]
    fn type_mismatch_neg_bool() {
        let expr = Expr::parse("-true").unwrap();
        assert!(matches!(expr.eval(&ExprContext::new()), Kind::NA));
    }

    #[test]
    fn type_mismatch_not_number() {
        let expr = Expr::parse("!5").unwrap();
        assert!(matches!(expr.eval(&ExprContext::new()), Kind::NA));
    }

    #[test]
    fn fn_wrong_arity() {
        let expr = Expr::parse("abs(1, 2)").unwrap();
        assert!(matches!(expr.eval(&ExprContext::new()), Kind::NA));
    }

    #[test]
    fn fn_unknown_returns_na() {
        let expr = Expr::parse("unknown(1)").unwrap();
        assert!(matches!(expr.eval(&ExprContext::new()), Kind::NA));
    }

    #[test]
    fn test_division_by_zero() {
        let expr = Expr::parse("$x / $y").unwrap();
        let mut ctx = ExprContext::new();
        ctx.set("x", Kind::Number(Number::unitless(10.0)));
        ctx.set("y", Kind::Number(Number::unitless(0.0)));
        assert_eq!(expr.eval(&ctx), Kind::NA);
    }

    #[test]
    fn test_modulo_by_zero() {
        let expr = Expr::parse("$x % $y").unwrap();
        let mut ctx = ExprContext::new();
        ctx.set("x", Kind::Number(Number::unitless(10.0)));
        ctx.set("y", Kind::Number(Number::unitless(0.0)));
        assert_eq!(expr.eval(&ctx), Kind::NA);
    }

    #[test]
    fn test_sqrt_negative() {
        let expr = Expr::parse("sqrt($x)").unwrap();
        let mut ctx = ExprContext::new();
        ctx.set("x", Kind::Number(Number::unitless(-1.0)));
        assert_eq!(expr.eval(&ctx), Kind::NA);
    }

    #[test]
    fn fn_avg_no_args_returns_na() {
        let expr = Expr::parse("avg()").unwrap();
        assert!(matches!(expr.eval(&ExprContext::new()), Kind::NA));
    }

    #[test]
    fn conditional_non_bool_returns_na() {
        let expr = Expr::parse("if 1 then 2 else 3").unwrap();
        assert!(matches!(expr.eval(&ExprContext::new()), Kind::NA));
    }

    // ── ExprContext ─────────────────────────────────────────

    #[test]
    fn context_set_and_get() {
        let mut ctx = ExprContext::new();
        ctx.set("x", n(42.0));
        assert!(matches!(ctx.get("x"), Some(Kind::Number(n)) if (n.val - 42.0).abs() < 1e-10));
        assert!(ctx.get("y").is_none());
    }

    #[test]
    fn context_default() {
        let ctx = ExprContext::default();
        assert!(ctx.get("anything").is_none());
    }

    // ── Complex expressions ─────────────────────────────────

    #[test]
    fn complex_expression() {
        // clamp($temp, 0, 100) > 50 and $enabled
        let expr = Expr::parse("clamp($temp, 0, 100) > 50 and $enabled").unwrap();
        let ctx = ctx_with(&[("temp", n(75.0)), ("enabled", Kind::Bool(true))]);
        assert!(expr.eval_bool(&ctx));
    }

    #[test]
    fn complex_conditional_expression() {
        let expr = Expr::parse("if $x > 10 then $x * 2 else $x + 1").unwrap();
        let ctx = ctx_with(&[("x", n(15.0))]);
        assert_eq!(expr.eval_number(&ctx), 30.0);
    }

    #[test]
    fn nested_function_expression() {
        let expr = Expr::parse("max(abs(-3), sqrt(4))").unwrap();
        assert_eq!(expr.eval_number(&ExprContext::new()), 3.0);
    }
}

//! Hand-written recursive descent parser for expressions.

use super::ast::*;
use crate::kinds::{Kind, Number};

/// Error produced when parsing an expression fails.
#[derive(Debug)]
pub struct ExprError {
    /// Human-readable error message.
    pub msg: String,
    /// Byte position in the source where the error occurred.
    pub pos: usize,
}

impl std::fmt::Display for ExprError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "expr error at {}: {}", self.pos, self.msg)
    }
}

impl std::error::Error for ExprError {}

/// Internal parser state.
struct Parser<'a> {
    source: &'a str,
    pos: usize,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            pos: 0,
            depth: 0,
        }
    }

    fn err(&self, msg: impl Into<String>) -> ExprError {
        ExprError {
            msg: msg.into(),
            pos: self.pos,
        }
    }

    fn enter(&mut self) -> Result<(), ExprError> {
        self.depth += 1;
        if self.depth > MAX_EXPR_DEPTH {
            Err(self.err("expression exceeds maximum nesting depth"))
        } else {
            Ok(())
        }
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    fn skip_ws(&mut self) {
        while self.pos < self.source.len() {
            let b = self.source.as_bytes()[self.pos];
            if b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.source.len()
    }

    fn peek(&self) -> Option<u8> {
        if self.pos < self.source.len() {
            Some(self.source.as_bytes()[self.pos])
        } else {
            None
        }
    }

    fn consume(&mut self, ch: u8) -> Result<(), ExprError> {
        self.skip_ws();
        if self.peek() == Some(ch) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.err(format!("expected '{}'", ch as char)))
        }
    }

    fn starts_with(&self, s: &str) -> bool {
        self.source[self.pos..].starts_with(s)
    }

    /// Check if identifier keyword `kw` starts at current position and is
    /// followed by a non-identifier character.
    fn keyword(&self, kw: &str) -> bool {
        if !self.starts_with(kw) {
            return false;
        }
        let after = self.pos + kw.len();
        if after >= self.source.len() {
            return true;
        }
        let b = self.source.as_bytes()[after];
        !b.is_ascii_alphanumeric() && b != b'_'
    }

    fn consume_keyword(&mut self, kw: &str) -> Result<(), ExprError> {
        self.skip_ws();
        if self.keyword(kw) {
            self.pos += kw.len();
            Ok(())
        } else {
            Err(self.err(format!("expected '{kw}'")))
        }
    }

    fn read_ident(&mut self) -> Result<String, ExprError> {
        self.skip_ws();
        let start = self.pos;
        while self.pos < self.source.len() {
            let b = self.source.as_bytes()[self.pos];
            if b.is_ascii_alphanumeric() || b == b'_' {
                self.pos += 1;
            } else {
                break;
            }
        }
        if self.pos == start {
            return Err(self.err("expected identifier"));
        }
        Ok(self.source[start..self.pos].to_string())
    }

    // ── Grammar rules ──────────────────────────────────────────────

    fn parse_expr(&mut self) -> Result<ExprNode, ExprError> {
        self.enter()?;
        let node = self.parse_logic_or()?;
        self.leave();
        Ok(node)
    }

    fn parse_logic_or(&mut self) -> Result<ExprNode, ExprError> {
        let mut left = self.parse_logic_and()?;
        loop {
            self.skip_ws();
            if self.keyword("or") {
                self.pos += 2;
                let right = self.parse_logic_and()?;
                left = ExprNode::Logical {
                    left: Box::new(left),
                    op: LogicOp::Or,
                    right: Box::new(right),
                };
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_logic_and(&mut self) -> Result<ExprNode, ExprError> {
        let mut left = self.parse_comparison()?;
        loop {
            self.skip_ws();
            if self.keyword("and") {
                self.pos += 3;
                let right = self.parse_comparison()?;
                left = ExprNode::Logical {
                    left: Box::new(left),
                    op: LogicOp::And,
                    right: Box::new(right),
                };
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_comparison(&mut self) -> Result<ExprNode, ExprError> {
        let left = self.parse_additive()?;
        self.skip_ws();
        let op = if self.starts_with("!=") {
            self.pos += 2;
            Some(CmpOp::Ne)
        } else if self.starts_with("==") {
            self.pos += 2;
            Some(CmpOp::Eq)
        } else if self.starts_with("<=") {
            self.pos += 2;
            Some(CmpOp::Le)
        } else if self.starts_with(">=") {
            self.pos += 2;
            Some(CmpOp::Ge)
        } else if self.peek() == Some(b'<') {
            self.pos += 1;
            Some(CmpOp::Lt)
        } else if self.peek() == Some(b'>') {
            self.pos += 1;
            Some(CmpOp::Gt)
        } else {
            None
        };
        if let Some(op) = op {
            let right = self.parse_additive()?;
            Ok(ExprNode::Comparison {
                left: Box::new(left),
                op,
                right: Box::new(right),
            })
        } else {
            Ok(left)
        }
    }

    fn parse_additive(&mut self) -> Result<ExprNode, ExprError> {
        let mut left = self.parse_multiplicative()?;
        loop {
            self.skip_ws();
            match self.peek() {
                Some(b'+') => {
                    self.pos += 1;
                    let right = self.parse_multiplicative()?;
                    left = ExprNode::BinaryOp {
                        left: Box::new(left),
                        op: BinOp::Add,
                        right: Box::new(right),
                    };
                }
                Some(b'-') => {
                    self.pos += 1;
                    let right = self.parse_multiplicative()?;
                    left = ExprNode::BinaryOp {
                        left: Box::new(left),
                        op: BinOp::Sub,
                        right: Box::new(right),
                    };
                }
                _ => break,
            }
        }
        Ok(left)
    }

    fn parse_multiplicative(&mut self) -> Result<ExprNode, ExprError> {
        let mut left = self.parse_unary()?;
        loop {
            self.skip_ws();
            match self.peek() {
                Some(b'*') => {
                    self.pos += 1;
                    let right = self.parse_unary()?;
                    left = ExprNode::BinaryOp {
                        left: Box::new(left),
                        op: BinOp::Mul,
                        right: Box::new(right),
                    };
                }
                Some(b'/') => {
                    self.pos += 1;
                    let right = self.parse_unary()?;
                    left = ExprNode::BinaryOp {
                        left: Box::new(left),
                        op: BinOp::Div,
                        right: Box::new(right),
                    };
                }
                Some(b'%') => {
                    self.pos += 1;
                    let right = self.parse_unary()?;
                    left = ExprNode::BinaryOp {
                        left: Box::new(left),
                        op: BinOp::Mod,
                        right: Box::new(right),
                    };
                }
                _ => break,
            }
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<ExprNode, ExprError> {
        self.skip_ws();
        if self.peek() == Some(b'-') {
            self.pos += 1;
            let operand = self.parse_unary()?;
            return Ok(ExprNode::UnaryOp {
                op: UnOp::Neg,
                operand: Box::new(operand),
            });
        }
        if self.peek() == Some(b'!') {
            self.pos += 1;
            let operand = self.parse_unary()?;
            return Ok(ExprNode::UnaryOp {
                op: UnOp::Not,
                operand: Box::new(operand),
            });
        }
        if self.keyword("not") {
            self.pos += 3;
            let operand = self.parse_unary()?;
            return Ok(ExprNode::UnaryOp {
                op: UnOp::Not,
                operand: Box::new(operand),
            });
        }
        self.parse_call()
    }

    fn parse_call(&mut self) -> Result<ExprNode, ExprError> {
        self.skip_ws();
        let start = self.pos;

        // Try to parse an identifier that could be a function name.
        // We need to check if the next char starts an identifier (alpha/underscore)
        // and is NOT a keyword (true/false/null/if/not) followed by '('.
        if self.pos < self.source.len() {
            let b = self.source.as_bytes()[self.pos];
            if (b.is_ascii_alphabetic() || b == b'_')
                && !self.keyword("true")
                && !self.keyword("false")
                && !self.keyword("null")
                && !self.keyword("if")
                && !self.keyword("not")
            {
                let name = self.read_ident()?;
                self.skip_ws();
                if self.peek() == Some(b'(') {
                    self.pos += 1;
                    let mut args = Vec::new();
                    self.skip_ws();
                    if self.peek() != Some(b')') {
                        args.push(self.parse_expr()?);
                        loop {
                            self.skip_ws();
                            if self.peek() == Some(b',') {
                                self.pos += 1;
                                args.push(self.parse_expr()?);
                            } else {
                                break;
                            }
                        }
                    }
                    self.consume(b')')?;
                    return Ok(ExprNode::FnCall { name, args });
                }
                // Not a function call — backtrack.
                self.pos = start;
            }
        }

        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<ExprNode, ExprError> {
        self.skip_ws();

        if self.at_end() {
            return Err(self.err("unexpected end of expression"));
        }

        // Number literal
        let b = self.source.as_bytes()[self.pos];
        if b.is_ascii_digit()
            || (b == b'.'
                && self.pos + 1 < self.source.len()
                && self.source.as_bytes()[self.pos + 1].is_ascii_digit())
        {
            return self.parse_number();
        }

        // String literal
        if b == b'"' {
            return self.parse_string();
        }

        // true / false
        if self.keyword("true") {
            self.pos += 4;
            return Ok(ExprNode::Literal(Kind::Bool(true)));
        }
        if self.keyword("false") {
            self.pos += 5;
            return Ok(ExprNode::Literal(Kind::Bool(false)));
        }

        // null
        if self.keyword("null") {
            self.pos += 4;
            return Ok(ExprNode::Literal(Kind::Null));
        }

        // Variable: $ident
        if b == b'$' {
            self.pos += 1;
            let name = self.read_ident()?;
            return Ok(ExprNode::Variable(name));
        }

        // Parenthesised expression
        if b == b'(' {
            self.pos += 1;
            let node = self.parse_expr()?;
            self.consume(b')')?;
            return Ok(node);
        }

        // Conditional: if expr then expr else expr
        if self.keyword("if") {
            self.pos += 2;
            let cond = self.parse_expr()?;
            self.consume_keyword("then")?;
            let then_expr = self.parse_expr()?;
            self.consume_keyword("else")?;
            let else_expr = self.parse_expr()?;
            return Ok(ExprNode::Conditional {
                cond: Box::new(cond),
                then_expr: Box::new(then_expr),
                else_expr: Box::new(else_expr),
            });
        }

        Err(self.err(format!("unexpected character '{}'", b as char)))
    }

    fn parse_number(&mut self) -> Result<ExprNode, ExprError> {
        let start = self.pos;
        while self.pos < self.source.len() && self.source.as_bytes()[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        if self.pos < self.source.len() && self.source.as_bytes()[self.pos] == b'.' {
            self.pos += 1;
            while self.pos < self.source.len() && self.source.as_bytes()[self.pos].is_ascii_digit()
            {
                self.pos += 1;
            }
        }
        let s = &self.source[start..self.pos];
        let val: f64 = s
            .parse()
            .map_err(|_| self.err(format!("invalid number '{s}'")))?;
        Ok(ExprNode::Literal(Kind::Number(Number::unitless(val))))
    }

    fn parse_string(&mut self) -> Result<ExprNode, ExprError> {
        self.pos += 1; // skip opening "
        let start = self.pos;
        while self.pos < self.source.len() && self.source.as_bytes()[self.pos] != b'"' {
            if self.source.as_bytes()[self.pos] == b'\\' {
                self.pos += 1;
                if self.pos >= self.source.len() {
                    return Err(self.err("unterminated string escape"));
                }
                match self.source.as_bytes()[self.pos] {
                    b'"' | b'\\' | b'n' | b't' | b'r' => {}
                    ch => {
                        return Err(self.err(format!("invalid escape sequence: \\{}", ch as char)));
                    }
                }
            }
            self.pos += 1;
        }
        if self.pos >= self.source.len() {
            return Err(self.err("unterminated string"));
        }
        let s = self.source[start..self.pos].to_string();
        self.pos += 1; // skip closing "
        Ok(ExprNode::Literal(Kind::Str(s)))
    }
}

/// Parse an expression source string into an AST.
pub fn parse_expr(source: &str) -> Result<ExprNode, ExprError> {
    if source.len() > MAX_EXPR_SOURCE {
        return Err(ExprError {
            msg: format!("expression source exceeds maximum length of {MAX_EXPR_SOURCE} bytes"),
            pos: 0,
        });
    }
    let mut parser = Parser::new(source);
    let node = parser.parse_expr()?;
    parser.skip_ws();
    if !parser.at_end() {
        return Err(parser.err("unexpected trailing input"));
    }
    Ok(node)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_number_literal() {
        let node = parse_expr("42").unwrap();
        assert!(matches!(node, ExprNode::Literal(Kind::Number(_))));
    }

    #[test]
    fn parse_float_literal() {
        let node = parse_expr("3.14").unwrap();
        if let ExprNode::Literal(Kind::Number(n)) = &node {
            // Test that the parser produces a value close to 3.14
            assert!(n.val > 3.13 && n.val < 3.15);
        } else {
            panic!("expected number literal");
        }
    }

    #[test]
    fn parse_string_literal() {
        let node = parse_expr(r#""hello""#).unwrap();
        assert!(matches!(node, ExprNode::Literal(Kind::Str(s)) if s == "hello"));
    }

    #[test]
    fn parse_bool_true() {
        let node = parse_expr("true").unwrap();
        assert!(matches!(node, ExprNode::Literal(Kind::Bool(true))));
    }

    #[test]
    fn parse_bool_false() {
        let node = parse_expr("false").unwrap();
        assert!(matches!(node, ExprNode::Literal(Kind::Bool(false))));
    }

    #[test]
    fn parse_null() {
        let node = parse_expr("null").unwrap();
        assert!(matches!(node, ExprNode::Literal(Kind::Null)));
    }

    #[test]
    fn parse_variable() {
        let node = parse_expr("$temp").unwrap();
        assert!(matches!(node, ExprNode::Variable(ref s) if s == "temp"));
    }

    #[test]
    fn parse_addition() {
        let node = parse_expr("1 + 2").unwrap();
        assert!(matches!(node, ExprNode::BinaryOp { op: BinOp::Add, .. }));
    }

    #[test]
    fn parse_arithmetic_precedence() {
        // 1 + 2 * 3 should be 1 + (2 * 3)
        let node = parse_expr("1 + 2 * 3").unwrap();
        if let ExprNode::BinaryOp {
            op: BinOp::Add,
            right,
            ..
        } = &node
        {
            assert!(matches!(
                right.as_ref(),
                ExprNode::BinaryOp { op: BinOp::Mul, .. }
            ));
        } else {
            panic!("expected Add at top");
        }
    }

    #[test]
    fn parse_subtraction() {
        let node = parse_expr("5 - 3").unwrap();
        assert!(matches!(node, ExprNode::BinaryOp { op: BinOp::Sub, .. }));
    }

    #[test]
    fn parse_division() {
        let node = parse_expr("10 / 2").unwrap();
        assert!(matches!(node, ExprNode::BinaryOp { op: BinOp::Div, .. }));
    }

    #[test]
    fn parse_modulo() {
        let node = parse_expr("10 % 3").unwrap();
        assert!(matches!(node, ExprNode::BinaryOp { op: BinOp::Mod, .. }));
    }

    #[test]
    fn parse_unary_neg() {
        let node = parse_expr("-5").unwrap();
        assert!(matches!(node, ExprNode::UnaryOp { op: UnOp::Neg, .. }));
    }

    #[test]
    fn parse_unary_not_bang() {
        let node = parse_expr("!true").unwrap();
        assert!(matches!(node, ExprNode::UnaryOp { op: UnOp::Not, .. }));
    }

    #[test]
    fn parse_unary_not_keyword() {
        let node = parse_expr("not false").unwrap();
        assert!(matches!(node, ExprNode::UnaryOp { op: UnOp::Not, .. }));
    }

    #[test]
    fn parse_comparison_eq() {
        let node = parse_expr("$x == 5").unwrap();
        assert!(matches!(node, ExprNode::Comparison { op: CmpOp::Eq, .. }));
    }

    #[test]
    fn parse_comparison_ne() {
        let node = parse_expr("$x != 5").unwrap();
        assert!(matches!(node, ExprNode::Comparison { op: CmpOp::Ne, .. }));
    }

    #[test]
    fn parse_comparison_lt() {
        let node = parse_expr("$x < 10").unwrap();
        assert!(matches!(node, ExprNode::Comparison { op: CmpOp::Lt, .. }));
    }

    #[test]
    fn parse_comparison_le() {
        let node = parse_expr("$x <= 10").unwrap();
        assert!(matches!(node, ExprNode::Comparison { op: CmpOp::Le, .. }));
    }

    #[test]
    fn parse_comparison_gt() {
        let node = parse_expr("$x > 0").unwrap();
        assert!(matches!(node, ExprNode::Comparison { op: CmpOp::Gt, .. }));
    }

    #[test]
    fn parse_comparison_ge() {
        let node = parse_expr("$x >= 0").unwrap();
        assert!(matches!(node, ExprNode::Comparison { op: CmpOp::Ge, .. }));
    }

    #[test]
    fn parse_logical_and() {
        let node = parse_expr("true and false").unwrap();
        assert!(matches!(
            node,
            ExprNode::Logical {
                op: LogicOp::And,
                ..
            }
        ));
    }

    #[test]
    fn parse_logical_or() {
        let node = parse_expr("true or false").unwrap();
        assert!(matches!(
            node,
            ExprNode::Logical {
                op: LogicOp::Or,
                ..
            }
        ));
    }

    #[test]
    fn parse_fn_call_one_arg() {
        let node = parse_expr("abs(-5)").unwrap();
        if let ExprNode::FnCall { name, args } = &node {
            assert_eq!(name, "abs");
            assert_eq!(args.len(), 1);
        } else {
            panic!("expected FnCall");
        }
    }

    #[test]
    fn parse_fn_call_two_args() {
        let node = parse_expr("min(1, 2)").unwrap();
        if let ExprNode::FnCall { name, args } = &node {
            assert_eq!(name, "min");
            assert_eq!(args.len(), 2);
        } else {
            panic!("expected FnCall");
        }
    }

    #[test]
    fn parse_fn_call_no_args() {
        let node = parse_expr("foo()").unwrap();
        if let ExprNode::FnCall { name, args } = &node {
            assert_eq!(name, "foo");
            assert!(args.is_empty());
        } else {
            panic!("expected FnCall");
        }
    }

    #[test]
    fn parse_conditional() {
        let node = parse_expr("if true then 1 else 0").unwrap();
        assert!(matches!(node, ExprNode::Conditional { .. }));
    }

    #[test]
    fn parse_parenthesised() {
        let node = parse_expr("(1 + 2) * 3").unwrap();
        assert!(matches!(node, ExprNode::BinaryOp { op: BinOp::Mul, .. }));
    }

    #[test]
    fn parse_complex_expression() {
        let node = parse_expr("$a + $b * 2 > 10 and $c != 0").unwrap();
        assert!(matches!(
            node,
            ExprNode::Logical {
                op: LogicOp::And,
                ..
            }
        ));
    }

    #[test]
    fn error_empty_input() {
        let err = parse_expr("").unwrap_err();
        assert!(err.msg.contains("unexpected end"));
    }

    #[test]
    fn error_trailing_input() {
        let err = parse_expr("1 2").unwrap_err();
        assert!(err.msg.contains("trailing"));
    }

    #[test]
    fn error_unterminated_string() {
        let err = parse_expr(r#""hello"#).unwrap_err();
        assert!(err.msg.contains("unterminated"));
    }

    #[test]
    fn error_source_too_long() {
        let long = "1+".repeat(MAX_EXPR_SOURCE);
        let err = parse_expr(&long).unwrap_err();
        assert!(err.msg.contains("maximum length"));
    }

    #[test]
    fn error_depth_exceeded() {
        // Build deeply nested parens: ((((...))))
        let open: String = "(".repeat(MAX_EXPR_DEPTH + 10);
        let close: String = ")".repeat(MAX_EXPR_DEPTH + 10);
        let src = format!("{open}1{close}");
        let err = parse_expr(&src).unwrap_err();
        assert!(err.msg.contains("depth"));
    }

    #[test]
    fn error_display() {
        let err = ExprError {
            msg: "bad".into(),
            pos: 5,
        };
        assert_eq!(err.to_string(), "expr error at 5: bad");
    }

    #[test]
    fn parse_nested_fn_calls() {
        let node = parse_expr("max(abs(-1), min(2, 3))").unwrap();
        if let ExprNode::FnCall { name, args } = &node {
            assert_eq!(name, "max");
            assert_eq!(args.len(), 2);
        } else {
            panic!("expected FnCall");
        }
    }

    #[test]
    fn parse_logical_precedence() {
        // `a or b and c` should be `a or (b and c)` since and binds tighter
        let node = parse_expr("true or false and true").unwrap();
        if let ExprNode::Logical {
            op: LogicOp::Or,
            right,
            ..
        } = &node
        {
            assert!(matches!(
                right.as_ref(),
                ExprNode::Logical {
                    op: LogicOp::And,
                    ..
                }
            ));
        } else {
            panic!("expected Or at top");
        }
    }
}

// Hand-written recursive descent parser for Haystack filter expressions.
//
// Grammar:
//   filter     := condOr
//   condOr     := condAnd ("or" condAnd)*
//   condAnd    := term ("and" term)*
//   term       := parens | missing | cmp_or_has | specMatch
//   parens     := "(" filter ")"
//   missing    := "not" path
//   cmp_or_has := path (cmpOp val)?
//   path       := name ("->" name)*
//   cmpOp      := "==" | "!=" | "<" | "<=" | ">" | ">="
//   val        := <zinc scalar literal>
//   specMatch  := qualified_name (contains "::")
//   name       := [a-zA-Z][a-zA-Z0-9_]*

use super::ast::{CmpOp, FilterNode, Path};
use crate::codecs::zinc::ZincParser;
use crate::kinds::Kind;

/// Errors that can occur during filter parsing.
#[derive(Debug, thiserror::Error)]
pub enum FilterError {
    #[error("filter parse error at position {pos}: {message}")]
    Parse { pos: usize, message: String },
}

/// Parse a filter expression string into a FilterNode AST.
pub fn parse_filter(expr: &str) -> Result<FilterNode, FilterError> {
    let mut parser = FilterParser::new(expr);
    parser.skip_spaces();
    if parser.at_end() {
        return Err(parser.err("empty filter expression"));
    }
    let node = parser.parse_cond_or()?;
    parser.skip_spaces();
    if !parser.at_end() {
        return Err(parser.err(format!(
            "unexpected trailing input: '{}'",
            &parser.src[parser.pos..]
        )));
    }
    Ok(node)
}

// ── Internal parser state ──

struct FilterParser<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> FilterParser<'a> {
    fn new(src: &'a str) -> Self {
        Self { src, pos: 0 }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.src.len()
    }

    fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn consume(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }

    fn skip_spaces(&mut self) {
        while let Some(ch) = self.peek() {
            if ch == ' ' || ch == '\t' {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn err(&self, msg: impl Into<String>) -> FilterError {
        FilterError::Parse {
            pos: self.pos,
            message: msg.into(),
        }
    }

    /// Check if the upcoming text matches the given keyword and is followed
    /// by a word boundary (not an alphanumeric or underscore character).
    fn at_keyword(&self, kw: &str) -> bool {
        let remaining = &self.src[self.pos..];
        if !remaining.starts_with(kw) {
            return false;
        }
        // Check word boundary
        let after = remaining[kw.len()..].chars().next();
        match after {
            None => true,
            Some(c) => !c.is_alphanumeric() && c != '_',
        }
    }

    /// Consume a keyword if it matches at the current position.
    fn consume_keyword(&mut self, kw: &str) -> bool {
        if self.at_keyword(kw) {
            self.pos += kw.len();
            true
        } else {
            false
        }
    }

    /// Read an identifier: [a-zA-Z][a-zA-Z0-9_]*
    fn read_name(&mut self) -> Option<String> {
        let start = self.pos;
        match self.peek() {
            Some(c) if c.is_ascii_alphabetic() => {
                self.pos += c.len_utf8();
            }
            _ => return None,
        }
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == '_' {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
        Some(self.src[start..self.pos].to_string())
    }

    /// Read a path: name ("->" name)*
    fn read_path(&mut self) -> Result<Path, FilterError> {
        let first = self
            .read_name()
            .ok_or_else(|| self.err("expected tag name"))?;
        let mut segments = vec![first];

        while self.try_consume_str("->") {
            let seg = self
                .read_name()
                .ok_or_else(|| self.err("expected tag name after '->'"))?;
            segments.push(seg);
        }

        Ok(Path(segments))
    }

    /// Try to consume a literal string at the current position.
    fn try_consume_str(&mut self, s: &str) -> bool {
        if self.src[self.pos..].starts_with(s) {
            self.pos += s.len();
            true
        } else {
            false
        }
    }

    /// Read a comparison operator.
    fn read_cmp_op(&mut self) -> Option<CmpOp> {
        self.skip_spaces();
        // Order matters: check two-char ops before single-char ops
        if self.try_consume_str("==") {
            Some(CmpOp::Eq)
        } else if self.try_consume_str("!=") {
            Some(CmpOp::Ne)
        } else if self.try_consume_str("<=") {
            Some(CmpOp::Le)
        } else if self.try_consume_str(">=") {
            Some(CmpOp::Ge)
        } else if self.try_consume_str("<") {
            Some(CmpOp::Lt)
        } else if self.try_consume_str(">") {
            Some(CmpOp::Gt)
        } else {
            None
        }
    }

    /// Read a Zinc scalar value using the ZincParser.
    fn read_val(&mut self) -> Result<Kind, FilterError> {
        self.skip_spaces();
        // Use ZincParser to parse the value from the current position
        let remaining = &self.src[self.pos..];
        let mut zinc = ZincParser::new(remaining);
        let val = zinc.read_val().map_err(|e| FilterError::Parse {
            pos: self.pos,
            message: format!("invalid value: {e}"),
        })?;
        // Advance our position by however far the zinc parser consumed
        self.pos += zinc.pos();
        Ok(val)
    }

    // ── Recursive descent productions ──

    /// condOr := condAnd ("or" condAnd)*
    fn parse_cond_or(&mut self) -> Result<FilterNode, FilterError> {
        let mut left = self.parse_cond_and()?;
        loop {
            self.skip_spaces();
            if self.consume_keyword("or") {
                self.skip_spaces();
                let right = self.parse_cond_and()?;
                left = FilterNode::Or(Box::new(left), Box::new(right));
            } else {
                break;
            }
        }
        Ok(left)
    }

    /// condAnd := term ("and" term)*
    fn parse_cond_and(&mut self) -> Result<FilterNode, FilterError> {
        let mut left = self.parse_term()?;
        loop {
            self.skip_spaces();
            if self.consume_keyword("and") {
                self.skip_spaces();
                let right = self.parse_term()?;
                left = FilterNode::And(Box::new(left), Box::new(right));
            } else {
                break;
            }
        }
        Ok(left)
    }

    /// term := parens | missing | cmp_or_has | specMatch
    fn parse_term(&mut self) -> Result<FilterNode, FilterError> {
        self.skip_spaces();

        // Parenthesized expression
        if self.peek() == Some('(') {
            self.consume(); // eat '('
            self.skip_spaces();
            let inner = self.parse_cond_or()?;
            self.skip_spaces();
            if self.peek() != Some(')') {
                return Err(self.err("expected closing ')'"));
            }
            self.consume(); // eat ')'
            return Ok(inner);
        }

        // "not" keyword → Missing
        if self.at_keyword("not") {
            self.consume_keyword("not");
            self.skip_spaces();
            let path = self.read_path()?;
            return Ok(FilterNode::Missing(path));
        }

        // Try to read a name; might be Has, Cmp, or SpecMatch
        let save_pos = self.pos;
        match self.read_name() {
            Some(first_name) => {
                // Check if this is a SpecMatch (contains "::")
                if self.try_consume_str("::") {
                    // Read the rest of the spec match
                    let mut spec = first_name;
                    spec.push_str("::");
                    // Read the type name after ::
                    match self.read_name() {
                        Some(type_name) => {
                            spec.push_str(&type_name);
                            return Ok(FilterNode::SpecMatch(spec));
                        }
                        None => {
                            return Err(self.err("expected type name after '::'"));
                        }
                    }
                }

                // Check for qualified name with dots before :: (e.g. ph.equips::Ahu)
                let mut full_name = first_name.clone();
                let mut dot_pos = self.pos;
                while self.peek() == Some('.') {
                    self.consume(); // eat '.'
                    match self.read_name() {
                        Some(seg) => {
                            full_name.push('.');
                            full_name.push_str(&seg);
                            dot_pos = self.pos;
                        }
                        None => {
                            // Not a dotted name, backtrack
                            self.pos = dot_pos;
                            break;
                        }
                    }
                    // Check if this dotted name is followed by ::
                    if self.try_consume_str("::") {
                        let mut spec = full_name;
                        spec.push_str("::");
                        match self.read_name() {
                            Some(type_name) => {
                                spec.push_str(&type_name);
                                return Ok(FilterNode::SpecMatch(spec));
                            }
                            None => {
                                return Err(self.err("expected type name after '::'"));
                            }
                        }
                    }
                }

                // If we consumed dots but no ::, backtrack to just after the first name
                if full_name.contains('.') {
                    self.pos = save_pos + first_name.len();
                }

                // Build path: we have the first name, now check for "->" continuations
                let mut segments = vec![first_name];
                while self.try_consume_str("->") {
                    let seg = self
                        .read_name()
                        .ok_or_else(|| self.err("expected tag name after '->'"))?;
                    segments.push(seg);
                }
                let path = Path(segments);

                // Check for comparison operator
                let pre_op_pos = self.pos;
                if let Some(op) = self.read_cmp_op() {
                    let val = self.read_val()?;
                    Ok(FilterNode::Cmp { path, op, val })
                } else {
                    // No comparison operator → Has
                    self.pos = pre_op_pos;
                    Ok(FilterNode::Has(path))
                }
            }
            None => Err(self.err("expected tag name, 'not', or '('")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kinds::{HRef, Number};

    #[test]
    fn parse_has_simple() {
        let node = parse_filter("site").unwrap();
        assert_eq!(node, FilterNode::Has(Path::single("site")));
    }

    #[test]
    fn parse_has_with_spaces() {
        let node = parse_filter("  site  ").unwrap();
        assert_eq!(node, FilterNode::Has(Path::single("site")));
    }

    #[test]
    fn parse_missing() {
        let node = parse_filter("not equip").unwrap();
        assert_eq!(node, FilterNode::Missing(Path::single("equip")));
    }

    #[test]
    fn parse_cmp_gt_number() {
        let node = parse_filter("temp > 72").unwrap();
        assert_eq!(
            node,
            FilterNode::Cmp {
                path: Path::single("temp"),
                op: CmpOp::Gt,
                val: Kind::Number(Number::unitless(72.0)),
            }
        );
    }

    #[test]
    fn parse_cmp_eq_number_with_unit() {
        let node = parse_filter("temp == 72.5°F").unwrap();
        assert_eq!(
            node,
            FilterNode::Cmp {
                path: Path::single("temp"),
                op: CmpOp::Eq,
                val: Kind::Number(Number::new(72.5, Some("°F".into()))),
            }
        );
    }

    #[test]
    fn parse_and() {
        let node = parse_filter("site and equip").unwrap();
        assert_eq!(
            node,
            FilterNode::And(
                Box::new(FilterNode::Has(Path::single("site"))),
                Box::new(FilterNode::Has(Path::single("equip"))),
            )
        );
    }

    #[test]
    fn parse_or() {
        let node = parse_filter("site or equip").unwrap();
        assert_eq!(
            node,
            FilterNode::Or(
                Box::new(FilterNode::Has(Path::single("site"))),
                Box::new(FilterNode::Has(Path::single("equip"))),
            )
        );
    }

    #[test]
    fn parse_nested_parens() {
        // (site or equip) and dis == "Main"
        let node = parse_filter("(site or equip) and dis == \"Main\"").unwrap();
        assert_eq!(
            node,
            FilterNode::And(
                Box::new(FilterNode::Or(
                    Box::new(FilterNode::Has(Path::single("site"))),
                    Box::new(FilterNode::Has(Path::single("equip"))),
                )),
                Box::new(FilterNode::Cmp {
                    path: Path::single("dis"),
                    op: CmpOp::Eq,
                    val: Kind::Str("Main".into()),
                }),
            )
        );
    }

    #[test]
    fn parse_multi_segment_path_with_cmp() {
        let node = parse_filter("equipRef->siteRef->area > 1000").unwrap();
        assert_eq!(
            node,
            FilterNode::Cmp {
                path: Path(vec!["equipRef".into(), "siteRef".into(), "area".into(),]),
                op: CmpOp::Gt,
                val: Kind::Number(Number::unitless(1000.0)),
            }
        );
    }

    #[test]
    fn parse_string_comparison() {
        let node = parse_filter("dis == \"hello world\"").unwrap();
        assert_eq!(
            node,
            FilterNode::Cmp {
                path: Path::single("dis"),
                op: CmpOp::Eq,
                val: Kind::Str("hello world".into()),
            }
        );
    }

    #[test]
    fn parse_ne_operator() {
        let node = parse_filter("status != \"active\"").unwrap();
        assert_eq!(
            node,
            FilterNode::Cmp {
                path: Path::single("status"),
                op: CmpOp::Ne,
                val: Kind::Str("active".into()),
            }
        );
    }

    #[test]
    fn parse_le_operator() {
        let node = parse_filter("temp <= 100").unwrap();
        assert_eq!(
            node,
            FilterNode::Cmp {
                path: Path::single("temp"),
                op: CmpOp::Le,
                val: Kind::Number(Number::unitless(100.0)),
            }
        );
    }

    #[test]
    fn parse_ge_operator() {
        let node = parse_filter("temp >= 50").unwrap();
        assert_eq!(
            node,
            FilterNode::Cmp {
                path: Path::single("temp"),
                op: CmpOp::Ge,
                val: Kind::Number(Number::unitless(50.0)),
            }
        );
    }

    #[test]
    fn parse_lt_operator() {
        let node = parse_filter("temp < 32").unwrap();
        assert_eq!(
            node,
            FilterNode::Cmp {
                path: Path::single("temp"),
                op: CmpOp::Lt,
                val: Kind::Number(Number::unitless(32.0)),
            }
        );
    }

    #[test]
    fn parse_ref_comparison() {
        let node = parse_filter("siteRef == @site-1").unwrap();
        assert_eq!(
            node,
            FilterNode::Cmp {
                path: Path::single("siteRef"),
                op: CmpOp::Eq,
                val: Kind::Ref(HRef::from_val("site-1")),
            }
        );
    }

    #[test]
    fn parse_spec_match() {
        let node = parse_filter("ph::Ahu").unwrap();
        assert_eq!(node, FilterNode::SpecMatch("ph::Ahu".into()));
    }

    #[test]
    fn parse_spec_match_dotted() {
        let node = parse_filter("ph.equips::Ahu").unwrap();
        assert_eq!(node, FilterNode::SpecMatch("ph.equips::Ahu".into()));
    }

    #[test]
    fn parse_multi_segment_path_has() {
        let node = parse_filter("equipRef->siteRef").unwrap();
        assert_eq!(
            node,
            FilterNode::Has(Path(vec!["equipRef".into(), "siteRef".into()]))
        );
    }

    #[test]
    fn parse_and_or_precedence() {
        // "a and b or c" should parse as "(a and b) or c"
        let node = parse_filter("a and b or c").unwrap();
        assert_eq!(
            node,
            FilterNode::Or(
                Box::new(FilterNode::And(
                    Box::new(FilterNode::Has(Path::single("a"))),
                    Box::new(FilterNode::Has(Path::single("b"))),
                )),
                Box::new(FilterNode::Has(Path::single("c"))),
            )
        );
    }

    #[test]
    fn parse_chained_and() {
        let node = parse_filter("a and b and c").unwrap();
        assert_eq!(
            node,
            FilterNode::And(
                Box::new(FilterNode::And(
                    Box::new(FilterNode::Has(Path::single("a"))),
                    Box::new(FilterNode::Has(Path::single("b"))),
                )),
                Box::new(FilterNode::Has(Path::single("c"))),
            )
        );
    }

    #[test]
    fn parse_chained_or() {
        let node = parse_filter("a or b or c").unwrap();
        assert_eq!(
            node,
            FilterNode::Or(
                Box::new(FilterNode::Or(
                    Box::new(FilterNode::Has(Path::single("a"))),
                    Box::new(FilterNode::Has(Path::single("b"))),
                )),
                Box::new(FilterNode::Has(Path::single("c"))),
            )
        );
    }

    #[test]
    fn parse_not_with_path() {
        let node = parse_filter("not equipRef->siteRef").unwrap();
        assert_eq!(
            node,
            FilterNode::Missing(Path(vec!["equipRef".into(), "siteRef".into()]))
        );
    }

    #[test]
    fn error_empty_string() {
        let err = parse_filter("").unwrap_err();
        assert!(err.to_string().contains("empty filter expression"));
    }

    #[test]
    fn error_invalid_syntax() {
        let err = parse_filter("123").unwrap_err();
        assert!(err.to_string().contains("expected tag name"));
    }

    #[test]
    fn error_unclosed_paren() {
        let err = parse_filter("(site and equip").unwrap_err();
        assert!(err.to_string().contains("expected closing ')'"));
    }

    #[test]
    fn error_trailing_input() {
        // "site )" has trailing input after valid parse
        let err = parse_filter("site )").unwrap_err();
        assert!(err.to_string().contains("unexpected trailing input"));
    }

    #[test]
    fn parse_tag_name_starting_with_or() {
        // "orfoo" should be parsed as Has("orfoo"), not keyword "or" + "foo"
        let node = parse_filter("orfoo").unwrap();
        assert_eq!(node, FilterNode::Has(Path::single("orfoo")));
    }

    #[test]
    fn parse_tag_name_starting_with_and() {
        let node = parse_filter("android").unwrap();
        assert_eq!(node, FilterNode::Has(Path::single("android")));
    }

    #[test]
    fn parse_tag_name_starting_with_not() {
        let node = parse_filter("notable").unwrap();
        assert_eq!(node, FilterNode::Has(Path::single("notable")));
    }

    #[test]
    fn parse_bool_comparison() {
        let node = parse_filter("enabled == T").unwrap();
        assert_eq!(
            node,
            FilterNode::Cmp {
                path: Path::single("enabled"),
                op: CmpOp::Eq,
                val: Kind::Bool(true),
            }
        );
    }

    #[test]
    fn parse_date_comparison() {
        let node = parse_filter("installed == 2024-01-15").unwrap();
        assert_eq!(
            node,
            FilterNode::Cmp {
                path: Path::single("installed"),
                op: CmpOp::Eq,
                val: Kind::Date(chrono::NaiveDate::from_ymd_opt(2024, 1, 15).unwrap()),
            }
        );
    }

    #[test]
    fn parse_complex_expression() {
        // (site or equip) and not deprecated and temp > 72°F
        let node = parse_filter("(site or equip) and not deprecated and temp > 72°F").unwrap();
        // Should parse as: ((site or equip) and (not deprecated)) and (temp > 72°F)
        match node {
            FilterNode::And(_, _) => {} // valid structure
            other => panic!("expected And, got {other:?}"),
        }
    }
}

// Xeto parser -- recursive descent parser producing AST nodes.

use std::collections::HashMap;

use crate::kinds::{Kind, Number};

use super::XetoError;
use super::ast::{LibPragma, SlotDef, SpecDef, XetoFile};
use super::lexer::{TokenType, XetoLexer};

/// Parse Xeto source text into an AST.
pub fn parse_xeto(source: &str) -> Result<XetoFile, XetoError> {
    // Bound both allocation and recursive grammar depth before descending.
    if source.len() > 10 * 1024 * 1024 {
        return Err(XetoError::Parse {
            line: 1,
            col: 1,
            message: "Xeto source exceeds 10 MiB".into(),
        });
    }
    let mut lexer = XetoLexer::new(source);
    let tokens = lexer.tokenize()?;
    let mut depth = 0_usize;
    for token in &tokens {
        match token.typ {
            TokenType::LBrace | TokenType::LAngle => depth += 1,
            TokenType::RBrace | TokenType::RAngle => depth = depth.saturating_sub(1),
            _ => {}
        }
        if depth > 64 {
            return Err(XetoError::Parse {
                line: token.line,
                col: token.col,
                message: "Xeto nesting exceeds 64 levels".into(),
            });
        }
    }
    let mut parser = Parser::new(tokens);
    parser.parse_file()
}

// ---------------------------------------------------------------------------
// Internal parser
// ---------------------------------------------------------------------------

struct Parser {
    tokens: Vec<super::lexer::Token>,
    pos: usize,
}

impl Parser {
    fn new(tokens: Vec<super::lexer::Token>) -> Self {
        Self { tokens, pos: 0 }
    }

    // -- peek / advance / expect helpers --

    fn peek(&self) -> &super::lexer::Token {
        &self.tokens[self.pos]
    }

    fn peek_type(&self) -> &TokenType {
        &self.tokens[self.pos].typ
    }

    fn at_end(&self) -> bool {
        self.peek_type() == &TokenType::Eof
    }

    fn advance(&mut self) -> &super::lexer::Token {
        let tok = &self.tokens[self.pos];
        if tok.typ != TokenType::Eof {
            self.pos += 1;
        }
        tok
    }

    fn expect(&mut self, typ: TokenType) -> Result<&super::lexer::Token, XetoError> {
        let tok = &self.tokens[self.pos];
        if tok.typ != typ {
            return Err(XetoError::Parse {
                line: tok.line,
                col: tok.col,
                message: format!("expected {:?}, got {:?} ('{}')", typ, tok.typ, tok.val),
            });
        }
        Ok(self.advance())
    }

    fn skip_newlines(&mut self) {
        while self.peek_type() == &TokenType::Newline {
            self.advance();
        }
    }

    /// Collect consecutive comment tokens as doc text.
    /// Skips section separators (lines of all slashes like `////`).
    fn collect_doc(&mut self) -> String {
        let mut lines: Vec<String> = Vec::new();
        loop {
            // Peek for comment tokens, skip newlines between comments
            if *self.peek_type() == TokenType::Comment {
                let val = self.peek().val.clone();
                self.advance();
                // Skip section separators: lines of just slashes
                let trimmed = val.trim();
                if !trimmed.is_empty() && trimmed.chars().all(|c| c == '/') {
                    // This is a separator like "////" -- skip it
                    // Also skip the newline after it
                    if *self.peek_type() == TokenType::Newline {
                        self.advance();
                    }
                    continue;
                }
                lines.push(val);
                // Skip newline after comment
                if *self.peek_type() == TokenType::Newline {
                    self.advance();
                }
            } else {
                break;
            }
        }
        lines.join("\n")
    }

    // -- grammar rules --

    /// File := Pragma? Spec*
    fn parse_file(&mut self) -> Result<XetoFile, XetoError> {
        self.skip_newlines();

        let mut pragma = None;

        let mut specs = Vec::new();
        loop {
            self.skip_newlines();
            if self.at_end() {
                break;
            }

            // Collect doc comments
            let doc = self.collect_doc();
            self.skip_newlines();
            if self.at_end() {
                break;
            }

            if *self.peek_type() == TokenType::Ident && self.peek().val == "pragma" {
                if pragma.is_some() || !specs.is_empty() {
                    let tok = self.peek();
                    return Err(XetoError::Parse {
                        line: tok.line,
                        col: tok.col,
                        message: "library pragma must occur once before declarations".into(),
                    });
                }
                pragma = Some(self.parse_pragma()?);
                continue;
            }

            let mut spec = self.parse_spec()?;
            if !doc.is_empty() && spec.doc.is_empty() {
                spec.doc = doc;
            }
            specs.push(spec);
        }

        Ok(XetoFile { pragma, specs })
    }

    /// Pragma := "pragma" ":" "Lib" Meta
    fn parse_pragma(&mut self) -> Result<LibPragma, XetoError> {
        self.expect(TokenType::Ident)?; // "pragma"
        self.expect(TokenType::Colon)?;
        let lib = self.expect(TokenType::Ident)?;
        if lib.val != "Lib" {
            return Err(XetoError::Parse {
                line: lib.line,
                col: lib.col,
                message: "expected Lib pragma".into(),
            });
        }
        self.skip_newlines();

        let meta = if *self.peek_type() == TokenType::LAngle {
            self.parse_meta()?
        } else {
            HashMap::new()
        };

        // Extract known fields from meta
        let name = match meta.get("name") {
            Some(Kind::Str(s)) => s.clone(),
            _ => String::new(),
        };
        let version = match meta.get("version") {
            Some(Kind::Str(s)) => s.clone(),
            _ => String::new(),
        };
        let doc = match meta.get("doc") {
            Some(Kind::Str(s)) => s.clone(),
            _ => String::new(),
        };

        // Parse depends list -- supports both string lists and dict-based format
        let depends = match meta.get("depends") {
            Some(Kind::List(items)) => items
                .iter()
                .filter_map(|k| match k {
                    Kind::Str(s) => Some(s.clone()),
                    Kind::Dict(d) => {
                        if let Some(Kind::Str(lib_name)) = d.get("lib") {
                            Some(lib_name.clone())
                        } else {
                            None
                        }
                    }
                    _ => None,
                })
                .collect(),
            Some(Kind::Dict(d)) => {
                // Single dict depends entry
                if let Some(Kind::Str(lib_name)) = d.get("lib") {
                    vec![lib_name.clone()]
                } else {
                    Vec::new()
                }
            }
            Some(Kind::Str(dep)) => vec![dep.clone()],
            _ => Vec::new(),
        };

        Ok(LibPragma {
            name,
            version,
            doc,
            depends,
            meta,
        })
    }

    /// Spec := Name (":" TypeRef)? Meta? Default? Body?
    fn parse_spec(&mut self) -> Result<SpecDef, XetoError> {
        let is_augmentation = *self.peek_type() == TokenType::Plus;
        if is_augmentation {
            self.advance();
        }
        let name = self.parse_dotted_name()?;
        let mut spec = SpecDef::new(name);
        spec.is_augmentation = is_augmentation;

        self.skip_newlines();

        // Optional `:` TypeRef
        if *self.peek_type() == TokenType::Colon {
            self.advance();
            self.skip_newlines();
            // Guard: if next is <, there's no base type, just meta
            if *self.peek_type() != TokenType::LAngle {
                spec.base = Some(self.parse_type_ref()?);
                self.skip_newlines();
            }
        }

        // Optional Meta
        if *self.peek_type() == TokenType::LAngle {
            spec.meta = self.parse_meta()?;
            self.skip_newlines();
        }

        // Optional Default
        if *self.peek_type() == TokenType::Str || *self.peek_type() == TokenType::Number {
            spec.default = Some(self.parse_value()?);
            self.skip_newlines();
        }

        // Optional Body
        if *self.peek_type() == TokenType::LBrace {
            spec.slots = self.parse_body()?;
        }

        Ok(spec)
    }

    /// Body := "{" Slot* "}"
    fn parse_body(&mut self) -> Result<Vec<SlotDef>, XetoError> {
        self.expect(TokenType::LBrace)?;
        self.skip_newlines();

        let mut slots = Vec::new();
        while *self.peek_type() != TokenType::RBrace && !self.at_end() {
            // Collect doc comments for slot
            let doc = self.collect_doc();
            self.skip_newlines();

            if *self.peek_type() == TokenType::RBrace {
                break;
            }

            let mut slot = self.parse_slot()?;
            if !doc.is_empty() && slot.doc.is_empty() {
                slot.doc = doc;
            }
            slots.push(slot);
            self.skip_newlines();
            if *self.peek_type() == TokenType::Comma {
                self.advance();
                self.skip_newlines();
            }
        }

        self.expect(TokenType::RBrace)?;
        Ok(slots)
    }

    /// Slot := "*"? Name (":" TypeRef Meta? Body? Default?)?
    ///
    /// A bare name with no colon is treated as a marker slot.
    fn parse_slot(&mut self) -> Result<SlotDef, XetoError> {
        let is_global = if *self.peek_type() == TokenType::Star {
            self.advance();
            self.skip_newlines();
            true
        } else {
            false
        };

        let name = self.parse_dotted_name()?;
        let mut slot = SlotDef::new(name);
        slot.is_global = is_global;

        self.skip_newlines();

        // Check for colon -> typed slot
        if *self.peek_type() == TokenType::Colon {
            self.advance();
            self.skip_newlines();

            let type_ref = self.parse_type_ref()?;

            // Check for Query type
            if type_ref == "Query" {
                slot.is_query = true;
                // Parse query parameters from meta
                if *self.peek_type() == TokenType::LAngle {
                    let query_meta = self.parse_meta()?;
                    if let Some(Kind::Str(s)) = query_meta.get("of") {
                        slot.query_of = Some(s.clone());
                    }
                    // Also handle "of" as a bare ident value
                    if slot.query_of.is_none()
                        && let Some(Kind::Marker) = query_meta.get("of")
                    {
                        // "of" was parsed as marker (bare ident); we need
                        // the actual ident value. This is handled by type
                        // ref parsing in meta.
                    }
                    if let Some(Kind::Str(s)) = query_meta.get("via") {
                        slot.query_via = Some(s.clone());
                    }
                    if let Some(Kind::Str(s)) = query_meta.get("inverse") {
                        slot.query_inverse = Some(s.clone());
                    }
                    slot.meta = query_meta;
                }
            } else if type_ref == "Marker" || type_ref == "sys::Marker" {
                // `ahu: Marker` and bare `ahu` mean the same thing. Converging
                // here is what makes them behave the same: a slot carrying
                // `type_ref = "Marker"` was not a marker slot, so it never
                // reached `mandatory_markers()` and was never required — and
                // `check_slot_types` finds nothing to check when the tag is
                // absent, so the spec fitted an entity that had none of it.
                //
                // Only the two spellings of the built-in are accepted. Matching
                // any name ending in `::Marker` looked equivalent and was not: a
                // library defining its own `Marker: Str` had `x: mylib::Marker`
                // silently reduced to presence-only, so `{x: "value"}` fitted a
                // slot that declared a Str-derived type. That destroys a
                // constraint the old code kept, which is worse than the bug being
                // fixed here.
                //
                // A library that shadows the *bare* name with its own `Marker` is
                // still read as the built-in. The parser is purely syntactic and
                // has no library scope to resolve that against; it was equally
                // unresolved before this change, so it is left alone rather than
                // half-handled.
                slot.is_marker = true;
            } else {
                slot.type_ref = Some(type_ref);
            }

            // Maybe suffix (?)
            self.skip_newlines();
            if *self.peek_type() == TokenType::Question {
                self.advance();
                slot.is_maybe = true;
                slot.meta.insert("maybe".to_string(), Kind::Marker);
            }

            // Additional meta
            self.skip_newlines();
            if *self.peek_type() == TokenType::LAngle {
                let extra_meta = self.parse_meta()?;
                for (k, v) in extra_meta {
                    slot.meta.insert(k, v);
                }
            }

            // Maybe suffix after <...> params (e.g. Ref<of:Spec>?)
            self.skip_newlines();
            if !slot.is_maybe && *self.peek_type() == TokenType::Question {
                self.advance();
                slot.is_maybe = true;
                slot.meta.insert("maybe".to_string(), Kind::Marker);
            }

            // Body
            self.skip_newlines();
            if *self.peek_type() == TokenType::LBrace {
                slot.children = self.parse_body()?;
            }

            // Default
            self.skip_newlines();
            if *self.peek_type() == TokenType::Str || *self.peek_type() == TokenType::Number {
                slot.default = Some(self.parse_value()?);
            }
        } else {
            // Bare name = marker slot
            slot.is_marker = true;

            // Bare members retain metadata. Enum admission interprets their
            // implicit type and key; the parser does not invent a Marker type.
            if *self.peek_type() == TokenType::Question {
                self.advance();
                slot.is_maybe = true;
                slot.meta.insert("maybe".to_string(), Kind::Marker);
            }
            if *self.peek_type() == TokenType::LAngle {
                slot.meta.extend(self.parse_meta()?);
            }
            if *self.peek_type() == TokenType::Question {
                self.advance();
                slot.is_maybe = true;
                slot.meta.insert("maybe".to_string(), Kind::Marker);
            }
        }

        Ok(slot)
    }

    /// TypeRef := Ident ("." Ident)* ("::" Ident)? ("?" )?
    ///
    /// Returns the assembled type reference string.
    fn parse_type_ref(&mut self) -> Result<String, XetoError> {
        let mut name = String::new();

        // First ident
        let tok = self.expect(TokenType::Ident)?;
        name.push_str(&tok.val.clone());

        // Dotted parts
        while *self.peek_type() == TokenType::Dot {
            self.advance();
            let part = self.expect(TokenType::Ident)?;
            name.push('.');
            name.push_str(&part.val.clone());
        }

        // Qualified name (::)
        if *self.peek_type() == TokenType::ColonColon {
            self.advance();
            let part = self.expect(TokenType::Ident)?;
            name.push_str("::");
            name.push_str(&part.val.clone());
            while *self.peek_type() == TokenType::Dot {
                self.advance();
                let member = self.expect(TokenType::Ident)?;
                name.push('.');
                name.push_str(&member.val.clone());
            }
        }

        Ok(name)
    }

    /// Meta := "<" MetaTag ("," MetaTag)* ">"
    fn parse_meta(&mut self) -> Result<HashMap<String, Kind>, XetoError> {
        self.expect(TokenType::LAngle)?;
        self.skip_newlines();

        let mut meta = HashMap::new();

        while *self.peek_type() != TokenType::RAngle && !self.at_end() {
            self.skip_newlines();
            if *self.peek_type() == TokenType::RAngle {
                break;
            }

            let tag_name = self.expect(TokenType::Ident)?;
            if meta.contains_key(&tag_name.val) {
                return Err(XetoError::Parse {
                    line: tag_name.line,
                    col: tag_name.col,
                    message: format!("duplicate metadata tag '{}'", tag_name.val),
                });
            }
            let tag_name = tag_name.val.clone();
            self.skip_newlines();

            if *self.peek_type() == TokenType::Colon {
                self.advance();
                self.skip_newlines();
                let value = self.parse_meta_value()?;
                meta.insert(tag_name, value);
            } else {
                // Bare tag = marker
                meta.insert(tag_name, Kind::Marker);
            }

            self.skip_newlines();

            // Optional comma separator
            if *self.peek_type() == TokenType::Comma {
                self.advance();
                self.skip_newlines();
            }
        }

        self.expect(TokenType::RAngle)?;
        Ok(meta)
    }

    /// Parse a meta value (string, number, ident, or list).
    fn parse_meta_value(&mut self) -> Result<Kind, XetoError> {
        match self.peek_type().clone() {
            TokenType::Str => {
                let tok = self.advance();
                Ok(Kind::Str(tok.val.clone()))
            }
            TokenType::Number => {
                let tok = self.advance();
                let val = tok.val.clone();
                Self::parse_number_val(&val)
            }
            TokenType::Ident => {
                let tok = self.advance();
                let val = tok.val.clone();
                // Handle dotted names as string values
                let mut full_name = val;
                while *self.peek_type() == TokenType::Dot {
                    self.advance();
                    let part = self.expect(TokenType::Ident)?;
                    full_name.push('.');
                    full_name.push_str(&part.val.clone());
                }
                // Handle qualified names
                if *self.peek_type() == TokenType::ColonColon {
                    self.advance();
                    let part = self.expect(TokenType::Ident)?;
                    full_name.push_str("::");
                    full_name.push_str(&part.val.clone());
                }
                // Check for parameterized type: Ident<...>
                if *self.peek_type() == TokenType::LAngle {
                    let inner_meta = self.parse_meta()?;
                    let parts: Vec<String> = inner_meta
                        .iter()
                        .map(|(k, v)| format!("{}:{}", k, v))
                        .collect();
                    Ok(Kind::Str(format!("{}<{}>", full_name, parts.join(","))))
                } else {
                    Ok(Kind::Str(full_name))
                }
            }
            TokenType::LBrace => self.parse_meta_collection(),
            _ => {
                let tok = self.peek();
                Err(XetoError::Parse {
                    line: tok.line,
                    col: tok.col,
                    message: format!("expected meta value, got {:?}", tok.typ),
                })
            }
        }
    }

    /// Braces contain either named dict fields or a list of values. Keep names
    /// and singleton lists intact (notably library dependency declarations).
    fn parse_meta_collection(&mut self) -> Result<Kind, XetoError> {
        self.expect(TokenType::LBrace)?;
        self.skip_newlines();
        let is_dict = *self.peek_type() == TokenType::Ident
            && self
                .tokens
                .get(self.pos + 1)
                .is_some_and(|t| t.typ == TokenType::Colon);
        let mut dict = crate::data::HDict::new();
        let mut items = Vec::new();
        while *self.peek_type() != TokenType::RBrace && !self.at_end() {
            if is_dict {
                let key = self.expect(TokenType::Ident)?;
                if dict.has(&key.val) {
                    return Err(XetoError::Parse {
                        line: key.line,
                        col: key.col,
                        message: format!("duplicate dictionary field '{}'", key.val),
                    });
                }
                let key = key.val.clone();
                self.expect(TokenType::Colon)?;
                self.skip_newlines();
                dict.set(key, self.parse_meta_value()?);
            } else {
                items.push(self.parse_meta_value()?);
            }
            self.skip_newlines();
            if *self.peek_type() == TokenType::Comma {
                self.advance();
                self.skip_newlines();
            }
        }
        self.expect(TokenType::RBrace)?;
        Ok(if is_dict {
            Kind::Dict(Box::new(dict))
        } else {
            Kind::List(items)
        })
    }

    /// Parse a value literal (string or number).
    fn parse_value(&mut self) -> Result<Kind, XetoError> {
        match self.peek_type().clone() {
            TokenType::Str => {
                let tok = self.advance();
                Ok(Kind::Str(tok.val.clone()))
            }
            TokenType::Number => {
                let tok = self.advance();
                Self::parse_number_val(&tok.val.clone())
            }
            _ => {
                let tok = self.peek();
                Err(XetoError::Parse {
                    line: tok.line,
                    col: tok.col,
                    message: format!("expected value literal, got {:?}", tok.typ),
                })
            }
        }
    }

    /// Parse a dotted name: Ident ("." Ident)*
    fn parse_dotted_name(&mut self) -> Result<String, XetoError> {
        let tok = self.expect(TokenType::Ident)?;
        let mut name = tok.val.clone();

        while *self.peek_type() == TokenType::Dot {
            self.advance();
            let part = self.expect(TokenType::Ident)?;
            name.push('.');
            name.push_str(&part.val.clone());
        }

        // Also handle qualified names at the top level
        if *self.peek_type() == TokenType::ColonColon {
            self.advance();
            let part = self.expect(TokenType::Ident)?;
            name.push_str("::");
            name.push_str(&part.val.clone());
            while *self.peek_type() == TokenType::Dot {
                self.advance();
                let member = self.expect(TokenType::Ident)?;
                name.push('.');
                name.push_str(&member.val.clone());
            }
        }

        Ok(name)
    }

    /// Parse a numeric string into a Kind::Number.
    fn parse_number_val(text: &str) -> Result<Kind, XetoError> {
        // Split numeric part from unit suffix.
        // The numeric part may include digits, '.', '-', and exponent notation (e/E followed
        // by optional +/- and digits).
        let bytes = text.as_bytes();
        let mut i = 0;
        // Leading minus
        if i < bytes.len() && bytes[i] == b'-' {
            i += 1;
        }
        // Integer digits
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        // Fractional part
        if i < bytes.len() && bytes[i] == b'.' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
        }
        // Exponent part
        if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
            i += 1;
            if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
                i += 1;
            }
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
        }
        let (num_str, unit_str) = text.split_at(i);
        let val: f64 = num_str.parse().map_err(|_| XetoError::Parse {
            line: 0,
            col: 0,
            message: format!("invalid number: {}", text),
        })?;
        let unit = if unit_str.is_empty() {
            None
        } else {
            Some(unit_str.to_string())
        };
        Ok(Kind::Number(Number::new(val, unit)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_file() {
        let file = parse_xeto("").unwrap();
        assert!(file.pragma.is_none());
        assert!(file.specs.is_empty());
    }

    #[test]
    fn parse_simple_spec() {
        let file = parse_xeto("Ahu : Equip {\n  discharge\n  return\n}").unwrap();
        assert_eq!(file.specs.len(), 1);
        let spec = &file.specs[0];
        assert_eq!(spec.name, "Ahu");
        assert_eq!(spec.base.as_deref(), Some("Equip"));
        assert_eq!(spec.slots.len(), 2);
        assert_eq!(spec.slots[0].name, "discharge");
        assert!(spec.slots[0].is_marker);
        assert_eq!(spec.slots[1].name, "return");
        assert!(spec.slots[1].is_marker);
    }

    #[test]
    fn parse_spec_with_meta() {
        let file = parse_xeto("Ahu : Equip <abstract> {\n  discharge\n}").unwrap();
        let spec = &file.specs[0];
        assert!(spec.meta.contains_key("abstract"));
        assert_eq!(spec.meta.get("abstract"), Some(&Kind::Marker));
    }

    #[test]
    fn parse_typed_slots() {
        let file = parse_xeto("Site : Entity {\n  dis : Str\n  area : Number\n}").unwrap();
        let spec = &file.specs[0];
        assert_eq!(spec.slots.len(), 2);
        assert_eq!(spec.slots[0].name, "dis");
        assert_eq!(spec.slots[0].type_ref.as_deref(), Some("Str"));
        assert!(!spec.slots[0].is_marker);
        assert_eq!(spec.slots[1].name, "area");
        assert_eq!(spec.slots[1].type_ref.as_deref(), Some("Number"));
    }

    #[test]
    fn parse_marker_slots() {
        let file = parse_xeto("Ahu : Equip {\n  hot\n  cold\n}").unwrap();
        let spec = &file.specs[0];
        assert!(spec.slots[0].is_marker);
        assert!(spec.slots[1].is_marker);
    }

    #[test]
    fn parse_maybe_slots() {
        let file = parse_xeto("Foo : Bar {\n  name : Str?\n}").unwrap();
        let slot = &file.specs[0].slots[0];
        assert_eq!(slot.name, "name");
        assert_eq!(slot.type_ref.as_deref(), Some("Str"));
        assert!(slot.is_maybe);
        assert!(slot.meta.contains_key("maybe"));
    }

    #[test]
    fn parse_query_slots() {
        let file = parse_xeto("Ahu : Equip {\n  points : Query<of:\"Point\", via:\"equipRef\">\n}")
            .unwrap();
        let slot = &file.specs[0].slots[0];
        assert_eq!(slot.name, "points");
        assert!(slot.is_query);
        assert_eq!(slot.query_of.as_deref(), Some("Point"));
        assert_eq!(slot.query_via.as_deref(), Some("equipRef"));
    }

    #[test]
    fn parse_pragma() {
        let src = r#"pragma : Lib <
  name: "acme",
  version: "1.0.0",
  doc: "My lib"
>
Foo : Bar
"#;
        let file = parse_xeto(src).unwrap();
        let pragma = file.pragma.as_ref().unwrap();
        assert_eq!(pragma.name, "acme");
        assert_eq!(pragma.version, "1.0.0");
        assert_eq!(pragma.doc, "My lib");
    }

    #[test]
    fn parse_dotted_type_refs() {
        let file = parse_xeto("Ahu : ph.equips.Equip").unwrap();
        let spec = &file.specs[0];
        assert_eq!(spec.base.as_deref(), Some("ph.equips.Equip"));
    }

    #[test]
    fn parse_qualified_type_refs() {
        let file = parse_xeto("Ahu : ph::Equip").unwrap();
        let spec = &file.specs[0];
        assert_eq!(spec.base.as_deref(), Some("ph::Equip"));
    }

    #[test]
    fn parse_defaults() {
        let file = parse_xeto("Foo : Str \"hello\"").unwrap();
        let spec = &file.specs[0];
        assert_eq!(spec.default, Some(Kind::Str("hello".to_string())));
    }

    #[test]
    fn parse_number_defaults() {
        let file = parse_xeto("Foo : Number {\n  val : Number 72.5\n}").unwrap();
        let slot = &file.specs[0].slots[0];
        assert_eq!(slot.default, Some(Kind::Number(Number::unitless(72.5))));
    }

    #[test]
    fn parse_doc_comments() {
        let src = "// This is an AHU\n// It handles air\nAhu : Equip";
        let file = parse_xeto(src).unwrap();
        let spec = &file.specs[0];
        assert_eq!(spec.doc, "This is an AHU\nIt handles air");
    }

    #[test]
    fn parse_section_separator_comments() {
        let src = "////\n// Equips\n////\nAhu : Equip";
        let file = parse_xeto(src).unwrap();
        assert_eq!(file.specs.len(), 1);
        assert_eq!(file.specs[0].name, "Ahu");
        assert_eq!(file.specs[0].doc, "Equips");
    }

    #[test]
    fn parse_multiple_specs() {
        let src = "Ahu : Equip\nVav : Equip";
        let file = parse_xeto(src).unwrap();
        assert_eq!(file.specs.len(), 2);
        assert_eq!(file.specs[0].name, "Ahu");
        assert_eq!(file.specs[1].name, "Vav");
    }

    #[test]
    fn parse_global_slots() {
        let file = parse_xeto("Foo : Bar {\n  *name : Str\n}").unwrap();
        let slot = &file.specs[0].slots[0];
        assert!(slot.is_global);
        assert_eq!(slot.name, "name");
    }

    #[test]
    fn parse_maybe_marker_slot() {
        let file = parse_xeto("Foo : Bar {\n  optional?\n}").unwrap();
        let slot = &file.specs[0].slots[0];
        assert!(slot.is_marker);
        assert!(slot.is_maybe);
    }

    #[test]
    fn parse_meta_with_typed_value() {
        let src = r#"Foo : Bar <doc: "A foo", maxVal: 100>"#;
        let file = parse_xeto(src).unwrap();
        let spec = &file.specs[0];
        assert_eq!(spec.meta.get("doc"), Some(&Kind::Str("A foo".to_string())));
        assert_eq!(
            spec.meta.get("maxVal"),
            Some(&Kind::Number(Number::unitless(100.0)))
        );
    }

    #[test]
    fn parse_nested_body() {
        let src = "Ahu : Equip {\n  points : Query {\n    temp : Point\n  }\n}";
        let file = parse_xeto(src).unwrap();
        let slot = &file.specs[0].slots[0];
        assert_eq!(slot.name, "points");
        assert_eq!(slot.children.len(), 1);
        assert_eq!(slot.children[0].name, "temp");
    }

    #[test]
    fn parse_spec_no_base() {
        let file = parse_xeto("Foo {\n  bar\n}").unwrap();
        let spec = &file.specs[0];
        assert_eq!(spec.name, "Foo");
        assert!(spec.base.is_none());
        assert_eq!(spec.slots.len(), 1);
    }

    #[test]
    fn parse_slot_doc_comments() {
        let src = "Foo : Bar {\n  // The name\n  name : Str\n}";
        let file = parse_xeto(src).unwrap();
        let slot = &file.specs[0].slots[0];
        assert_eq!(slot.doc, "The name");
    }

    #[test]
    fn spec_colon_meta_no_base() {
        let file = parse_xeto("Obj : <sealed> {}").unwrap();
        let spec = &file.specs[0];
        assert_eq!(spec.name, "Obj");
        assert!(spec.base.is_none());
        assert!(spec.meta.contains_key("sealed"));
    }

    #[test]
    fn meta_dict_value() {
        let src = r#"pragma : Lib <depends: { { lib: "ph" } }>"#;
        let file = parse_xeto(src).unwrap();
        let pragma = file.pragma.as_ref().unwrap();
        assert_eq!(pragma.depends, vec!["ph"]);
    }

    #[test]
    fn meta_parameterized_type() {
        let src = r#"Foo : Bar <type: Ref<of:Spec>>"#;
        let file = parse_xeto(src).unwrap();
        let spec = &file.specs[0];
        let type_val = spec.meta.get("type").unwrap();
        if let Kind::Str(s) = type_val {
            assert!(s.starts_with("Ref<"));
            assert!(s.contains("of:"));
            assert!(s.contains("Spec"));
            assert!(s.ends_with(">"));
        } else {
            panic!("expected Str, got {:?}", type_val);
        }
    }

    #[test]
    fn slot_maybe_after_params() {
        // ? before <...>
        let file = parse_xeto("Foo : Bar {\n  dis : Str?\n}").unwrap();
        let slot = &file.specs[0].slots[0];
        assert!(slot.is_maybe);

        // ? after <...> meta
        let file2 = parse_xeto("Foo : Bar {\n  link : Ref <of: Equip>?\n}").unwrap();
        let slot2 = &file2.specs[0].slots[0];
        assert!(slot2.is_maybe);
    }
    #[test]
    fn an_explicit_marker_type_is_the_same_slot_as_a_bare_name() {
        // Xeto allows both spellings. Only the bare one used to become a marker
        // slot, so `ahu: Marker` was required by nothing and the spec fitted an
        // entity carrying none of it.
        let file = parse_xeto("Bare: Dict {\n  ahu\n}\nTyped: Dict {\n  ahu: Marker\n}\n")
            .expect("parses");
        for spec in &file.specs {
            let slot = &spec.slots[0];
            assert!(
                slot.is_marker,
                "{}.{} should be a marker slot",
                spec.name, slot.name
            );
            assert_eq!(
                slot.type_ref, None,
                "{}.{} should not keep Marker as a type_ref",
                spec.name, slot.name
            );
            assert!(
                !slot.is_maybe,
                "{}.{} should be required",
                spec.name, slot.name
            );
        }
    }

    #[test]
    fn an_explicit_maybe_marker_stays_optional() {
        let file = parse_xeto("Opt: Dict {\n  ahu: Marker?\n}\nOptBare: Dict {\n  ahu?\n}\n")
            .expect("parses");
        for spec in &file.specs {
            let slot = &spec.slots[0];
            assert!(
                slot.is_marker,
                "{}.{} should be a marker",
                spec.name, slot.name
            );
            assert!(
                slot.is_maybe,
                "{}.{} should stay optional",
                spec.name, slot.name
            );
        }
    }
}

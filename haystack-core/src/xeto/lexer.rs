// Xeto lexer -- tokenizes Xeto source text.

use super::XetoError;

/// Token types produced by the Xeto lexer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenType {
    // Delimiters
    /// `:`
    Colon,
    /// `::`
    ColonColon,
    /// `{`
    LBrace,
    /// `}`
    RBrace,
    /// `<`
    LAngle,
    /// `>`
    RAngle,
    /// `,`
    Comma,
    /// `.`
    Dot,
    /// `?`
    Question,
    /// `*`
    Star,

    // Literals
    /// Identifier (letters, digits, underscores; starts with letter or underscore).
    Ident,
    /// Quoted string literal.
    Str,
    /// Numeric literal (integer or float, with optional unit suffix).
    Number,
    /// Comment (`// ...` to end of line).
    Comment,

    // Special
    /// Newline (consecutive newlines are collapsed).
    Newline,
    /// End of file.
    Eof,
}

/// A single token from Xeto source.
#[derive(Debug, Clone)]
pub struct Token {
    /// Token type.
    pub typ: TokenType,
    /// Token text.
    pub val: String,
    /// 1-based line number.
    pub line: usize,
    /// 1-based column number.
    pub col: usize,
}

/// Tokenizer for Xeto source text.
pub struct XetoLexer {
    chars: Vec<char>,
    pos: usize,
    line: usize,
    col: usize,
}

impl XetoLexer {
    /// Create a new lexer for the given source text.
    pub fn new(source: &str) -> Self {
        Self {
            chars: source.chars().collect(),
            pos: 0,
            line: 1,
            col: 1,
        }
    }

    /// Tokenize the entire source, returning a list of tokens.
    pub fn tokenize(&mut self) -> Result<Vec<Token>, XetoError> {
        let mut tokens = Vec::new();
        let mut last_was_newline = false;

        loop {
            self.skip_spaces();

            if self.at_end() {
                tokens.push(Token {
                    typ: TokenType::Eof,
                    val: String::new(),
                    line: self.line,
                    col: self.col,
                });
                break;
            }

            let ch = self.peek();

            // Newlines
            if ch == '\n' || ch == '\r' {
                self.consume_newline();
                if !last_was_newline {
                    tokens.push(Token {
                        typ: TokenType::Newline,
                        val: "\n".to_string(),
                        line: self.line - 1,
                        col: 1,
                    });
                    last_was_newline = true;
                }
                continue;
            }

            last_was_newline = false;

            // Comments
            if ch == '/' && self.peek_at(1) == Some('/') {
                let tok = self.read_comment();
                tokens.push(tok);
                continue;
            }

            // String literals
            if ch == '"' {
                let tok = self.read_string()?;
                tokens.push(tok);
                continue;
            }

            // Numbers
            if ch.is_ascii_digit()
                || (ch == '-' && self.peek_at(1).is_some_and(|c| c.is_ascii_digit()))
            {
                let tok = self.read_number();
                tokens.push(tok);
                continue;
            }

            // Identifiers
            if ch.is_alphabetic() || ch == '_' {
                let tok = self.read_ident();
                tokens.push(tok);
                continue;
            }

            // Delimiters
            let (typ, val) = match ch {
                ':' => {
                    if self.peek_at(1) == Some(':') {
                        self.advance();
                        self.advance();
                        (TokenType::ColonColon, "::".to_string())
                    } else {
                        self.advance();
                        (TokenType::Colon, ":".to_string())
                    }
                }
                '{' => {
                    self.advance();
                    (TokenType::LBrace, "{".to_string())
                }
                '}' => {
                    self.advance();
                    (TokenType::RBrace, "}".to_string())
                }
                '<' => {
                    self.advance();
                    (TokenType::LAngle, "<".to_string())
                }
                '>' => {
                    self.advance();
                    (TokenType::RAngle, ">".to_string())
                }
                ',' => {
                    self.advance();
                    (TokenType::Comma, ",".to_string())
                }
                '.' => {
                    self.advance();
                    (TokenType::Dot, ".".to_string())
                }
                '?' => {
                    self.advance();
                    (TokenType::Question, "?".to_string())
                }
                '*' => {
                    self.advance();
                    (TokenType::Star, "*".to_string())
                }
                other => {
                    return Err(XetoError::Parse {
                        line: self.line,
                        col: self.col,
                        message: format!("unexpected character: '{}'", other),
                    });
                }
            };

            let line = self.line;
            // col for delimiter was before the advance(s); compute it
            let col_start = self.col - val.len();
            tokens.push(Token {
                typ,
                val,
                line,
                col: col_start,
            });
        }

        Ok(tokens)
    }

    // --- internal helpers ---

    fn at_end(&self) -> bool {
        self.pos >= self.chars.len()
    }

    fn peek(&self) -> char {
        self.chars[self.pos]
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }

    fn advance(&mut self) -> char {
        let ch = self.chars[self.pos];
        self.pos += 1;
        if ch == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        ch
    }

    fn skip_spaces(&mut self) {
        while !self.at_end() {
            let ch = self.peek();
            if ch == ' ' || ch == '\t' || ch == '\r' {
                self.advance();
            } else {
                break;
            }
        }
    }

    fn consume_newline(&mut self) {
        if self.peek() == '\r' {
            self.advance();
            if !self.at_end() && self.peek() == '\n' {
                self.advance();
            }
        } else {
            self.advance();
        }
    }

    fn read_comment(&mut self) -> Token {
        let line = self.line;
        let col = self.col;
        // Skip the two slashes
        self.advance();
        self.advance();
        // Skip optional leading space
        if !self.at_end() && self.peek() == ' ' {
            self.advance();
        }
        let mut text = String::new();
        while !self.at_end() && self.peek() != '\n' && self.peek() != '\r' {
            text.push(self.advance());
        }
        Token {
            typ: TokenType::Comment,
            val: text,
            line,
            col,
        }
    }

    fn read_string(&mut self) -> Result<Token, XetoError> {
        let line = self.line;
        let col = self.col;
        self.advance(); // opening quote
        let mut text = String::new();
        loop {
            if self.at_end() {
                return Err(XetoError::Parse {
                    line,
                    col,
                    message: "unterminated string literal".to_string(),
                });
            }
            let ch = self.advance();
            if ch == '"' {
                break;
            }
            if ch == '\\' {
                if self.at_end() {
                    return Err(XetoError::Parse {
                        line,
                        col,
                        message: "unterminated escape sequence".to_string(),
                    });
                }
                let esc = self.advance();
                match esc {
                    'n' => text.push('\n'),
                    't' => text.push('\t'),
                    '\\' => text.push('\\'),
                    '"' => text.push('"'),
                    other => {
                        text.push('\\');
                        text.push(other);
                    }
                }
            } else {
                text.push(ch);
            }
        }
        Ok(Token {
            typ: TokenType::Str,
            val: text,
            line,
            col,
        })
    }

    fn read_number(&mut self) -> Token {
        let line = self.line;
        let col = self.col;
        let mut text = String::new();

        // Optional leading minus
        if !self.at_end() && self.peek() == '-' {
            text.push(self.advance());
        }

        // Integer part
        while !self.at_end() && self.peek().is_ascii_digit() {
            text.push(self.advance());
        }

        // Fractional part
        if !self.at_end() && self.peek() == '.' && self.peek_at(1).is_some_and(|c| c.is_ascii_digit()) {
            text.push(self.advance()); // '.'
            while !self.at_end() && self.peek().is_ascii_digit() {
                text.push(self.advance());
            }
        }

        // Exponent part (scientific notation)
        if !self.at_end() && (self.peek() == 'e' || self.peek() == 'E') {
            text.push(self.advance());
            if !self.at_end() && (self.peek() == '+' || self.peek() == '-') {
                text.push(self.advance());
            }
            while !self.at_end() && self.peek().is_ascii_digit() {
                text.push(self.advance());
            }
        }

        // Optional unit suffix (letters and special chars like degree, percent, etc.)
        while !self.at_end() {
            let ch = self.peek();
            if ch.is_alphabetic() || ch == '%' || ch == '/' || ch == '\u{00b0}' {
                text.push(self.advance());
            } else {
                break;
            }
        }

        Token {
            typ: TokenType::Number,
            val: text,
            line,
            col,
        }
    }

    fn read_ident(&mut self) -> Token {
        let line = self.line;
        let col = self.col;
        let mut text = String::new();

        while !self.at_end() {
            let ch = self.peek();
            if ch.is_alphanumeric() || ch == '_' {
                text.push(self.advance());
            } else {
                break;
            }
        }

        Token {
            typ: TokenType::Ident,
            val: text,
            line,
            col,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex(source: &str) -> Vec<Token> {
        let mut lexer = XetoLexer::new(source);
        lexer.tokenize().unwrap()
    }

    fn types(tokens: &[Token]) -> Vec<&TokenType> {
        tokens.iter().map(|t| &t.typ).collect()
    }

    #[test]
    fn tokenize_identifiers() {
        let tokens = lex("foo bar_baz Ahu123");
        let idents: Vec<&str> = tokens
            .iter()
            .filter(|t| t.typ == TokenType::Ident)
            .map(|t| t.val.as_str())
            .collect();
        assert_eq!(idents, vec!["foo", "bar_baz", "Ahu123"]);
    }

    #[test]
    fn tokenize_strings() {
        let tokens = lex(r#""hello" "world""#);
        let strs: Vec<&str> = tokens
            .iter()
            .filter(|t| t.typ == TokenType::Str)
            .map(|t| t.val.as_str())
            .collect();
        assert_eq!(strs, vec!["hello", "world"]);
    }

    #[test]
    fn string_escape_sequences() {
        let tokens = lex(r#""line\nnew\ttab\\back\"quote""#);
        let s = &tokens[0];
        assert_eq!(s.typ, TokenType::Str);
        assert_eq!(s.val, "line\nnew\ttab\\back\"quote");
    }

    #[test]
    fn tokenize_numbers() {
        let tokens = lex("42 72.5 -10");
        let nums: Vec<&str> = tokens
            .iter()
            .filter(|t| t.typ == TokenType::Number)
            .map(|t| t.val.as_str())
            .collect();
        assert_eq!(nums, vec!["42", "72.5", "-10"]);
    }

    #[test]
    fn token_positions() {
        let tokens = lex("foo : bar");
        // foo at col 1, : at col 5, bar at col 7
        assert_eq!(tokens[0].line, 1);
        assert_eq!(tokens[0].col, 1);
        assert_eq!(tokens[0].typ, TokenType::Ident);

        assert_eq!(tokens[1].typ, TokenType::Colon);
        assert_eq!(tokens[1].col, 5);

        assert_eq!(tokens[2].typ, TokenType::Ident);
        assert_eq!(tokens[2].col, 7);
    }

    #[test]
    fn comments() {
        let tokens = lex("// this is a comment\nfoo");
        assert_eq!(tokens[0].typ, TokenType::Comment);
        assert_eq!(tokens[0].val, "this is a comment");
        assert_eq!(tokens[1].typ, TokenType::Newline);
        assert_eq!(tokens[2].typ, TokenType::Ident);
        assert_eq!(tokens[2].val, "foo");
    }

    #[test]
    fn newlines_collapsed() {
        let tokens = lex("foo\n\n\nbar");
        let typs = types(&tokens);
        // foo, newline (collapsed), bar, eof
        assert_eq!(
            typs,
            vec![
                &TokenType::Ident,
                &TokenType::Newline,
                &TokenType::Ident,
                &TokenType::Eof,
            ]
        );
    }

    #[test]
    fn delimiters() {
        let tokens = lex(": :: { } < > , . ? *");
        let typs: Vec<&TokenType> = tokens
            .iter()
            .filter(|t| t.typ != TokenType::Eof)
            .map(|t| &t.typ)
            .collect();
        assert_eq!(
            typs,
            vec![
                &TokenType::Colon,
                &TokenType::ColonColon,
                &TokenType::LBrace,
                &TokenType::RBrace,
                &TokenType::LAngle,
                &TokenType::RAngle,
                &TokenType::Comma,
                &TokenType::Dot,
                &TokenType::Question,
                &TokenType::Star,
            ]
        );
    }

    #[test]
    fn complex_sequence() {
        let tokens = lex("Ahu : Equip <abstract> {\n  discharge\n}");
        let typs: Vec<&TokenType> = tokens
            .iter()
            .filter(|t| t.typ != TokenType::Eof)
            .map(|t| &t.typ)
            .collect();
        assert_eq!(
            typs,
            vec![
                &TokenType::Ident,   // Ahu
                &TokenType::Colon,   // :
                &TokenType::Ident,   // Equip
                &TokenType::LAngle,  // <
                &TokenType::Ident,   // abstract
                &TokenType::RAngle,  // >
                &TokenType::LBrace,  // {
                &TokenType::Newline, // \n
                &TokenType::Ident,   // discharge
                &TokenType::Newline, // \n
                &TokenType::RBrace,  // }
            ]
        );
    }

    #[test]
    fn colon_colon_qualified_name() {
        let tokens = lex("ph::Ahu");
        assert_eq!(tokens[0].typ, TokenType::Ident);
        assert_eq!(tokens[0].val, "ph");
        assert_eq!(tokens[1].typ, TokenType::ColonColon);
        assert_eq!(tokens[2].typ, TokenType::Ident);
        assert_eq!(tokens[2].val, "Ahu");
    }

    #[test]
    fn unterminated_string_error() {
        let mut lexer = XetoLexer::new(r#""hello"#);
        let result = lexer.tokenize();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("unterminated string"));
    }

    #[test]
    fn number_with_unit() {
        let tokens = lex("72.5kW");
        assert_eq!(tokens[0].typ, TokenType::Number);
        assert_eq!(tokens[0].val, "72.5kW");
    }

    #[test]
    fn number_with_exponent() {
        let tokens = lex("1e3 2.5E-4 1E+10");
        let nums: Vec<&str> = tokens
            .iter()
            .filter(|t| t.typ == TokenType::Number)
            .map(|t| t.val.as_str())
            .collect();
        assert_eq!(nums, vec!["1e3", "2.5E-4", "1E+10"]);
    }

    #[test]
    fn number_exponent_without_fraction() {
        let tokens = lex("1e3");
        assert_eq!(tokens[0].typ, TokenType::Number);
        assert_eq!(tokens[0].val, "1e3");
    }

    #[test]
    fn bare_cr_as_whitespace() {
        let tokens = lex("foo\rbar");
        let idents: Vec<&str> = tokens
            .iter()
            .filter(|t| t.typ == TokenType::Ident)
            .map(|t| t.val.as_str())
            .collect();
        assert_eq!(idents, vec!["foo", "bar"]);
    }
}

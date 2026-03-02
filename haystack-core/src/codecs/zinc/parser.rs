// Zinc recursive descent parser for scalars and grids.

use crate::codecs::CodecError;
use crate::data::{HCol, HDict, HGrid};
use crate::kinds::*;
use chrono::{NaiveDate, NaiveTime};

/// Hand-written recursive descent parser for Zinc wire format.
pub struct ZincParser<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> ZincParser<'a> {
    /// Create a new parser for the given input.
    pub fn new(src: &'a str) -> Self {
        Self { src, pos: 0 }
    }

    /// Create a new parser starting at the given position within the source.
    pub fn new_at(src: &'a str, pos: usize) -> Self {
        Self { src, pos }
    }

    /// Return the current byte position of the parser.
    pub fn pos(&self) -> usize {
        self.pos
    }

    /// Parse a single scalar value, consuming the entire input.
    pub fn parse_scalar(&mut self) -> Result<Kind, CodecError> {
        let val = self.read_val()?;
        self.skip_spaces();
        if !self.at_end() {
            return Err(self.err(format!(
                "unexpected trailing input: {:?}",
                &self.src[self.pos..]
            )));
        }
        Ok(val)
    }

    // ── Navigation helpers ──

    pub fn at_end(&self) -> bool {
        self.pos >= self.src.len()
    }

    fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn peek_ahead(&self, n: usize) -> Option<char> {
        self.src[self.pos..].chars().nth(n)
    }

    fn consume(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }

    fn consume_if(&mut self, ch: char) -> bool {
        if self.peek() == Some(ch) {
            self.pos += ch.len_utf8();
            true
        } else {
            false
        }
    }

    pub fn skip_spaces(&mut self) {
        while let Some(ch) = self.peek() {
            if ch == ' ' || ch == '\t' {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn remaining(&self) -> &str {
        &self.src[self.pos..]
    }

    fn err(&self, msg: impl Into<String>) -> CodecError {
        CodecError::Parse {
            pos: self.pos,
            message: msg.into(),
        }
    }

    // ── Value dispatch ──

    pub fn read_val(&mut self) -> Result<Kind, CodecError> {
        self.skip_spaces();
        if self.at_end() {
            return Ok(Kind::Null);
        }

        let ch = self.peek().unwrap();

        // N → Null or NA
        if ch == 'N' {
            let next = self.peek_ahead(1);
            if next == Some('A') && !self.is_alpha_at(2) {
                self.pos += 2;
                return Ok(Kind::NA);
            }
            if next.is_none() || !next.unwrap().is_alphanumeric() {
                self.pos += 1;
                return Ok(Kind::Null);
            }
            // Fall through to xstr_or_keyword for NaN etc.
        }

        // T → true
        if ch == 'T' && !self.is_alpha_at(1) {
            self.pos += 1;
            return Ok(Kind::Bool(true));
        }

        // F → false
        if ch == 'F' && !self.is_alpha_at(1) {
            self.pos += 1;
            return Ok(Kind::Bool(false));
        }

        // M → Marker
        if ch == 'M' && !self.is_alpha_at(1) {
            self.pos += 1;
            return Ok(Kind::Marker);
        }

        // R → Remove
        if ch == 'R' && !self.is_alpha_at(1) {
            self.pos += 1;
            return Ok(Kind::Remove);
        }

        // -INF (must check before general number since '-' followed by 'I' is not a digit)
        if ch == '-' && self.remaining().starts_with("-INF") {
            self.pos += 4;
            return Ok(Kind::Number(Number::unitless(f64::NEG_INFINITY)));
        }

        // Number, Date, Time, DateTime (starts with digit or '-' followed by digit)
        let is_neg_num = ch == '-' && self.peek_ahead(1).is_some_and(|c| c.is_ascii_digit());
        if ch.is_ascii_digit() || is_neg_num {
            return self.read_number();
        }

        // String
        if ch == '"' {
            let s = self.read_str()?;
            return Ok(Kind::Str(s));
        }

        // Ref
        if ch == '@' {
            return self.read_ref();
        }

        // URI
        if ch == '`' {
            return self.read_uri();
        }

        // Symbol
        if ch == '^' {
            return self.read_symbol();
        }

        // Coord
        if ch == 'C' && self.peek_ahead(1) == Some('(') {
            return self.read_coord();
        }

        // List
        if ch == '[' {
            return self.read_list();
        }

        // Dict
        if ch == '{' {
            return self.read_dict();
        }

        // XStr or keyword (INF, NaN, NA, etc.)
        if ch.is_uppercase() {
            return self.read_xstr_or_keyword();
        }

        Err(self.err(format!("unexpected character '{ch}'")))
    }

    fn is_alpha_at(&self, offset: usize) -> bool {
        self.src[self.pos..]
            .chars()
            .nth(offset)
            .is_some_and(|c| c.is_alphanumeric())
    }

    // ── Number / Date / Time / DateTime ──

    fn read_number(&mut self) -> Result<Kind, CodecError> {
        // Check for date pattern: YYYY-MM-DD
        if self.looks_like_date() {
            return self.read_date_or_datetime();
        }

        // Check for time pattern: HH:MM
        if self.looks_like_time() {
            return self.read_time();
        }

        // Parse sign
        let neg = self.consume_if('-');

        // Integer part
        let int_part = self.read_digits()?;

        // Decimal part
        let frac_part = if self.peek() == Some('.') {
            self.pos += 1;
            Some(self.read_digits()?)
        } else {
            None
        };

        // Exponent
        let exp_part = if self.peek() == Some('e') || self.peek() == Some('E') {
            let mut exp = String::new();
            exp.push(self.consume().unwrap());
            if self.peek() == Some('+') || self.peek() == Some('-') {
                exp.push(self.consume().unwrap());
            }
            exp.push_str(&self.read_digits()?);
            Some(exp)
        } else {
            None
        };

        // Build number string
        let mut num_str = String::new();
        if neg {
            num_str.push('-');
        }
        num_str.push_str(&int_part);
        if let Some(ref frac) = frac_part {
            num_str.push('.');
            num_str.push_str(frac);
        }
        if let Some(ref exp) = exp_part {
            num_str.push_str(exp);
        }

        let val: f64 = num_str
            .parse()
            .map_err(|_| self.err(format!("invalid number: {num_str}")))?;

        // Unit
        let unit = self.read_unit();

        Ok(Kind::Number(Number::new(
            val,
            if unit.is_empty() { None } else { Some(unit) },
        )))
    }

    fn read_digits(&mut self) -> Result<String, CodecError> {
        let start = self.pos;
        while let Some(ch) = self.peek() {
            if ch.is_ascii_digit() || ch == '_' {
                self.pos += ch.len_utf8();
            } else {
                break;
            }
        }
        let raw = &self.src[start..self.pos];
        let result: String = raw.chars().filter(|&c| c != '_').collect();
        if result.is_empty() {
            return Err(self.err("expected digits"));
        }
        Ok(result)
    }

    fn read_unit(&mut self) -> String {
        let start = self.pos;
        let mut first = true;
        while let Some(ch) = self.peek() {
            if ch.is_alphabetic()
                || ch as u32 > 127
                || ch == '_'
                || ch == '/'
                || ch == '%'
                || ch == '$'
            {
                self.pos += ch.len_utf8();
                first = false;
            } else if ch.is_ascii_digit() && !first {
                // Digits allowed after first unit char
                self.pos += 1;
            } else {
                break;
            }
        }
        self.src[start..self.pos].to_string()
    }

    fn looks_like_date(&self) -> bool {
        // YYYY-MM-DD: need at least 10 chars
        let rem = self.remaining();
        if rem.len() < 10 {
            return false;
        }
        let bytes = rem.as_bytes();
        bytes[0..4].iter().all(|b| b.is_ascii_digit())
            && bytes[4] == b'-'
            && bytes[5..7].iter().all(|b| b.is_ascii_digit())
            && bytes[7] == b'-'
            && bytes[8..10].iter().all(|b| b.is_ascii_digit())
    }

    fn looks_like_time(&self) -> bool {
        // HH:MM
        let rem = self.remaining();
        if rem.len() < 5 {
            return false;
        }
        let bytes = rem.as_bytes();
        bytes[0..2].iter().all(|b| b.is_ascii_digit())
            && bytes[2] == b':'
            && bytes[3..5].iter().all(|b| b.is_ascii_digit())
    }

    fn read_date_or_datetime(&mut self) -> Result<Kind, CodecError> {
        // Read YYYY-MM-DD
        let date_str = &self.src[self.pos..self.pos + 10];
        let date = NaiveDate::parse_from_str(date_str, "%Y-%m-%d")
            .map_err(|e| self.err(format!("invalid date: {e}")))?;
        self.pos += 10;

        // Check for T → datetime
        if self.peek() == Some('T') {
            return self.read_datetime_after_date(date);
        }

        Ok(Kind::Date(date))
    }

    fn read_datetime_after_date(&mut self, date: NaiveDate) -> Result<Kind, CodecError> {
        self.pos += 1; // skip T
        let time_str = self.read_time_str()?;
        let offset_str = self.read_offset()?;

        // Build ISO string and parse
        let iso = format!("{}T{}{}", date, time_str, offset_str);
        let dt = chrono::DateTime::parse_from_str(&iso, "%Y-%m-%dT%H:%M:%S%.f%:z")
            .or_else(|_| chrono::DateTime::parse_from_str(&iso, "%Y-%m-%dT%H:%M:%S%:z"))
            .map_err(|e| self.err(format!("invalid datetime: {e} (from '{iso}')")))?;

        // Read optional timezone name
        self.skip_spaces();
        let tz_name = self.read_tz_name();

        let tz = if tz_name.is_empty() {
            "UTC".to_string()
        } else {
            tz_name
        };

        Ok(Kind::DateTime(HDateTime::new(dt, tz)))
    }

    fn read_time_str(&mut self) -> Result<String, CodecError> {
        let start = self.pos;
        // HH:MM
        if self.remaining().len() < 5 {
            return Err(self.err("expected time HH:MM"));
        }
        self.pos += 5;
        // Optional :SS
        if self.peek() == Some(':') {
            if self.remaining().len() < 3 {
                return Err(self.err("incomplete seconds in time"));
            }
            self.pos += 3; // :SS
            // Optional .FFF...
            if self.peek() == Some('.') {
                self.pos += 1;
                while let Some(ch) = self.peek() {
                    if ch.is_ascii_digit() {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
            }
        }
        Ok(self.src[start..self.pos].to_string())
    }

    fn read_offset(&mut self) -> Result<String, CodecError> {
        if self.at_end() {
            return Ok(String::new());
        }
        if self.peek() == Some('Z') {
            self.pos += 1;
            return Ok("+00:00".to_string());
        }
        if self.peek() == Some('+') || self.peek() == Some('-') {
            let start = self.pos;
            if self.remaining().len() < 3 {
                return Err(self.err("incomplete UTC offset"));
            }
            self.pos += 1; // sign
            self.pos += 2; // HH
            if self.peek() == Some(':') {
                if self.remaining().len() < 3 {
                    return Err(self.err("incomplete UTC offset minutes"));
                }
                self.pos += 3; // :MM
            }
            return Ok(self.src[start..self.pos].to_string());
        }
        Ok(String::new())
    }

    fn read_tz_name(&mut self) -> String {
        let start = self.pos;
        while let Some(ch) = self.peek() {
            if ch.is_alphanumeric() || ch == '_' || ch == '-' || ch == '/' {
                self.pos += ch.len_utf8();
            } else {
                break;
            }
        }
        self.src[start..self.pos].to_string()
    }

    fn read_time(&mut self) -> Result<Kind, CodecError> {
        let time_str = self.read_time_str()?;
        let time = NaiveTime::parse_from_str(&time_str, "%H:%M:%S%.f")
            .or_else(|_| NaiveTime::parse_from_str(&time_str, "%H:%M:%S"))
            .or_else(|_| NaiveTime::parse_from_str(&time_str, "%H:%M"))
            .map_err(|e| self.err(format!("invalid time: {e}")))?;
        Ok(Kind::Time(time))
    }

    // ── Strings ──

    fn read_str(&mut self) -> Result<String, CodecError> {
        self.pos += 1; // skip opening "
        let mut result = String::new();
        while !self.at_end() {
            let ch = self.peek().unwrap();
            if ch == '"' {
                self.pos += 1;
                return Ok(result);
            }
            if ch == '\\' {
                self.pos += 1;
                result.push(self.read_escape()?);
            } else {
                result.push(ch);
                self.pos += ch.len_utf8();
            }
        }
        Err(self.err("unterminated string"))
    }

    fn read_escape(&mut self) -> Result<char, CodecError> {
        if self.at_end() {
            return Err(self.err("unexpected end of escape sequence"));
        }
        let ch = self.consume().unwrap();
        match ch {
            'n' => Ok('\n'),
            'r' => Ok('\r'),
            't' => Ok('\t'),
            '\\' => Ok('\\'),
            '"' => Ok('"'),
            '$' => Ok('$'),
            'b' => Ok('\u{0008}'),
            'f' => Ok('\u{000C}'),
            'u' => {
                if self.remaining().len() < 4 {
                    return Err(self.err("incomplete unicode escape"));
                }
                let hex = &self.src[self.pos..self.pos + 4];
                self.pos += 4;
                let code = u32::from_str_radix(hex, 16)
                    .map_err(|_| self.err(format!("invalid unicode escape: {hex}")))?;
                char::from_u32(code)
                    .ok_or_else(|| self.err(format!("invalid unicode codepoint: {code}")))
            }
            _ => Err(self.err(format!("unknown escape sequence: \\{ch}"))),
        }
    }

    // ── Ref ──

    fn read_ref(&mut self) -> Result<Kind, CodecError> {
        self.pos += 1; // skip @
        let start = self.pos;
        while let Some(ch) = self.peek() {
            if is_ref_char(ch) {
                self.pos += ch.len_utf8();
            } else {
                break;
            }
        }
        let val = self.src[start..self.pos].to_string();

        // Optional display string
        self.skip_spaces();
        let dis = if self.peek() == Some('"') {
            Some(self.read_str()?)
        } else {
            None
        };

        Ok(Kind::Ref(HRef::new(val, dis)))
    }

    // ── URI ──

    fn read_uri(&mut self) -> Result<Kind, CodecError> {
        self.pos += 1; // skip `
        let mut result = String::new();
        while !self.at_end() {
            let ch = self.peek().unwrap();
            if ch == '`' {
                self.pos += 1;
                return Ok(Kind::Uri(Uri::new(result)));
            }
            if ch == '\\' {
                self.pos += 1;
                if let Some(next) = self.consume() {
                    result.push(next);
                }
            } else {
                result.push(ch);
                self.pos += ch.len_utf8();
            }
        }
        Err(self.err("unterminated URI"))
    }

    // ── Symbol ──

    fn read_symbol(&mut self) -> Result<Kind, CodecError> {
        self.pos += 1; // skip ^
        let start = self.pos;
        while let Some(ch) = self.peek() {
            if is_ref_char(ch) {
                self.pos += ch.len_utf8();
            } else {
                break;
            }
        }
        Ok(Kind::Symbol(Symbol::new(&self.src[start..self.pos])))
    }

    // ── Coord ──

    fn read_coord(&mut self) -> Result<Kind, CodecError> {
        self.pos += 2; // skip C(
        let start = self.pos;
        while self.peek() != Some(',') && !self.at_end() {
            self.pos += 1;
        }
        if self.at_end() {
            return Err(self.err("unterminated coord literal, expected ','"));
        }
        let lat: f64 = self.src[start..self.pos]
            .trim()
            .parse()
            .map_err(|_| self.err("invalid coord latitude"))?;
        if !(-90.0..=90.0).contains(&lat) {
            return Err(self.err("coord latitude must be between -90 and 90"));
        }
        self.pos += 1; // skip comma
        let start = self.pos;
        while self.peek() != Some(')') && !self.at_end() {
            self.pos += 1;
        }
        if self.at_end() {
            return Err(self.err("unterminated coord literal, expected ')'"));
        }
        let lng: f64 = self.src[start..self.pos]
            .trim()
            .parse()
            .map_err(|_| self.err("invalid coord longitude"))?;
        if !(-180.0..=180.0).contains(&lng) {
            return Err(self.err("coord longitude must be between -180 and 180"));
        }
        self.pos += 1; // skip )
        Ok(Kind::Coord(Coord::new(lat, lng)))
    }

    // ── List ──

    fn read_list(&mut self) -> Result<Kind, CodecError> {
        self.pos += 1; // skip [
        let mut vals = Vec::new();
        self.skip_spaces();
        while !self.at_end() && self.peek() != Some(']') {
            vals.push(self.read_val()?);
            self.skip_spaces();
            self.consume_if(',');
            self.skip_spaces();
        }
        if !self.at_end() {
            self.pos += 1; // skip ]
        }
        Ok(Kind::List(vals))
    }

    // ── Dict ──

    fn read_dict(&mut self) -> Result<Kind, CodecError> {
        self.pos += 1; // skip {
        let mut dict = HDict::new();
        self.skip_spaces();
        while !self.at_end() && self.peek() != Some('}') {
            let name = self.read_tag_name()?;
            self.skip_spaces();
            if self.peek() == Some(':') {
                self.pos += 1;
                self.skip_spaces();
                let val = self.read_val()?;
                dict.set(name, val);
            } else {
                dict.set(name, Kind::Marker);
            }
            self.skip_spaces();
            self.consume_if(',');
            self.skip_spaces();
        }
        if !self.at_end() {
            self.pos += 1; // skip }
        }
        Ok(Kind::Dict(Box::new(dict)))
    }

    fn read_tag_name(&mut self) -> Result<String, CodecError> {
        let start = self.pos;
        while let Some(ch) = self.peek() {
            if ch.is_alphanumeric() || ch == '_' {
                self.pos += ch.len_utf8();
            } else {
                break;
            }
        }
        let name = self.src[start..self.pos].to_string();
        if name.is_empty() {
            return Err(self.err("expected tag name"));
        }
        Ok(name)
    }

    // ── XStr or keyword ──

    fn read_xstr_or_keyword(&mut self) -> Result<Kind, CodecError> {
        let start = self.pos;
        while let Some(ch) = self.peek() {
            if ch.is_alphanumeric() || ch == '_' {
                self.pos += ch.len_utf8();
            } else {
                break;
            }
        }
        let name = &self.src[start..self.pos];

        match name {
            "INF" => return Ok(Kind::Number(Number::unitless(f64::INFINITY))),
            "NaN" => return Ok(Kind::Number(Number::unitless(f64::NAN))),
            "NA" => return Ok(Kind::NA),
            _ => {}
        }

        // XStr: Type("value")
        if self.peek() == Some('(') {
            self.pos += 1; // skip (
            self.skip_spaces();
            let val = self.read_str()?;
            self.skip_spaces();
            if self.peek() == Some(')') {
                self.pos += 1;
            }
            return Ok(Kind::XStr(XStr::new(name, val)));
        }

        Err(self.err(format!("unknown keyword '{name}'")))
    }

    /// Read an identifier (alphanumeric + underscore).
    pub fn read_id(&mut self) -> String {
        let start = self.pos;
        while let Some(ch) = self.peek() {
            if ch.is_alphanumeric() || ch == '_' {
                self.pos += ch.len_utf8();
            } else {
                break;
            }
        }
        self.src[start..self.pos].to_string()
    }
}

fn is_ref_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_' || ch == ':' || ch == '-' || ch == '.' || ch == '~'
}

/// Decode a single Zinc scalar value from a string.
pub fn decode_scalar(input: &str) -> Result<Kind, CodecError> {
    let mut parser = ZincParser::new(input.trim());
    parser.parse_scalar()
}

// ── Grid decoding ──

/// Decode a Zinc-formatted string into an HGrid.
pub fn decode_grid(input: &str) -> Result<HGrid, CodecError> {
    let lines: Vec<&str> = input
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with("//"))
        .collect();

    if lines.is_empty() {
        return Ok(HGrid::new());
    }

    let mut line_idx = 0;

    // Line 1: ver + grid meta
    let ver_line = lines[line_idx];
    line_idx += 1;

    if !ver_line.starts_with("ver:") {
        return Err(CodecError::Parse {
            pos: 0,
            message: format!("expected 'ver:' header, got: {ver_line:?}"),
        });
    }

    // Skip past ver:"X.X"
    let meta = parse_ver_line_meta(ver_line)?;

    // Line 2: columns
    if line_idx >= lines.len() {
        return Ok(HGrid::from_parts(meta, vec![], vec![]));
    }

    let col_line = lines[line_idx];
    line_idx += 1;

    let cols = if col_line == "empty" {
        vec![]
    } else {
        parse_cols(col_line)?
    };

    // Remaining lines: rows
    let mut rows = Vec::new();
    while line_idx < lines.len() {
        let row_line = lines[line_idx];
        line_idx += 1;
        if row_line.is_empty() {
            continue;
        }
        let row = parse_row(row_line, &cols)?;
        rows.push(row);
    }

    Ok(HGrid::from_parts(meta, cols, rows))
}

fn parse_ver_line_meta(ver_line: &str) -> Result<HDict, CodecError> {
    // Skip past ver:"X.X"
    let mut parser = ZincParser::new(ver_line);
    // Consume ver:"3.0" — find the first space
    while !parser.at_end() && parser.peek() != Some(' ') {
        parser.consume();
    }
    parser.skip_spaces();
    if parser.at_end() {
        return Ok(HDict::new());
    }
    parse_inline_meta(&mut parser)
}

fn parse_cols(line: &str) -> Result<Vec<HCol>, CodecError> {
    let parts = split_csv_aware(line);
    let mut cols = Vec::new();
    for part in parts {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let mut parser = ZincParser::new(part);
        let name = read_col_name(&mut parser);
        parser.skip_spaces();
        let meta = if !parser.at_end() {
            parse_inline_meta(&mut parser)?
        } else {
            HDict::new()
        };
        cols.push(HCol::with_meta(name, meta));
    }
    Ok(cols)
}

fn parse_row(line: &str, cols: &[HCol]) -> Result<HDict, CodecError> {
    let parts = split_csv_aware(line);
    let mut dict = HDict::new();
    for (i, col) in cols.iter().enumerate() {
        if i < parts.len() {
            let cell = parts[i].trim();
            if !cell.is_empty() && cell != "N" {
                let mut parser = ZincParser::new(cell);
                let val = parser.read_val()?;
                dict.set(&col.name, val);
            }
        }
    }
    Ok(dict)
}

fn parse_inline_meta(parser: &mut ZincParser<'_>) -> Result<HDict, CodecError> {
    let mut dict = HDict::new();
    while !parser.at_end() {
        parser.skip_spaces();
        if parser.at_end() {
            break;
        }
        let name = read_col_name(parser);
        if name.is_empty() {
            break;
        }
        parser.skip_spaces();
        if parser.peek() == Some(':') {
            parser.consume();
            parser.skip_spaces();
            let val = parser.read_val()?;
            dict.set(name, val);
        } else {
            dict.set(name, Kind::Marker);
        }
        parser.skip_spaces();
    }
    Ok(dict)
}

fn read_col_name(parser: &mut ZincParser<'_>) -> String {
    parser.read_id()
}

/// Split a line by commas, respecting quoted strings and nested structures.
fn split_csv_aware(line: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;

    for ch in line.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' {
            current.push(ch);
            escaped = true;
            continue;
        }
        if ch == '"' && depth == 0 {
            in_str = !in_str;
            current.push(ch);
            continue;
        }
        if in_str {
            current.push(ch);
            continue;
        }
        match ch {
            '(' | '[' | '{' => {
                depth += 1;
                current.push(ch);
            }
            ')' | ']' | '}' => {
                depth -= 1;
                current.push(ch);
            }
            ',' if depth == 0 => {
                parts.push(std::mem::take(&mut current));
            }
            _ => {
                current.push(ch);
            }
        }
    }
    parts.push(current);
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{HDict, HGrid};
    use chrono::{Datelike, FixedOffset, NaiveDate, NaiveTime, TimeZone};

    // ── Scalar round-trip tests ──

    fn round_trip(kind: &Kind) -> Kind {
        let encoded = crate::codecs::zinc::encode_scalar(kind).unwrap();
        decode_scalar(&encoded).unwrap()
    }

    #[test]
    fn parse_null() {
        assert_eq!(decode_scalar("N").unwrap(), Kind::Null);
    }

    #[test]
    fn parse_true() {
        assert_eq!(decode_scalar("T").unwrap(), Kind::Bool(true));
    }

    #[test]
    fn parse_false() {
        assert_eq!(decode_scalar("F").unwrap(), Kind::Bool(false));
    }

    #[test]
    fn parse_marker() {
        assert_eq!(decode_scalar("M").unwrap(), Kind::Marker);
    }

    #[test]
    fn parse_na() {
        assert_eq!(decode_scalar("NA").unwrap(), Kind::NA);
    }

    #[test]
    fn parse_remove() {
        assert_eq!(decode_scalar("R").unwrap(), Kind::Remove);
    }

    #[test]
    fn roundtrip_null() {
        assert_eq!(round_trip(&Kind::Null), Kind::Null);
    }

    #[test]
    fn roundtrip_bool_true() {
        assert_eq!(round_trip(&Kind::Bool(true)), Kind::Bool(true));
    }

    #[test]
    fn roundtrip_bool_false() {
        assert_eq!(round_trip(&Kind::Bool(false)), Kind::Bool(false));
    }

    #[test]
    fn roundtrip_marker() {
        assert_eq!(round_trip(&Kind::Marker), Kind::Marker);
    }

    #[test]
    fn roundtrip_na() {
        assert_eq!(round_trip(&Kind::NA), Kind::NA);
    }

    #[test]
    fn roundtrip_remove() {
        assert_eq!(round_trip(&Kind::Remove), Kind::Remove);
    }

    // ── Numbers ──

    #[test]
    fn parse_number_zero() {
        assert_eq!(
            decode_scalar("0").unwrap(),
            Kind::Number(Number::unitless(0.0))
        );
    }

    #[test]
    fn parse_number_integer() {
        assert_eq!(
            decode_scalar("42").unwrap(),
            Kind::Number(Number::unitless(42.0))
        );
    }

    #[test]
    fn parse_number_float() {
        assert_eq!(
            decode_scalar("72.5").unwrap(),
            Kind::Number(Number::unitless(72.5))
        );
    }

    #[test]
    fn parse_number_negative() {
        assert_eq!(
            decode_scalar("-23.45").unwrap(),
            Kind::Number(Number::unitless(-23.45))
        );
    }

    #[test]
    fn parse_number_scientific() {
        let k = decode_scalar("5.4e8").unwrap();
        if let Kind::Number(n) = &k {
            assert!((n.val - 5.4e8).abs() < 1.0);
        } else {
            panic!("expected Number, got {k:?}");
        }
    }

    #[test]
    fn parse_number_inf() {
        let k = decode_scalar("INF").unwrap();
        if let Kind::Number(n) = &k {
            assert!(n.val.is_infinite() && n.val > 0.0);
        } else {
            panic!("expected Number(INF)");
        }
    }

    #[test]
    fn parse_number_neg_inf() {
        let k = decode_scalar("-INF").unwrap();
        if let Kind::Number(n) = &k {
            assert!(n.val.is_infinite() && n.val < 0.0);
        } else {
            panic!("expected Number(-INF)");
        }
    }

    #[test]
    fn parse_number_nan() {
        let k = decode_scalar("NaN").unwrap();
        if let Kind::Number(n) = &k {
            assert!(n.val.is_nan());
        } else {
            panic!("expected Number(NaN)");
        }
    }

    #[test]
    fn parse_number_with_unit() {
        let k = decode_scalar("72.5\u{00B0}F").unwrap();
        if let Kind::Number(n) = &k {
            assert_eq!(n.val, 72.5);
            assert_eq!(n.unit.as_deref(), Some("\u{00B0}F"));
        } else {
            panic!("expected Number with unit");
        }
    }

    #[test]
    fn roundtrip_number_zero() {
        assert_eq!(
            round_trip(&Kind::Number(Number::unitless(0.0))),
            Kind::Number(Number::unitless(0.0))
        );
    }

    #[test]
    fn roundtrip_number_integer() {
        assert_eq!(
            round_trip(&Kind::Number(Number::unitless(42.0))),
            Kind::Number(Number::unitless(42.0))
        );
    }

    #[test]
    fn roundtrip_number_float() {
        assert_eq!(
            round_trip(&Kind::Number(Number::unitless(72.5))),
            Kind::Number(Number::unitless(72.5))
        );
    }

    #[test]
    fn roundtrip_number_negative() {
        assert_eq!(
            round_trip(&Kind::Number(Number::unitless(-23.45))),
            Kind::Number(Number::unitless(-23.45))
        );
    }

    #[test]
    fn roundtrip_number_with_unit() {
        let k = Kind::Number(Number::new(72.5, Some("\u{00B0}F".into())));
        let rt = round_trip(&k);
        if let Kind::Number(n) = &rt {
            assert_eq!(n.val, 72.5);
            assert_eq!(n.unit.as_deref(), Some("\u{00B0}F"));
        } else {
            panic!("expected Number");
        }
    }

    #[test]
    fn roundtrip_inf() {
        let k = Kind::Number(Number::unitless(f64::INFINITY));
        let rt = round_trip(&k);
        if let Kind::Number(n) = &rt {
            assert!(n.val.is_infinite() && n.val > 0.0);
        } else {
            panic!("expected Number(INF)");
        }
    }

    #[test]
    fn roundtrip_neg_inf() {
        let k = Kind::Number(Number::unitless(f64::NEG_INFINITY));
        let rt = round_trip(&k);
        if let Kind::Number(n) = &rt {
            assert!(n.val.is_infinite() && n.val < 0.0);
        } else {
            panic!("expected Number(-INF)");
        }
    }

    #[test]
    fn roundtrip_nan() {
        let k = Kind::Number(Number::unitless(f64::NAN));
        let rt = round_trip(&k);
        if let Kind::Number(n) = &rt {
            assert!(n.val.is_nan());
        } else {
            panic!("expected Number(NaN)");
        }
    }

    // ── Strings ──

    #[test]
    fn parse_string_empty() {
        assert_eq!(decode_scalar("\"\"").unwrap(), Kind::Str(String::new()));
    }

    #[test]
    fn parse_string_simple() {
        assert_eq!(
            decode_scalar("\"hello\"").unwrap(),
            Kind::Str("hello".into())
        );
    }

    #[test]
    fn parse_string_escapes() {
        assert_eq!(
            decode_scalar("\"line1\\nline2\"").unwrap(),
            Kind::Str("line1\nline2".into())
        );
        assert_eq!(
            decode_scalar("\"tab\\there\"").unwrap(),
            Kind::Str("tab\there".into())
        );
        assert_eq!(
            decode_scalar("\"back\\\\slash\"").unwrap(),
            Kind::Str("back\\slash".into())
        );
        assert_eq!(
            decode_scalar("\"q\\\"uote\"").unwrap(),
            Kind::Str("q\"uote".into())
        );
        assert_eq!(
            decode_scalar("\"dollar\\$sign\"").unwrap(),
            Kind::Str("dollar$sign".into())
        );
    }

    #[test]
    fn parse_string_unicode_escape() {
        assert_eq!(decode_scalar("\"\\u0041\"").unwrap(), Kind::Str("A".into()));
    }

    #[test]
    fn roundtrip_string_empty() {
        assert_eq!(
            round_trip(&Kind::Str(String::new())),
            Kind::Str(String::new())
        );
    }

    #[test]
    fn roundtrip_string_escapes() {
        let s = "line1\nline2\ttab\\slash\"quote$dollar";
        assert_eq!(round_trip(&Kind::Str(s.into())), Kind::Str(s.into()));
    }

    // ── Refs ──

    #[test]
    fn parse_ref_simple() {
        let k = decode_scalar("@site-1").unwrap();
        if let Kind::Ref(r) = &k {
            assert_eq!(r.val, "site-1");
            assert_eq!(r.dis, None);
        } else {
            panic!("expected Ref");
        }
    }

    #[test]
    fn parse_ref_with_dis() {
        let k = decode_scalar("@site-1 \"Main Site\"").unwrap();
        if let Kind::Ref(r) = &k {
            assert_eq!(r.val, "site-1");
            assert_eq!(r.dis, Some("Main Site".into()));
        } else {
            panic!("expected Ref");
        }
    }

    #[test]
    fn roundtrip_ref_simple() {
        let k = Kind::Ref(HRef::from_val("site-1"));
        let rt = round_trip(&k);
        if let Kind::Ref(r) = &rt {
            assert_eq!(r.val, "site-1");
        } else {
            panic!("expected Ref");
        }
    }

    #[test]
    fn roundtrip_ref_with_dis() {
        let k = Kind::Ref(HRef::new("site-1", Some("Main Site".into())));
        let rt = round_trip(&k);
        if let Kind::Ref(r) = &rt {
            assert_eq!(r.val, "site-1");
            assert_eq!(r.dis, Some("Main Site".into()));
        } else {
            panic!("expected Ref");
        }
    }

    // ── URIs ──

    #[test]
    fn parse_uri_simple() {
        let k = decode_scalar("`http://example.com`").unwrap();
        assert_eq!(k, Kind::Uri(Uri::new("http://example.com")));
    }

    #[test]
    fn parse_uri_with_special() {
        let k = decode_scalar("`http://ex.com/path?q=1&b=2`").unwrap();
        assert_eq!(k, Kind::Uri(Uri::new("http://ex.com/path?q=1&b=2")));
    }

    #[test]
    fn roundtrip_uri() {
        let k = Kind::Uri(Uri::new("http://example.com/path"));
        assert_eq!(round_trip(&k), k);
    }

    // ── Symbols ──

    #[test]
    fn parse_symbol_simple() {
        let k = decode_scalar("^site").unwrap();
        assert_eq!(k, Kind::Symbol(Symbol::new("site")));
    }

    #[test]
    fn parse_symbol_compound() {
        let k = decode_scalar("^hot-water").unwrap();
        assert_eq!(k, Kind::Symbol(Symbol::new("hot-water")));
    }

    #[test]
    fn roundtrip_symbol() {
        let k = Kind::Symbol(Symbol::new("hot-water"));
        assert_eq!(round_trip(&k), k);
    }

    // ── Dates ──

    #[test]
    fn parse_date() {
        let k = decode_scalar("2024-03-13").unwrap();
        assert_eq!(k, Kind::Date(NaiveDate::from_ymd_opt(2024, 3, 13).unwrap()));
    }

    #[test]
    fn roundtrip_date() {
        let k = Kind::Date(NaiveDate::from_ymd_opt(2024, 3, 13).unwrap());
        assert_eq!(round_trip(&k), k);
    }

    // ── Times ──

    #[test]
    fn parse_time() {
        let k = decode_scalar("08:12:05").unwrap();
        assert_eq!(k, Kind::Time(NaiveTime::from_hms_opt(8, 12, 5).unwrap()));
    }

    #[test]
    fn parse_time_with_frac() {
        let k = decode_scalar("14:30:00.123").unwrap();
        assert_eq!(
            k,
            Kind::Time(NaiveTime::from_hms_milli_opt(14, 30, 0, 123).unwrap())
        );
    }

    #[test]
    fn roundtrip_time() {
        let k = Kind::Time(NaiveTime::from_hms_opt(8, 12, 5).unwrap());
        assert_eq!(round_trip(&k), k);
    }

    #[test]
    fn roundtrip_time_frac() {
        let k = Kind::Time(NaiveTime::from_hms_milli_opt(14, 30, 0, 123).unwrap());
        assert_eq!(round_trip(&k), k);
    }

    // ── DateTimes ──

    #[test]
    fn parse_datetime() {
        let k = decode_scalar("2024-01-01T08:12:05-05:00 New_York").unwrap();
        if let Kind::DateTime(hdt) = &k {
            assert_eq!(hdt.tz_name, "New_York");
            assert_eq!(hdt.dt.year(), 2024);
        } else {
            panic!("expected DateTime");
        }
    }

    #[test]
    fn parse_datetime_utc() {
        let k = decode_scalar("2024-06-15T12:00:00+00:00 UTC").unwrap();
        if let Kind::DateTime(hdt) = &k {
            assert_eq!(hdt.tz_name, "UTC");
        } else {
            panic!("expected DateTime");
        }
    }

    #[test]
    fn parse_datetime_z() {
        let k = decode_scalar("2024-06-15T12:00:00Z UTC").unwrap();
        if let Kind::DateTime(hdt) = &k {
            assert_eq!(hdt.tz_name, "UTC");
            assert_eq!(hdt.dt.offset(), &FixedOffset::east_opt(0).unwrap());
        } else {
            panic!("expected DateTime");
        }
    }

    #[test]
    fn roundtrip_datetime() {
        let offset = FixedOffset::west_opt(5 * 3600).unwrap();
        let dt = offset.with_ymd_and_hms(2024, 1, 1, 8, 12, 5).unwrap();
        let k = Kind::DateTime(HDateTime::new(dt, "New_York"));
        let rt = round_trip(&k);
        if let Kind::DateTime(hdt) = &rt {
            assert_eq!(hdt.tz_name, "New_York");
            assert_eq!(hdt.dt, dt);
        } else {
            panic!("expected DateTime");
        }
    }

    #[test]
    fn roundtrip_datetime_utc() {
        let offset = FixedOffset::east_opt(0).unwrap();
        let dt = offset.with_ymd_and_hms(2024, 6, 15, 12, 0, 0).unwrap();
        let k = Kind::DateTime(HDateTime::new(dt, "UTC"));
        let rt = round_trip(&k);
        if let Kind::DateTime(hdt) = &rt {
            assert_eq!(hdt.tz_name, "UTC");
            assert_eq!(hdt.dt, dt);
        } else {
            panic!("expected DateTime");
        }
    }

    // ── Coords ──

    #[test]
    fn parse_coord() {
        let k = decode_scalar("C(37.5458266,-77.4491888)").unwrap();
        assert_eq!(k, Kind::Coord(Coord::new(37.5458266, -77.4491888)));
    }

    #[test]
    fn parse_coord_negative() {
        let k = decode_scalar("C(-33.8688,151.2093)").unwrap();
        assert_eq!(k, Kind::Coord(Coord::new(-33.8688, 151.2093)));
    }

    #[test]
    fn roundtrip_coord() {
        let k = Kind::Coord(Coord::new(37.5458266, -77.4491888));
        assert_eq!(round_trip(&k), k);
    }

    // ── XStr ──

    #[test]
    fn parse_xstr() {
        let k = decode_scalar("Color(\"red\")").unwrap();
        assert_eq!(k, Kind::XStr(XStr::new("Color", "red")));
    }

    #[test]
    fn roundtrip_xstr() {
        let k = Kind::XStr(XStr::new("Color", "red"));
        assert_eq!(round_trip(&k), k);
    }

    // ── Lists ──

    #[test]
    fn parse_list_empty() {
        assert_eq!(decode_scalar("[]").unwrap(), Kind::List(vec![]));
    }

    #[test]
    fn parse_list_mixed() {
        let k = decode_scalar("[1, \"two\", M]").unwrap();
        assert_eq!(
            k,
            Kind::List(vec![
                Kind::Number(Number::unitless(1.0)),
                Kind::Str("two".into()),
                Kind::Marker,
            ])
        );
    }

    #[test]
    fn parse_list_nested() {
        let k = decode_scalar("[[1, 2], [3, 4]]").unwrap();
        assert_eq!(
            k,
            Kind::List(vec![
                Kind::List(vec![
                    Kind::Number(Number::unitless(1.0)),
                    Kind::Number(Number::unitless(2.0)),
                ]),
                Kind::List(vec![
                    Kind::Number(Number::unitless(3.0)),
                    Kind::Number(Number::unitless(4.0)),
                ]),
            ])
        );
    }

    #[test]
    fn roundtrip_list_empty() {
        assert_eq!(round_trip(&Kind::List(vec![])), Kind::List(vec![]));
    }

    #[test]
    fn roundtrip_list_mixed() {
        let k = Kind::List(vec![
            Kind::Number(Number::unitless(1.0)),
            Kind::Str("two".into()),
            Kind::Marker,
        ]);
        assert_eq!(round_trip(&k), k);
    }

    // ── Dicts ──

    #[test]
    fn parse_dict_empty() {
        let k = decode_scalar("{}").unwrap();
        assert_eq!(k, Kind::Dict(Box::new(HDict::new())));
    }

    #[test]
    fn parse_dict_with_marker() {
        let k = decode_scalar("{site}").unwrap();
        if let Kind::Dict(d) = &k {
            assert_eq!(d.get("site"), Some(&Kind::Marker));
        } else {
            panic!("expected Dict");
        }
    }

    #[test]
    fn parse_dict_with_values() {
        let k = decode_scalar("{dis:\"Main\" area:42}").unwrap();
        if let Kind::Dict(d) = &k {
            assert_eq!(d.get("dis"), Some(&Kind::Str("Main".into())));
            assert_eq!(d.get("area"), Some(&Kind::Number(Number::unitless(42.0))));
        } else {
            panic!("expected Dict");
        }
    }

    #[test]
    fn parse_dict_mixed() {
        let k = decode_scalar("{site dis:\"Main\" area:42}").unwrap();
        if let Kind::Dict(d) = &k {
            assert_eq!(d.get("site"), Some(&Kind::Marker));
            assert_eq!(d.get("dis"), Some(&Kind::Str("Main".into())));
            assert_eq!(d.get("area"), Some(&Kind::Number(Number::unitless(42.0))));
        } else {
            panic!("expected Dict");
        }
    }

    #[test]
    fn roundtrip_dict_empty() {
        let k = Kind::Dict(Box::new(HDict::new()));
        assert_eq!(round_trip(&k), k);
    }

    #[test]
    fn roundtrip_dict_with_values() {
        let mut d = HDict::new();
        d.set("dis", Kind::Str("Main".into()));
        d.set("site", Kind::Marker);
        let k = Kind::Dict(Box::new(d));
        let rt = round_trip(&k);
        if let Kind::Dict(d) = &rt {
            assert_eq!(d.get("dis"), Some(&Kind::Str("Main".into())));
            assert_eq!(d.get("site"), Some(&Kind::Marker));
        } else {
            panic!("expected Dict");
        }
    }

    // ── Grid decoding tests ──

    #[test]
    fn decode_grid_empty() {
        let zinc = "ver:\"3.0\"\nempty\n";
        let g = decode_grid(zinc).unwrap();
        assert!(g.cols.is_empty());
        assert!(g.rows.is_empty());
    }

    #[test]
    fn decode_grid_simple() {
        let zinc = "ver:\"3.0\"\ndis,area\n\"Site One\",4500\n\"Site Two\",3200\n";
        let g = decode_grid(zinc).unwrap();
        assert_eq!(g.num_cols(), 2);
        assert_eq!(g.cols[0].name, "dis");
        assert_eq!(g.cols[1].name, "area");
        assert_eq!(g.len(), 2);

        let r0 = g.row(0).unwrap();
        assert_eq!(r0.get("dis"), Some(&Kind::Str("Site One".into())));
        assert_eq!(
            r0.get("area"),
            Some(&Kind::Number(Number::unitless(4500.0)))
        );

        let r1 = g.row(1).unwrap();
        assert_eq!(r1.get("dis"), Some(&Kind::Str("Site Two".into())));
        assert_eq!(
            r1.get("area"),
            Some(&Kind::Number(Number::unitless(3200.0)))
        );
    }

    #[test]
    fn decode_grid_with_meta() {
        let zinc = "ver:\"3.0\" err dis:\"some error\"\nempty\n";
        let g = decode_grid(zinc).unwrap();
        assert!(g.is_err());
        assert_eq!(g.meta.get("dis"), Some(&Kind::Str("some error".into())));
    }

    #[test]
    fn decode_grid_with_col_meta() {
        let zinc = "ver:\"3.0\"\nname,power unit:\"kW\"\n\"AHU-1\",75\n";
        let g = decode_grid(zinc).unwrap();
        assert_eq!(g.num_cols(), 2);
        assert_eq!(g.cols[0].name, "name");
        assert_eq!(g.cols[1].name, "power");
        assert_eq!(g.cols[1].meta.get("unit"), Some(&Kind::Str("kW".into())));
    }

    #[test]
    fn decode_grid_with_null_cells() {
        let zinc = "ver:\"3.0\"\na,b\n1,N\nN,2\n";
        let g = decode_grid(zinc).unwrap();
        assert_eq!(g.len(), 2);
        let r0 = g.row(0).unwrap();
        assert_eq!(r0.get("a"), Some(&Kind::Number(Number::unitless(1.0))));
        assert!(r0.missing("b"));

        let r1 = g.row(1).unwrap();
        assert!(r1.missing("a"));
        assert_eq!(r1.get("b"), Some(&Kind::Number(Number::unitless(2.0))));
    }

    #[test]
    fn decode_grid_with_comments() {
        let zinc = "// comment\nver:\"3.0\"\nempty\n";
        let g = decode_grid(zinc).unwrap();
        assert!(g.cols.is_empty());
    }

    // ── Grid round-trip tests ──

    #[test]
    fn grid_roundtrip_empty() {
        let g = HGrid::new();
        let encoded = crate::codecs::zinc::encode_grid(&g).unwrap();
        let decoded = decode_grid(&encoded).unwrap();
        assert!(decoded.cols.is_empty());
        assert!(decoded.rows.is_empty());
    }

    #[test]
    fn grid_roundtrip_with_data() {
        let cols = vec![HCol::new("dis"), HCol::new("area")];
        let mut row1 = HDict::new();
        row1.set("dis", Kind::Str("Site One".into()));
        row1.set("area", Kind::Number(Number::unitless(4500.0)));
        let mut row2 = HDict::new();
        row2.set("dis", Kind::Str("Site Two".into()));
        row2.set("area", Kind::Number(Number::unitless(3200.0)));
        let g = HGrid::from_parts(HDict::new(), cols, vec![row1, row2]);

        let encoded = crate::codecs::zinc::encode_grid(&g).unwrap();
        let decoded = decode_grid(&encoded).unwrap();
        assert_eq!(decoded.num_cols(), 2);
        assert_eq!(decoded.len(), 2);
        assert_eq!(
            decoded.row(0).unwrap().get("dis"),
            Some(&Kind::Str("Site One".into()))
        );
        assert_eq!(
            decoded.row(0).unwrap().get("area"),
            Some(&Kind::Number(Number::unitless(4500.0)))
        );
    }

    #[test]
    fn grid_roundtrip_with_meta() {
        let mut meta = HDict::new();
        meta.set("err", Kind::Marker);
        meta.set("dis", Kind::Str("some error".into()));
        let g = HGrid::from_parts(meta, vec![], vec![]);

        let encoded = crate::codecs::zinc::encode_grid(&g).unwrap();
        let decoded = decode_grid(&encoded).unwrap();
        assert!(decoded.is_err());
        assert_eq!(
            decoded.meta.get("dis"),
            Some(&Kind::Str("some error".into()))
        );
    }

    #[test]
    fn grid_roundtrip_error_grid() {
        let mut meta = HDict::new();
        meta.set("err", Kind::Marker);
        meta.set("dis", Kind::Str("Error occurred".into()));
        meta.set("errTrace", Kind::Str("stack trace here".into()));
        let g = HGrid::from_parts(meta, vec![], vec![]);

        let encoded = crate::codecs::zinc::encode_grid(&g).unwrap();
        let decoded = decode_grid(&encoded).unwrap();
        assert!(decoded.is_err());
        assert_eq!(
            decoded.meta.get("errTrace"),
            Some(&Kind::Str("stack trace here".into()))
        );
    }

    // ── CSV-aware splitting ──

    #[test]
    fn split_csv_simple() {
        let parts = split_csv_aware("a,b,c");
        assert_eq!(parts, vec!["a", "b", "c"]);
    }

    #[test]
    fn split_csv_with_quotes() {
        let parts = split_csv_aware("\"a,b\",c");
        assert_eq!(parts, vec!["\"a,b\"", "c"]);
    }

    #[test]
    fn split_csv_with_nested() {
        let parts = split_csv_aware("[1,2],3");
        assert_eq!(parts, vec!["[1,2]", "3"]);
    }

    // ── -INF as negative number start ──

    #[test]
    fn parse_neg_inf_standalone() {
        let k = decode_scalar("-INF").unwrap();
        if let Kind::Number(n) = &k {
            assert!(n.val.is_infinite() && n.val < 0.0);
        } else {
            panic!("expected Number(-INF)");
        }
    }

    // ── Codec trait tests ──

    #[test]
    fn zinc_codec_trait() {
        use crate::codecs::Codec;
        let codec = crate::codecs::zinc::ZincCodec;
        assert_eq!(codec.mime_type(), "text/zinc");

        let encoded = codec.encode_scalar(&Kind::Bool(true)).unwrap();
        assert_eq!(encoded, "T");

        let decoded = codec.decode_scalar("T").unwrap();
        assert_eq!(decoded, Kind::Bool(true));

        let g = HGrid::new();
        let grid_str = codec.encode_grid(&g).unwrap();
        let decoded_grid = codec.decode_grid(&grid_str).unwrap();
        assert!(decoded_grid.cols.is_empty());
    }

    // ── Bug fix: trailing input rejection ──

    #[test]
    fn parse_scalar_rejects_trailing_input() {
        assert!(decode_scalar("T extra garbage").is_err());
        assert!(decode_scalar("42 xyz").is_err());
        assert!(decode_scalar("\"hello\" world").is_err());
        assert!(decode_scalar("M extra").is_err());
    }

    #[test]
    fn parse_scalar_allows_trailing_whitespace() {
        assert_eq!(decode_scalar("T  ").unwrap(), Kind::Bool(true));
        assert_eq!(decode_scalar("M ").unwrap(), Kind::Marker);
        assert_eq!(
            decode_scalar("42 ").unwrap(),
            Kind::Number(Number::unitless(42.0))
        );
    }

    // ── Bug fix: unknown escape sequences ──

    #[test]
    fn parse_string_rejects_unknown_escapes() {
        assert!(decode_scalar("\"bad\\x\"").is_err());
        assert!(decode_scalar("\"bad\\a\"").is_err());
        assert!(decode_scalar("\"bad\\z\"").is_err());
    }

    #[test]
    fn parse_string_accepts_valid_escapes() {
        assert!(decode_scalar("\"\\n\"").is_ok());
        assert!(decode_scalar("\"\\r\"").is_ok());
        assert!(decode_scalar("\"\\t\"").is_ok());
        assert!(decode_scalar("\"\\\\\"").is_ok());
        assert!(decode_scalar("\"\\\"\"").is_ok());
        assert!(decode_scalar("\"\\$\"").is_ok());
        assert!(decode_scalar("\"\\b\"").is_ok());
        assert!(decode_scalar("\"\\f\"").is_ok());
        assert!(decode_scalar("\"\\u0041\"").is_ok());
    }
}

//! Private JSON syntax tree. Numbers borrow their original token; this is not
//! another semantic value model. Duplicate keys and trailing input reject.
use super::storage::Index;
use super::*;
use std::borrow::Cow;
pub(super) type Object<'a> = Index<Cow<'a, str>, Node<'a>>;
#[derive(Debug)]
pub(super) enum Node<'a> {
    Null,
    Bool(bool),
    Number(&'a str),
    Str(Cow<'a, str>),
    List(Vec<Node<'a>>),
    Object(Object<'a>),
}
pub(super) fn parse<'a, M: Meter>(
    bytes: &'a [u8],
    meter: &mut M,
) -> Result<Node<'a>, Error<M::Error>> {
    charge(meter, Charge::Input(bytes.len()))?;
    charge(meter, Charge::Work(bytes.len().saturating_add(1)))?;
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("invalid UTF-8"))?;
    let mut parser = Parser { text, at: 0, meter };
    let value = parser.value(0)?;
    parser.space();
    if parser.at != bytes.len() {
        return Err(parser.invalid("trailing input"));
    }
    Ok(value)
}
struct Parser<'a, 'm, M> {
    text: &'a str,
    at: usize,
    meter: &'m mut M,
}
impl<'a, M: Meter> Parser<'a, '_, M> {
    fn invalid(&self, reason: &'static str) -> Error<M::Error> {
        Error::Invalid {
            offset: self.at,
            reason,
        }
    }
    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.at).copied()
    }
    fn space(&mut self) {
        while self.peek().is_some_and(|b| b" \t\n\r".contains(&b)) {
            self.at += 1;
        }
    }
    fn string(&mut self) -> Result<Cow<'a, str>, Error<M::Error>> {
        self.at += 1;
        let start = self.at;
        let mut escaped = false;
        let mut has_escape = false;
        while let Some(byte) = self.peek() {
            self.at += 1;
            if byte == b'"' && !escaped {
                let raw = &self.text[start..self.at - 1];
                charge(self.meter, Charge::Work(raw.len().saturating_add(1)))?;
                if !has_escape {
                    return Ok(Cow::Borrowed(raw));
                }
                charge(self.meter, Charge::Retained(raw.len()))?;
                let mut decoded = String::new();
                decoded
                    .try_reserve_exact(raw.len())
                    .map_err(|_| Error::Allocation)?;
                unescape(raw, &mut decoded).map_err(|_| self.invalid("invalid JSON string"))?;
                return Ok(Cow::Owned(decoded));
            }
            if byte < 0x20 {
                return Err(self.invalid("invalid JSON string"));
            }
            has_escape |= byte == b'\\';
            escaped = byte == b'\\' && !escaped;
        }
        Err(self.invalid("unterminated string"))
    }
    fn value(&mut self, depth: usize) -> Result<Node<'a>, Error<M::Error>> {
        charge(self.meter, Charge::Depth(depth))?;
        charge(self.meter, Charge::Nodes(1))?;
        charge(self.meter, Charge::Work(1))?;
        self.space();
        Ok(match self.peek() {
            Some(b'n') if self.text[self.at..].starts_with("null") => {
                self.at += 4;
                Node::Null
            }
            Some(b't') if self.text[self.at..].starts_with("true") => {
                self.at += 4;
                Node::Bool(true)
            }
            Some(b'f') if self.text[self.at..].starts_with("false") => {
                self.at += 5;
                Node::Bool(false)
            }
            Some(b'"') => Node::Str(self.string()?),
            Some(b'[') => {
                self.at += 1;
                self.space();
                let mut items = Vec::new();
                if self.peek() == Some(b']') {
                    self.at += 1;
                    return Ok(Node::List(items));
                }
                loop {
                    let needed = items.len().saturating_add(1);
                    storage::reserve(&mut items, needed, self.meter)?;
                    items.push(self.value(depth + 1)?);
                    self.space();
                    match self.peek() {
                        Some(b',') => {
                            self.at += 1;
                        }
                        Some(b']') => {
                            self.at += 1;
                            break;
                        }
                        _ => return Err(self.invalid("invalid array delimiter")),
                    }
                }
                Node::List(items)
            }
            Some(b'{') => {
                self.at += 1;
                self.space();
                let mut members = Index::new();
                if self.peek() == Some(b'}') {
                    self.at += 1;
                    return Ok(Node::Object(members));
                }
                loop {
                    self.space();
                    if self.peek() != Some(b'"') {
                        return Err(self.invalid("object key must be string"));
                    }
                    let key = self.string()?;
                    self.space();
                    if self.peek() != Some(b':') {
                        return Err(self.invalid("missing colon"));
                    }
                    self.at += 1;
                    let value = self.value(depth + 1)?;
                    members.push(key, value, self.meter)?;
                    self.space();
                    match self.peek() {
                        Some(b',') => {
                            self.at += 1;
                        }
                        Some(b'}') => {
                            self.at += 1;
                            break;
                        }
                        _ => return Err(self.invalid("invalid object delimiter")),
                    }
                }
                if !members.finish(self.meter)? {
                    return Err(self.invalid("duplicate object key"));
                }
                Node::Object(members)
            }
            Some(b'-' | b'0'..=b'9') => {
                let start = self.at;
                self.at += scalar::number_prefix(&self.text[self.at..])
                    .ok_or_else(|| self.invalid("invalid number"))?;
                Node::Number(&self.text[start..self.at])
            }
            _ => return Err(self.invalid("expected JSON value")),
        })
    }
}

// One output allocation, bounded by the JSON string's source length. ASCII
// escape positions are UTF-8 boundaries, so raw chunks can be copied directly.
fn unescape(raw: &str, output: &mut String) -> Result<(), ()> {
    let bytes = raw.as_bytes();
    let mut at = 0;
    let mut chunk = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        if byte < 0x20 {
            return Err(());
        }
        if byte != b'\\' {
            at += 1;
            continue;
        }
        output.push_str(&raw[chunk..at]);
        at += 1;
        let escape = *bytes.get(at).ok_or(())?;
        at += 1;
        match escape {
            b'"' => output.push('"'),
            b'\\' => output.push('\\'),
            b'/' => output.push('/'),
            b'b' => output.push('\u{8}'),
            b'f' => output.push('\u{c}'),
            b'n' => output.push('\n'),
            b'r' => output.push('\r'),
            b't' => output.push('\t'),
            b'u' => {
                let first = hex4(bytes, &mut at)?;
                let code = if (0xD800..=0xDBFF).contains(&first) {
                    if bytes.get(at..at + 2) != Some(b"\\u") {
                        return Err(());
                    }
                    at += 2;
                    let second = hex4(bytes, &mut at)?;
                    if !(0xDC00..=0xDFFF).contains(&second) {
                        return Err(());
                    }
                    0x10000 + ((first - 0xD800) << 10) + second - 0xDC00
                } else {
                    first
                };
                output.push(char::from_u32(code).ok_or(())?);
            }
            _ => return Err(()),
        }
        chunk = at;
    }
    output.push_str(&raw[chunk..]);
    Ok(())
}
fn hex4(bytes: &[u8], at: &mut usize) -> Result<u32, ()> {
    let digits = bytes.get(*at..*at + 4).ok_or(())?;
    let mut value = 0;
    for byte in digits {
        value = (value << 4) | char::from(*byte).to_digit(16).ok_or(())?;
    }
    *at += 4;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unescaped_syntax_borrows_but_native_conversion_is_precharged() {
        let wire = br#""ordinary ASCII text""#;
        let mut no_owned_storage = |cost| match cost {
            Charge::Retained(_) => Err("owned storage"),
            _ => Ok(()),
        };
        let Node::Str(Cow::Borrowed(text)) = parse(wire, &mut no_owned_storage).unwrap() else {
            panic!("syntax should borrow")
        };
        assert_eq!(text.as_ptr(), wire[1..].as_ptr());
        assert_eq!(text, "ordinary ASCII text");
        assert!(matches!(
            super::super::decode_metered(wire, &Context::standard(), None, &mut no_owned_storage),
            Err(Error::Budget("owned storage"))
        ));
    }

    #[test]
    fn escaped_string_has_one_source_sized_destination() {
        let text = "x".repeat(4096) + "\n" + &"é".repeat(2048);
        let wire = serde_json::to_vec(&text).unwrap();
        let capacity = wire.len() - 2;
        let mut tight = budget::Bounded::new(Limits {
            max_retained_bytes: capacity - 1,
            ..Limits::default()
        })
        .unwrap();
        assert!(matches!(
            parse(&wire, &mut tight),
            Err(Error::Budget(Limit::Retained))
        ));
        let mut sufficient = budget::Bounded::new(Limits {
            max_retained_bytes: capacity,
            ..Limits::default()
        })
        .unwrap();
        let Node::Str(decoded) = parse(&wire, &mut sufficient).unwrap() else {
            panic!()
        };
        assert_eq!(decoded, text);
        let Cow::Owned(decoded) = decoded else {
            panic!("escaped input must own decoded storage")
        };
        assert_eq!(decoded.capacity(), capacity);
    }
    #[test]
    fn first_object_entry_is_reserved_by_its_storage_layout() {
        let mut index = storage::Index::<Cow<'_, str>, Node<'_>>::new();
        let bytes = std::mem::size_of::<(Cow<'_, str>, Node<'_>)>();
        let mut tight = budget::Bounded::new(Limits {
            max_retained_bytes: bytes - 1,
            ..Limits::default()
        })
        .unwrap();
        assert!(matches!(
            index.push(Cow::Borrowed(""), Node::Null, &mut tight),
            Err(Error::Budget(Limit::Retained))
        ));
        assert_eq!(index.len(), 0);
        let mut sufficient = budget::Bounded::new(Limits {
            max_retained_bytes: bytes,
            ..Limits::default()
        })
        .unwrap();
        index
            .push(Cow::Borrowed(""), Node::Null, &mut sufficient)
            .unwrap();
        assert_eq!(index.len(), 1);
    }
}

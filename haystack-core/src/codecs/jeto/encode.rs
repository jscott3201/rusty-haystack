use super::storage::Columns;
use super::*;
use super::{context::Class, scalar::Form};
use crate::data::{HDict, HGrid};
use crate::kinds::{ValuePath, ValuePathSegment};
pub(super) fn run<M: Meter>(
    value: &Kind,
    context: &Context,
    expected: Option<&str>,
    boxing: Boxing,
    meter: &mut M,
) -> Result<Encoding, Error<M::Error>> {
    let mut encoder = Encoder {
        context,
        boxing,
        meter,
        bytes: Vec::new(),
        path: Vec::new(),
        issues: Vec::new(),
    };
    let supported = encoder.value(value, expected, 0)?;
    charge(encoder.meter, Charge::Work(1))?;
    Ok(if !supported {
        Encoding::Unsupported {
            issues: encoder.issues,
        }
    } else if encoder.issues.is_empty() {
        Encoding::Exact(encoder.bytes)
    } else {
        Encoding::Lossy {
            bytes: encoder.bytes,
            issues: encoder.issues,
        }
    })
}
struct Encoder<'a, M> {
    context: &'a Context,
    boxing: Boxing,
    meter: &'a mut M,
    bytes: Vec<u8>,
    path: ValuePath,
    issues: Vec<Issue>,
}
impl<M: Meter> Encoder<'_, M> {
    fn raw(&mut self, text: &str) -> Result<(), Error<M::Error>> {
        charge(self.meter, Charge::Work(text.len().saturating_add(1)))?;
        let needed = self
            .bytes
            .len()
            .checked_add(text.len())
            .ok_or(Error::Allocation)?;
        charge(self.meter, Charge::Output(needed))?;
        storage::reserve(&mut self.bytes, needed, self.meter)?;
        self.bytes.extend_from_slice(text.as_bytes());
        Ok(())
    }
    fn quoted(&mut self, text: &str) -> Result<(), Error<M::Error>> {
        charge(self.meter, Charge::Work(text.len().saturating_add(1)))?;
        // Reserve the complete escaped span once. The second pass writes
        // directly to this buffer, without a quoting temporary or inner growth.
        let size = text
            .bytes()
            .try_fold(2usize, |size, byte| {
                size.checked_add(match byte {
                    b'"' | b'\\' | b'\n' | b'\r' | b'\t' | 8 | 12 => 2,
                    0..=31 => 6,
                    _ => 1,
                })
            })
            .ok_or(Error::Allocation)?;
        let needed = self
            .bytes
            .len()
            .checked_add(size)
            .ok_or(Error::Allocation)?;
        charge(self.meter, Charge::Output(needed))?;
        storage::reserve(&mut self.bytes, needed, self.meter)?;
        self.raw("\"")?;
        let mut chunk = 0;
        for (at, byte) in text.bytes().enumerate() {
            if byte >= 0x20 && byte != b'"' && byte != b'\\' {
                continue;
            }
            self.raw(&text[chunk..at])?;
            match byte {
                b'"' => self.raw("\\\"")?,
                b'\\' => self.raw("\\\\")?,
                b'\n' => self.raw("\\n")?,
                b'\r' => self.raw("\\r")?,
                b'\t' => self.raw("\\t")?,
                8 => self.raw("\\b")?,
                12 => self.raw("\\f")?,
                _ => {
                    const HEX: &[u8; 16] = b"0123456789abcdef";
                    let escaped = [
                        b'\\',
                        b'u',
                        b'0',
                        b'0',
                        HEX[usize::from(byte >> 4)],
                        HEX[usize::from(byte & 15)],
                    ];
                    let escaped =
                        std::str::from_utf8(&escaped).map_err(|_| invalid("invalid escape"))?;
                    self.raw(escaped)?;
                }
            }
            chunk = at + 1;
        }
        self.raw(&text[chunk..])?;
        self.raw("\"")
    }
    fn issue(&mut self, reason: Reason) -> Result<(), Error<M::Error>> {
        let names = self
            .path
            .iter()
            .map(|p| match p {
                crate::kinds::ValuePathSegment::Tag(s) => s.len(),
                _ => 0,
            })
            .sum::<usize>();
        charge(
            self.meter,
            Charge::Work(names.saturating_add(self.path.len())),
        )?;
        charge(
            self.meter,
            Charge::Retained(
                names
                    .saturating_add(self.path.len().saturating_mul(64))
                    .saturating_add(512),
            ),
        )?;
        self.issues.push(Issue {
            path: self.path.clone(),
            reason,
        });
        Ok(())
    }
    fn unsupported(&mut self, reason: Reason) -> Result<bool, Error<M::Error>> {
        self.issue(reason)?;
        Ok(false)
    }
    fn value(
        &mut self,
        value: &Kind,
        expected: Option<&str>,
        depth: usize,
    ) -> Result<bool, Error<M::Error>> {
        charge(self.meter, Charge::Depth(depth))?;
        // Native traversal plus the emitted JSON value. Generated wrapper
        // members are counted separately before writing them.
        charge(self.meter, Charge::Nodes(2))?;
        charge(self.meter, Charge::Work(1))?;
        if matches!(value, Kind::Null) {
            self.raw("null")?;
            return Ok(true);
        }
        match value {
            Kind::Dict(dict) => return self.dict(dict, expected, None, &[], depth),
            Kind::List(items) => return self.list(items, expected, depth),
            Kind::Grid(grid) => return self.grid(grid, depth),
            _ => {}
        }
        let class = expected
            .map(|name| resolve(self.context, name, self.meter))
            .transpose()?;
        if matches!(value, Kind::Str(_)) && matches!(class, Some(Class::Nominal(_))) {
            return self.unsupported(Reason::ContextChangesIdentity);
        }
        let source = scalar::source_bytes(value);
        charge(
            self.meter,
            Charge::Work(source.saturating_mul(8).saturating_add(512)),
        )?;
        charge(
            self.meter,
            Charge::Retained(source.saturating_mul(4).saturating_add(1024)),
        )?;
        if let Kind::Nominal(n) = value {
            charge(
                self.meter,
                Charge::Work(n.text().len().saturating_mul(8).saturating_add(1)),
            )?;
            let Some(Class::Nominal(pattern)) = self.context.lookup(n.spec()) else {
                return self.unsupported(Reason::CatalogMismatch);
            };
            if !pattern
                .is_match(n.text())
                .map_err(|_| invalid("nominal matcher failed"))?
            {
                return self.unsupported(Reason::InvalidScalar);
            }
        }
        let plain = match scalar::representation(value, self.context) {
            Ok(p) => p,
            Err(reason) => return self.unsupported(reason),
        };
        let decoded: Option<Result<Kind, Error<M::Error>>> = if self.boxing == Boxing::All {
            None
        } else {
            text_charge(&plain.text, self.context, self.meter)?;
            Some(match plain.form {
                Form::Bool(b) => Ok(Kind::Bool(b)),
                Form::Number => scalar::numeric(&plain.text, class),
                Form::String => scalar::string(&plain.text, self.context, expected, class),
            })
        };
        let exact = decoded
            .as_ref()
            .is_some_and(|v| v.as_ref().is_ok_and(|v| scalar::same(value, v)));
        let boxed = self.boxing == Boxing::All || self.boxing == Boxing::Auto && !exact;
        if boxed {
            charge(self.meter, Charge::Depth(depth + 1))?;
            charge(
                self.meter,
                Charge::Nodes(2 + usize::from(matches!(value, Kind::Ref(r) if r.dis.is_some()))),
            )?;
            self.raw("{\"spec\":")?;
            self.quoted(plain.spec)?;
            self.raw(",\"val\":")?;
            self.quoted(&plain.text)?;
            if let Kind::Ref(reference) = value
                && let Some(display) = &reference.dis
            {
                self.raw(",\"dis\":")?;
                self.quoted(display)?;
            }
            self.raw("}")?;
        } else {
            if !exact {
                if decoded.is_some_and(|v| v.is_err()) {
                    return self.unsupported(Reason::ContextChangesIdentity);
                }
                self.issue(
                    if matches!(value,Kind::Ref(reference) if reference.dis.is_some()) {
                        Reason::ReferenceDisplay
                    } else {
                        Reason::TypeErased
                    },
                )?;
            }
            match plain.form {
                Form::Bool(true) => self.raw("true")?,
                Form::Bool(false) => self.raw("false")?,
                Form::Number => self.raw(&plain.text)?,
                Form::String => self.quoted(&plain.text)?,
            }
        }
        Ok(true)
    }

    fn push(&mut self, segment: ValuePathSegment) -> Result<(), Error<M::Error>> {
        let size = match &segment {
            ValuePathSegment::Tag(name) => name.len(),
            _ => 0,
        };
        charge(self.meter, Charge::Work(size.saturating_add(1)))?;
        charge(
            self.meter,
            Charge::Retained(size.saturating_add(4 * std::mem::size_of::<ValuePathSegment>())),
        )?;
        self.path.push(segment);
        Ok(())
    }
    fn tag(&mut self, name: &str) -> Result<(), Error<M::Error>> {
        // Reserve the name before constructing the owned path component.
        charge(self.meter, Charge::Retained(name.len()))?;
        self.push(ValuePathSegment::Tag(name.to_owned()))
    }
    fn list(
        &mut self,
        items: &[Kind],
        expected: Option<&str>,
        depth: usize,
    ) -> Result<bool, Error<M::Error>> {
        let class = expected
            .map(|name| resolve(self.context, name, self.meter))
            .transpose()?;
        let of = match class {
            Some(Class::List(of)) => of.as_deref(),
            _ => None,
        };
        self.raw("[")?;
        let mut supported = true;
        for (index, item) in items.iter().enumerate() {
            if index != 0 {
                self.raw(",")?;
            }
            self.push(ValuePathSegment::Item(index))?;
            supported &= self.value(item, of, depth + 1)?;
            self.path.pop();
        }
        self.raw("]")?;
        Ok(supported)
    }
    fn structural<'b>(
        &mut self,
        dict: &'b HDict,
        name: &str,
    ) -> Result<Result<Option<&'b str>, Reason>, Error<M::Error>> {
        charge(self.meter, Charge::Work(name.len().saturating_add(1)))?;
        Ok(match dict.get(name) {
            None => Ok(None),
            Some(Kind::Ref(reference)) if reference.dis.is_none() => Ok(Some(&reference.val)),
            _ => Err(Reason::ReservedMetadata),
        })
    }
    fn sorted<'b>(&mut self, dict: &'b HDict) -> Result<Vec<(&'b str, &'b Kind)>, Error<M::Error>> {
        // HDict preserves this upper bound through deletion and normalizes
        // arbitrary caller tables at construction; insertion capacity alone
        // would undercount tombstones in a previously full map.
        charge(self.meter, Charge::Work(dict.scan_bound()))?;
        let mut pairs = Vec::new();
        storage::reserve(&mut pairs, dict.len(), self.meter)?;
        pairs.extend(dict.iter());
        for (name, _) in &pairs {
            tree_charge(name, pairs.len(), self.meter)?;
        }
        pairs.sort_unstable_by_key(|(name, _)| *name);
        Ok(pairs)
    }
    fn dict(
        &mut self,
        dict: &HDict,
        expected: Option<&str>,
        columns: Option<&Columns<'_>>,
        skip: &[&str],
        depth: usize,
    ) -> Result<bool, Error<M::Error>> {
        let spec = if skip.contains(&"spec") {
            None
        } else {
            match self.structural(dict, "spec")? {
                Ok(spec) => spec,
                Err(reason) => return self.unsupported(reason),
            }
        };
        let class = spec
            .or(expected)
            .map(|name| resolve(self.context, name, self.meter))
            .transpose()?;
        if spec.is_some() && !matches!(class, Some(Class::Dict(_))) {
            return self.unsupported(Reason::ReservedMetadata);
        }
        if spec.is_none() && matches!(class, Some(Class::Grid(_))) {
            return self.unsupported(Reason::ContextChangesIdentity);
        }
        let members = self.sorted(dict)?;
        self.raw("{")?;
        let mut first = true;
        let mut supported = true;
        for (name, value) in members {
            if skip.contains(&name) {
                continue;
            }
            if !first {
                self.raw(",")?;
            }
            first = false;
            self.quoted(name)?;
            self.raw(":")?;
            self.tag(name)?;
            if name == "spec" {
                charge(self.meter, Charge::Depth(depth + 1))?;
                charge(self.meter, Charge::Nodes(2))?;
                self.quoted(spec.ok_or_else(|| invalid("missing structural spec"))?)?;
            } else {
                let column_type = if let Some(columns) = columns {
                    tree_charge(name, columns.len(), self.meter)?;
                    match columns.get(name) {
                        Some(of) => *of,
                        None => {
                            supported &= self.unsupported(Reason::InvalidGridShape)?;
                            self.path.pop();
                            continue;
                        }
                    }
                } else {
                    None
                };
                if matches!(value, Kind::Null) {
                    supported &= self.unsupported(Reason::NullDictMember)?;
                } else {
                    let expected = column_type.or(member_type(class, name, self.meter)?);
                    supported &= self.value(value, expected, depth + 1)?;
                }
            }
            self.path.pop();
        }
        self.raw("}")?;
        Ok(supported)
    }
    fn grid(&mut self, grid: &HGrid, depth: usize) -> Result<bool, Error<M::Error>> {
        let spec = match self.structural(&grid.meta, "spec")? {
            Ok(spec) => spec,
            Err(reason) => return self.unsupported(reason),
        };
        // A generated sys::Grid has no native metadata tag. Emitting a redundant
        // native tag would erase that distinction when it is decoded.
        if spec == Some("sys::Grid") {
            return self.unsupported(Reason::ReservedMetadata);
        }
        let spec = spec.unwrap_or("sys::Grid");
        let Class::Grid(inherited_of) = resolve(self.context, spec, self.meter)? else {
            return self.unsupported(Reason::ReservedMetadata);
        };
        let of = match self.structural(&grid.meta, "of")? {
            Ok(of) => of,
            Err(reason) => return self.unsupported(reason),
        };
        if of.is_some() && inherited_of.is_some() {
            return self.unsupported(Reason::ReservedMetadata);
        }
        let row_type = of.or(inherited_of.as_deref());
        if let Some(row_type) = row_type
            && !matches!(resolve(self.context, row_type, self.meter)?, Class::Dict(_))
        {
            return self.unsupported(Reason::ReservedMetadata);
        }
        let mut columns = Columns::with_capacity(grid.cols.len(), self.meter)?;
        for col in &grid.cols {
            charge(self.meter, Charge::Work(col.name.len().saturating_add(1)))?;
            if col.name.is_empty() || col.name == "spec" || col.meta.has("spec") {
                return self.unsupported(Reason::InvalidGridShape);
            }
            let of = match self.structural(&col.meta, "of")? {
                Ok(of) => of,
                Err(reason) => return self.unsupported(reason),
            };
            if let Some(of) = of {
                resolve(self.context, of, self.meter)?;
            }
            columns.push(col.name.as_str(), of, self.meter)?;
        }
        if !columns.finish(self.meter)? {
            return self.unsupported(Reason::InvalidGridShape);
        }
        charge(self.meter, Charge::Depth(depth + 1))?;
        // spec, cols array, rows array, and optional structural of.
        charge(self.meter, Charge::Nodes(3 + usize::from(of.is_some())))?;
        self.raw("{\"spec\":")?;
        self.quoted(spec)?;
        if let Some(of) = of {
            self.raw(",\"of\":")?;
            self.quoted(of)?;
        }
        let mut supported = true;
        if grid.meta.len() > usize::from(grid.meta.has("spec")) + usize::from(grid.meta.has("of")) {
            self.raw(",\"meta\":")?;
            self.push(ValuePathSegment::GridMeta)?;
            charge(self.meter, Charge::Nodes(2))?;
            supported &= self.dict(&grid.meta, None, None, &["spec", "of"], depth + 1)?;
            self.path.pop();
        }
        self.raw(",\"cols\":[")?;
        for (index, col) in grid.cols.iter().enumerate() {
            if index != 0 {
                self.raw(",")?;
            }
            charge(self.meter, Charge::Depth(depth + 3))?;
            charge(self.meter, Charge::Nodes(3))?; // native column, wire object, name
            self.raw("{\"name\":")?;
            self.quoted(&col.name)?;
            tree_charge(&col.name, columns.len(), self.meter)?;
            if let Some(of) = columns.get(col.name.as_str()).copied().flatten() {
                charge(self.meter, Charge::Nodes(2))?; // native Ref and wire string
                self.raw(",\"of\":")?;
                self.quoted(of)?;
            }
            if col.meta.len() > usize::from(col.meta.has("of")) {
                self.raw(",\"meta\":")?;
                self.push(ValuePathSegment::ColumnMeta(index))?;
                charge(self.meter, Charge::Nodes(2))?;
                supported &= self.dict(&col.meta, None, None, &["spec", "of"], depth + 3)?;
                self.path.pop();
            }
            self.raw("}")?;
        }
        self.raw("],\"rows\":[")?;
        for (index, row) in grid.rows.iter().enumerate() {
            if index != 0 {
                self.raw(",")?;
            }
            charge(self.meter, Charge::Depth(depth + 2))?;
            charge(self.meter, Charge::Nodes(2))?;
            self.push(ValuePathSegment::Row(index))?;
            supported &= self.dict(row, row_type, Some(&columns), &[], depth + 2)?;
            self.path.pop();
        }
        self.raw("]}")?;
        Ok(supported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quoted_growth_rejects_before_reallocating_output() {
        let context = Context::standard();
        let mut meter = budget::Bounded::new(Limits {
            max_retained_bytes: 2,
            ..Limits::default()
        })
        .unwrap();
        let mut encoder = Encoder {
            context: &context,
            boxing: Boxing::Auto,
            meter: &mut meter,
            bytes: Vec::new(),
            path: Vec::new(),
            issues: Vec::new(),
        };
        assert!(matches!(
            encoder.quoted("a"),
            Err(Error::Budget(Limit::Retained))
        ));
        assert!(encoder.bytes.is_empty());
        assert_eq!(encoder.bytes.capacity(), 0);
        let mut meter = budget::Bounded::new(Limits {
            max_retained_bytes: 4,
            ..Limits::default()
        })
        .unwrap();
        let mut encoder = Encoder {
            context: &context,
            boxing: Boxing::Auto,
            meter: &mut meter,
            bytes: Vec::new(),
            path: Vec::new(),
            issues: Vec::new(),
        };
        encoder.quoted("a").unwrap();
        assert_eq!(encoder.bytes, br#""a""#);
        assert_eq!(encoder.bytes.capacity(), 4);
    }
}

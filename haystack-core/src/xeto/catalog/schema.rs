//! Bounded JSON Schema projection of the admitted contextual, unboxed wire shape.
//!
//! JSON validation cannot establish nominal identity, reference resolution, units,
//! or lossless encoding. In particular, a generic Dict stays generic: its schema
//! never promises that every native Dict can be served with `box=none`.
use super::{Catalog, Slot, slot_type};
use crate::kinds::Kind;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub trait SchemaControl {
    type Error;
    /// Called before retained allocations and traversal; owned by the caller.
    fn charge(&mut self, work: usize, retained: usize) -> Result<(), Self::Error>;
    fn visible(&self, qname: &str) -> bool;
    fn depth(&mut self, depth: usize) -> Result<(), Self::Error>;
}

#[derive(Debug)]
pub enum SchemaError<E> {
    Control(E),
    /// Fixed reasons intentionally contain no hidden dependency identities.
    Unsupported(&'static str),
}
/// One export, one immutable catalog, one caller's original resource budget.
pub struct SchemaProjector<'a, C> {
    catalog: &'a Catalog,
    control: C,
    definitions: BTreeMap<String, Value>,
    active: BTreeSet<String>,
}
impl<'a, C: SchemaControl> SchemaProjector<'a, C> {
    pub fn new(catalog: &'a Catalog, control: C) -> Self {
        Self {
            catalog,
            control,
            definitions: BTreeMap::new(),
            active: BTreeSet::new(),
        }
    }
    pub fn definitions(self) -> BTreeMap<String, Value> {
        self.definitions
    }
    fn charge(&mut self, work: usize, retained: usize) -> Result<(), SchemaError<C::Error>> {
        self.control
            .charge(work, retained)
            .map_err(SchemaError::Control)
    }
    /// Failed roots cannot leave dangling references or partially emitted types.
    pub fn definition(&mut self, name: &str) -> Result<Value, SchemaError<C::Error>> {
        self.charge(
            self.definitions.len() + 1,
            self.definitions.keys().map(|name| name.len() + 64).sum(),
        )?;
        let before: BTreeSet<_> = self.definitions.keys().cloned().collect();
        let result = self.ty(name, 0);
        if result.is_err() {
            self.definitions.retain(|key, _| before.contains(key));
            self.active.clear();
        }
        result
    }
    pub fn callable(&mut self, name: &str) -> Result<(Value, Value), SchemaError<C::Error>> {
        self.charge(
            self.definitions.len() + 1,
            self.definitions.keys().map(|name| name.len() + 64).sum(),
        )?;
        let before: BTreeSet<_> = self.definitions.keys().cloned().collect();
        let result = self.callable_inner(name);
        if result.is_err() {
            self.definitions.retain(|key, _| before.contains(key));
            self.active.clear();
        }
        result
    }
    fn callable_inner(&mut self, name: &str) -> Result<(Value, Value), SchemaError<C::Error>> {
        self.charge(1, 8192)?;
        let declaration = self
            .catalog
            .declaration(name)
            .ok_or(SchemaError::Unsupported("unadmitted declaration"))?;
        let mut properties = BTreeMap::new();
        let mut required = Vec::new();
        let mut result = json!({"type":"null"});
        for slot in &declaration.spec.slots {
            if slot.name == "returns" {
                result = if slot_type(slot) == "sys::None" {
                    json!({"type":"null"})
                } else {
                    self.slot(slot, 0)?
                };
                if slot.is_maybe() {
                    result = json!({"anyOf":[result,{"type":"null"}]});
                }
            } else {
                let mut schema = self.slot(slot, 0)?;
                if slot.is_maybe() || slot.default.is_some() {
                    schema = json!({"anyOf":[schema,{"type":"null"}],"description":"JSON null is normalized to absence before parameter defaults and native binding."});
                } else {
                    required.push(slot.name.clone());
                }
                if let Some(default) = &slot.default {
                    schema["default"] = match default {
                        Kind::Bool(v) => json!(v),
                        Kind::Str(v) => json!(v),
                        Kind::Int(v) => json!(v),
                        _ => {
                            return Err(SchemaError::Unsupported(
                                "parameter default has no qualified JSON projection",
                            ));
                        }
                    };
                }
                properties.insert(slot.name.clone(), schema);
            }
        }
        Ok((
            json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
            result,
        ))
    }
    fn slot(&mut self, slot: &Slot, depth: usize) -> Result<Value, SchemaError<C::Error>> {
        self.charge(1, 2048 + slot.name.len())?;
        if slot.is_query || !slot.children.is_empty() {
            return Err(SchemaError::Unsupported(
                "query or inline slots are outside the schema profile",
            ));
        }
        let name = slot_type(slot);
        let mut result = if name == "sys::List" {
            let Some(Kind::Ref(of)) = slot.meta.get("of") else {
                return Err(SchemaError::Unsupported(
                    "untyped List is outside the schema profile",
                ));
            };
            json!({"type":"array","items":self.ty(&of.val,depth+1)?})
        } else {
            self.ty(name, depth + 1)?
        };
        if name == "sys::Grid"
            && let Some(Kind::Ref(of)) = slot.meta.get("of")
        {
            self.charge(1, 8192)?;
            let row = self.ty(&of.val, depth + 1)?;
            result = self.definitions["sys::Grid"].clone();
            result["properties"]["rows"]["items"] = row;
            result["description"] = json!(
                "Grid with the admitted row shape. Column agreement and native contextual identity still require runtime validation."
            );
        }
        let mut constraints = Vec::new();
        Self::constraints(&slot.meta, &mut constraints)?;
        self.apply_constraints(&mut result, constraints)?;
        Ok(result)
    }
    fn ty(&mut self, name: &str, depth: usize) -> Result<Value, SchemaError<C::Error>> {
        self.control.depth(depth).map_err(SchemaError::Control)?;
        self.charge(1, name.len() * 2 + 256)?;
        if !self.control.visible(name) {
            return Err(SchemaError::Unsupported("dependency is not visible"));
        }
        if depth > 32 {
            return Err(SchemaError::Unsupported("schema traversal bound exceeded"));
        }
        let reference = json!({"$ref":format!("#/components/schemas/{name}")});
        if self.definitions.contains_key(name) || self.active.contains(name) {
            return Ok(reference);
        }
        if self.definitions.len() + self.active.len() >= 256 {
            return Err(SchemaError::Unsupported("schema traversal bound exceeded"));
        }
        self.charge(1, 8192)?;
        let declaration = self
            .catalog
            .declaration(name)
            .ok_or(SchemaError::Unsupported("unadmitted type"))?;
        self.active.insert(name.into());
        let mut schema = if let Some(keys) = self.catalog.enum_keys(name) {
            self.charge(keys.len(), keys.iter().map(|key| key.len() + 64).sum())?;
            json!({"type":"string","enum":keys})
        } else {
            match name {
                "sys::Obj" | "sys::Scalar" | "sys::Collection" | "sys::Func" | "sys::Funcs"
                | "sys::Interface" | "sys::Enum" | "sys::List" => {
                    return Err(SchemaError::Unsupported(
                        "abstract or generic type is outside the qualified schema profile",
                    ));
                }
                "sys::Bool" => json!({"type":"boolean"}),
                "sys::Int" => json!({"type":"integer","minimum":i64::MIN,"maximum":i64::MAX}),
                "sys::Float" => {
                    json!({"anyOf":[{"type":"number"},{"type":"string","enum":["NaN","INF","-INF"]}]})
                }
                "sys::Number" => {
                    json!({"anyOf":[{"type":"number"},{"type":"string","pattern":"^(?:NaN|INF|-INF|-?(?:0|[1-9][0-9]*)(?:\\.[0-9]+)?(?:[eE][+-]?[0-9]+)?(?:[A-Za-z%_/$]|[^\\x00-\\x7F])+)$(?![\\s\\S])"}],"description":"Unit-bearing numbers retain their lexical unit suffix; unit semantics require the native context."})
                }
                "sys::Marker" => json!({"type":"string","enum":["✓"]}),
                "sys::None" => json!({"type":"string","enum":["∅"]}),
                "sys::NA" => json!({"type":"string","enum":["NA"]}),
                "sys::Str" | "sys::Uri" | "sys::Filter" | "sys.api::ApiVersion" => {
                    json!({"type":"string"})
                }
                "sys::Ref" => {
                    json!({"type":"string","pattern":"^[A-Za-z0-9_.~:-]*$(?![\\s\\S])","description":"Contextual reference identifier; JSON validation does not resolve its target."})
                }
                "sys::Date" => json!({"type":"string","format":"date"}),
                "sys::DateTime" => {
                    json!({"type":"string","pattern":"^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(?:\\.[0-9]{1,9})?(?:Z|[+-][0-9]{2}:[0-9]{2})(?: [A-Za-z0-9_+-]+)?$(?![\\s\\S])","description":"Xeto DateTime text, including an optional Haystack time zone suffix; not restricted to RFC3339."})
                }
                "sys::Time" => {
                    json!({"type":"string","pattern":"^[0-9]{2}:[0-9]{2}:[0-9]{2}(?:\\.[0-9]{1,9})?$(?![\\s\\S])"})
                }
                "sys::Buf" => {
                    return Err(SchemaError::Unsupported(
                        "scalar lexical profile is not yet qualified",
                    ));
                }
                "sys::Version" => {
                    json!({"type":"string","pattern":"^[0-9]+(?:\\.[0-9]+)*$(?![\\s\\S])"})
                }
                "sys::Grid" => {
                    let dict = self.ty("sys::Dict", depth + 1)?;
                    json!({"type":"object","required":["spec","cols","rows"],"properties":{"spec":{"type":"string","enum":["sys::Grid"]},"of":{"type":"string"},"meta":dict,"cols":{"type":"array","items":{"type":"object","required":["name"],"properties":{"name":{"type":"string"},"of":{"type":"string"},"meta":dict},"additionalProperties":false}},"rows":{"type":"array","items":dict}},"additionalProperties":false,"description":"Generic Grid shape. Column contexts, row identity and exact unboxing require runtime validation."})
                }
                _ if self.catalog.derives_from(name, "sys::Dict")
                    && !self.catalog.derives_from(name, "sys::Func") =>
                {
                    let mut properties = BTreeMap::new();
                    let mut required = Vec::new();
                    if name != "sys::Dict" {
                        // A base-typed slot can carry an explicitly typed derived
                        // Dict. The structural schema retains the base shape;
                        // native fitting establishes the actual nominal relation.
                        // Do not enumerate potentially hidden subtype identities.
                        properties.insert("spec".to_owned(), json!({"type":"string","description":"Optional structural type name, including admitted derived types. Native catalog fitting must establish compatibility with the declared shape."}));
                    }
                    for compiled in self.catalog.compiled[name].slots.values() {
                        let slot = &compiled.slot;
                        if slot.name == "spec" {
                            continue;
                        }
                        self.charge(1, slot.name.len() + 128)?;
                        let mut field = self.slot(slot, depth + 1)?;
                        let mut restrictions = Vec::new();
                        for pattern in &compiled.constraints.patterns {
                            let source = pattern
                                .as_str()
                                .strip_prefix("\\A(?:")
                                .and_then(|s| s.strip_suffix(")\\z"))
                                .ok_or(SchemaError::Unsupported(
                                    "compiled pattern is outside the schema profile",
                                ))?;
                            Self::pattern(source, &mut restrictions)?;
                        }
                        if let Some(min) = compiled.constraints.min {
                            restrictions.push(json!({"minimum":min}));
                        }
                        if let Some(max) = compiled.constraints.max {
                            restrictions.push(json!({"maximum":max}));
                        }
                        self.apply_constraints(&mut field, restrictions)?;
                        properties.insert(slot.name.clone(), field);
                        if !slot.is_maybe() {
                            required.push(slot.name.clone());
                        }
                    }
                    json!({"type":"object","properties":properties,"required":required,"additionalProperties":true,"description":"Open contextual Dict. Optional fields are omitted, not stored null. Extensions are generic JSON and do not establish native nominal identity or exact box:none eligibility."})
                }
                _ if self.catalog.derives_from(name, "sys::Ref") => {
                    self.ty("sys::Ref", depth + 1)?
                }
                _ if self.catalog.derives_from(name, "sys::Scalar") => {
                    json!({"type":"string","description":"Contextual nominal scalar; JSON alone does not preserve nominal identity."})
                }
                _ => {
                    return Err(SchemaError::Unsupported(
                        "type is outside the qualified schema profile",
                    ));
                }
            }
        };
        let mut constraints = Vec::new();
        let mut current = Some(declaration);
        while let Some(entry) = current {
            self.charge(1, 2048)?;
            if !self.control.visible(&entry.spec.qname) {
                return Err(SchemaError::Unsupported("dependency is not visible"));
            }
            if entry.spec.lib != "sys" {
                Self::constraints(&entry.spec.meta, &mut constraints)?;
            }
            current = entry
                .spec
                .base
                .as_deref()
                .and_then(|base| self.catalog.declaration(base));
        }
        self.apply_constraints(&mut schema, constraints)?;
        schema["x-xeto-type"] = json!(name);
        self.active.remove(name);
        self.definitions.insert(name.into(), schema);
        Ok(reference)
    }
    fn constraints(
        meta: &std::collections::HashMap<String, Kind>,
        out: &mut Vec<Value>,
    ) -> Result<(), SchemaError<C::Error>> {
        if let Some(Kind::Str(pattern)) = meta.get("pattern") {
            Self::pattern(pattern, out)?;
        }
        for (source, target) in [("minVal", "minimum"), ("maxVal", "maximum")] {
            if let Some(value) = meta.get(source) {
                let value = match value {
                    Kind::Int(v) => json!(v),
                    Kind::Number(v) => json!(v.val),
                    _ => {
                        return Err(SchemaError::Unsupported(
                            "bound is outside the schema profile",
                        ));
                    }
                };
                out.push(json!({target:value}));
            }
        }
        Ok(())
    }
    // Keep a scalar a scalar for external generators. JSON Schema's allOf is
    // an intersection, so repeated regexes use lookahead intersection and
    // numeric ranges retain their strongest endpoints.
    fn apply_constraints(
        &mut self,
        schema: &mut Value,
        restrictions: Vec<Value>,
    ) -> Result<(), SchemaError<C::Error>> {
        for restriction in restrictions {
            for (key, value) in restriction.as_object().expect("constraint object") {
                let size = value.as_str().map_or(32, str::len)
                    + schema.get(key).and_then(Value::as_str).map_or(0, str::len)
                    + 128;
                self.charge(size, size.saturating_mul(2))?;
                match (key.as_str(), schema.get(key)) {
                    ("pattern", Some(old)) if old != value => {
                        schema[key] = json!(format!(
                            "^(?={})(?={})[\\s\\S]*$(?![\\s\\S])",
                            old.as_str().expect("pattern"),
                            value.as_str().expect("pattern")
                        ));
                    }
                    ("minimum", Some(old)) if old.as_f64() > value.as_f64() => (),
                    ("maximum", Some(old)) if old.as_f64() < value.as_f64() => (),
                    _ => schema[key] = value.clone(),
                }
            }
        }
        Ok(())
    }
    fn pattern(pattern: &str, out: &mut Vec<Value>) -> Result<(), SchemaError<C::Error>> {
        // The qualified subset matches only ASCII characters. This avoids
        // Rust's scalar-value vs ECMAScript's UTF-16-code-unit quantification,
        // dot's different line terminators, and Rust-only class operations.
        // Be conservative: unsupported syntax excludes the visible root.
        let without_groups = pattern.replace("(?:", "(");
        if !pattern.is_ascii()
            || without_groups.contains("(?")
            || [
                "\\p", "\\P", "\\A", "\\z", "\\w", "\\W", "\\d", "\\D", "\\s", "\\S", "\\b", "\\B",
                "\\x{", "\\u{", "[:", "&&", "--", "~~",
            ]
            .iter()
            .any(|s| pattern.contains(s))
        {
            return Err(SchemaError::Unsupported(
                "pattern is outside the qualified ECMA subset",
            ));
        }
        let mut escaped = false;
        let mut class = false;
        let mut class_has_member = false;
        let mut can_repeat = false;
        let mut chars = pattern.chars().peekable();
        while let Some(ch) = chars.next() {
            if escaped {
                if !"\\.*+?()[]{}|^$-/nrtf".contains(ch) || (ch == '-' && !class) {
                    return Err(SchemaError::Unsupported(
                        "pattern escape is outside the qualified ECMA subset",
                    ));
                }
                escaped = false;
                class_has_member |= class;
                can_repeat = !class;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            match ch {
                '[' if class => {
                    return Err(SchemaError::Unsupported(
                        "nested pattern classes are outside the qualified ECMA subset",
                    ));
                }
                '[' => {
                    class = true;
                    class_has_member = false;
                    can_repeat = false;
                }
                ']' if class && class_has_member => {
                    class = false;
                    can_repeat = true;
                }
                ']' => {
                    return Err(SchemaError::Unsupported(
                        "literal closing brackets require an escape in the qualified ECMA subset",
                    ));
                }
                '^' => {
                    return Err(SchemaError::Unsupported(
                        "pattern anchors and negated classes are outside the qualified ECMA subset",
                    ));
                }
                '.' | '$' if !class => {
                    return Err(SchemaError::Unsupported(
                        "pattern dot and anchors are outside the qualified ECMA subset",
                    ));
                }
                '(' if !class => {
                    // The only qualified special group is noncapturing (?:).
                    if chars.peek() == Some(&'?') {
                        chars.next();
                        if chars.next() != Some(':') {
                            return Err(SchemaError::Unsupported(
                                "pattern group is outside the qualified ECMA subset",
                            ));
                        }
                    }
                    can_repeat = false;
                }
                '|' if !class => can_repeat = false,
                ')' if !class => can_repeat = true,
                '?' | '*' | '+' | '{' if !class => {
                    // Rust permits repeating a repetition; ECMA requires an
                    // atom and at most one quantifier, with one lazy suffix.
                    if !can_repeat {
                        return Err(SchemaError::Unsupported(
                            "stacked pattern repetitions are outside the qualified ECMA subset",
                        ));
                    }
                    if ch == '{' {
                        // Only {n}, {n,}, {n,m}: native admission already
                        // checks count bounds, but also accepts whitespace
                        // which ECMA does not treat as part of a quantifier.
                        let mut lower_digits = 0;
                        while chars.peek().is_some_and(char::is_ascii_digit) {
                            chars.next();
                            lower_digits += 1;
                        }
                        if lower_digits == 0 {
                            return Err(SchemaError::Unsupported(
                                "pattern count is outside the qualified ECMA subset",
                            ));
                        }
                        if chars.peek() == Some(&',') {
                            chars.next();
                            while chars.peek().is_some_and(char::is_ascii_digit) {
                                chars.next();
                            }
                        }
                        if chars.next() != Some('}') {
                            return Err(SchemaError::Unsupported(
                                "pattern count is outside the qualified ECMA subset",
                            ));
                        }
                    }
                    if chars.peek() == Some(&'?') {
                        chars.next();
                    }
                    can_repeat = false;
                }
                '}' if !class => {
                    return Err(SchemaError::Unsupported(
                        "literal pattern braces require an escape in the qualified ECMA subset",
                    ));
                }
                _ => {
                    class_has_member |= class;
                    can_repeat = !class;
                }
            }
        }
        out.push(json!({"pattern":format!("^(?:{pattern})$(?![\\s\\S])")}));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Control;
    impl SchemaControl for Control {
        type Error = ();
        fn charge(&mut self, _: usize, _: usize) -> Result<(), ()> {
            Ok(())
        }
        fn visible(&self, _: &str) -> bool {
            true
        }
        fn depth(&mut self, _: usize) -> Result<(), ()> {
            Ok(())
        }
    }
    #[test]
    fn schema_pattern_excludes_unqualified_dialect_forms() {
        for pattern in [
            "A.*", "[a[b]]", "[^A]", r"\a", "é+", r"\u0041", "[a&&b]", "[]a]", "A]",
        ] {
            assert!(
                matches!(
                    SchemaProjector::<Control>::pattern(pattern, &mut Vec::new()),
                    Err(SchemaError::Unsupported(_))
                ),
                "accepted {pattern:?}"
            );
        }
    }
    #[test]
    fn schema_pattern_excludes_native_only_repetition_and_literals() {
        let cases = [
            ("A{ 2 }", "AA"),
            ("A{2 ,3}", "AAA"),
            ("A{2, 3}", "AAA"),
            ("A{2,3 }", "AAA"),
            ("A{2 ,}", "AA"),
            ("A{\t2}", "AA"),
            ("A{2,\t3}", "AAA"),
            ("A{2,\n3}", "AAA"),
            ("A++", "AA"),
            ("A**", "AA"),
            ("A?+", "AA"),
            ("A{2}*", "AAAA"),
            ("A+??", "AA"),
            ("A{2}{3}", "AAAAAA"),
            (r"A\-B", "A-B"),
            ("A}", "A}"),
        ];
        let mut source = String::from(
            r#"pragma: Lib <version:"1.0.0", depends:{{lib:"sys",versions:"5.0.0"}}>"#,
        );
        for (i, (pattern, _)) in cases.iter().enumerate() {
            source.push_str(&format!(
                "\nCase{i}: sys::Dict {{ code:Str <pattern:{}> }}",
                serde_json::to_string(pattern).unwrap()
            ));
        }
        let catalog = Catalog::load_http_pinned()
            .unwrap()
            .with_project("quantifiers", &source, &[])
            .unwrap();
        let mut projector = SchemaProjector::new(&catalog, Control);
        let mut incorrectly_projected = Vec::new();
        for (i, (pattern, value)) in cases.iter().enumerate() {
            let name = format!("quantifiers::Case{i}");
            let mut row = crate::data::HDict::new();
            row.set("code", Kind::Str((*value).into()));
            assert!(
                catalog.fits_selected(&name, &row, None),
                "native fit: {pattern:?}"
            );
            if !matches!(
                projector.definition(&name),
                Err(SchemaError::Unsupported(_))
            ) {
                incorrectly_projected.push(*pattern);
            }
        }
        assert!(
            incorrectly_projected.is_empty(),
            "native-only syntax was projected: {incorrectly_projected:?}"
        );
    }
    #[test]
    fn schema_pattern_retains_qualified_literals_classes_and_intersections() {
        let catalog = Catalog::load_http_pinned().unwrap();
        let mut projector = SchemaProjector::new(&catalog, Control);
        let mut schema = json!({"type":"string"});
        let mut constraints = Vec::new();
        for pattern in ["[A-Z]+", "A[A-Za-z]*"] {
            SchemaProjector::<Control>::pattern(pattern, &mut constraints).unwrap();
        }
        projector
            .apply_constraints(&mut schema, constraints)
            .unwrap();
        assert_eq!(
            schema["pattern"],
            r"^(?=^(?:[A-Z]+)$(?![\s\S]))(?=^(?:A[A-Za-z]*)$(?![\s\S]))[\s\S]*$(?![\s\S])"
        );
        for (pattern, accepted, rejected) in [
            (r"A\.B", "A.B", "AXB"),
            (r"\[A\]", "[A]", "A"),
            ("(?:A|B){1,3}", "ABA", "ABAB"),
            (r"[A-Z\-]+", "AB-Z", "Ab-Z"),
        ] {
            SchemaProjector::<Control>::pattern(pattern, &mut Vec::new()).unwrap();
            let native = regex::Regex::new(&format!(r"\A(?:{pattern})\z")).unwrap();
            assert!(native.is_match(accepted));
            assert!(!native.is_match(rejected));
            assert!(!native.is_match(&format!("{accepted}\n")));
        }
    }
    #[test]
    fn schema_pattern_preserves_canonical_repetition_including_empty_values() {
        let cases: &[(&str, &[&str], &[&str])] = &[
            ("A?", &["", "A"], &["AA", "B"]),
            ("A*", &["", "AAA"], &["B"]),
            ("A+", &["A", "AAA"], &[""]),
            ("A{2}", &["AA"], &["A", "AAA"]),
            ("A{2,}", &["AA", "AAA"], &["A"]),
            ("A{2,3}", &["AA", "AAA"], &["A", "AAAA"]),
            ("A??", &["", "A"], &["AA"]),
            ("A*?", &["", "AAA"], &["B"]),
            ("A+?", &["A", "AAA"], &[""]),
            ("A{2}?", &["AA"], &["A", "AAA"]),
            ("A{2,}?", &["AA", "AAA"], &["A"]),
            ("A{2,3}?", &["AA", "AAA"], &["A", "AAAA"]),
            ("A{0}", &[""], &["A"]),
            ("A{0,0}?", &[""], &["A"]),
            ("A{0,}", &["", "AAA"], &["B"]),
            ("A{0,1}?", &["", "A"], &["AA"]),
            ("A{02,03}", &["AA", "AAA"], &["A", "AAAA"]),
            ("(?:AB){0,2}?", &["", "AB", "ABAB"], &["A", "ABABAB"]),
            ("(A+)+", &["A", "AAA"], &["", "B"]),
            ("[?*+{}-]{2}", &["*?", "{}"], &["*", "***"]),
            (r"A\{2\}", &["A{2}"], &["AA"]),
            (r"A\}B", &["A}B"], &["A}BB"]),
            ("[}]", &["}"], &["}}"]),
            (r"A[A-Z\-]+", &["A-Z"], &["A-z"]),
            ("A B{2}", &["A BB"], &["A B"]),
        ];
        for &(pattern, accepted, rejected) in cases {
            SchemaProjector::<Control>::pattern(pattern, &mut Vec::new()).unwrap();
            let native = regex::Regex::new(&format!(r"\A(?:{pattern})\z")).unwrap();
            for value in accepted {
                assert!(native.is_match(value), "{pattern:?}: rejected {value:?}");
                assert!(!native.is_match(&format!("{value}\n")));
            }
            for value in rejected {
                assert!(!native.is_match(value), "{pattern:?}: accepted {value:?}");
            }
        }
    }
}

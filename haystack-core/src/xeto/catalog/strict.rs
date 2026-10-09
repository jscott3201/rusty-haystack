//! Compiled, controlled native fitting of the selected entity closure.
use super::*;
use std::sync::Arc;

/// Integrity validation borrows raw records. Query adapters can return an
/// already-authorized owned view under this same evaluator contract.
pub enum FitRecord<'a> {
    Borrowed(&'a HDict),
    Shared(Arc<HDict>),
}
impl std::ops::Deref for FitRecord<'_> {
    type Target = HDict;
    fn deref(&self) -> &HDict {
        match self {
            Self::Borrowed(row) => row,
            Self::Shared(row) => row,
        }
    }
}
pub trait FitEnvironment<'a> {
    type Error;
    fn work(&mut self, amount: usize) -> Result<(), Self::Error>;
    fn retain(&mut self, bytes: usize) -> Result<(), Self::Error>;
    fn depth(&mut self, depth: usize) -> Result<(), Self::Error>;
    fn record(&mut self, id: &HRef) -> Result<Option<FitRecord<'a>>, Self::Error>;
}
#[derive(Debug)]
pub enum FitError<E> {
    Invalid(ProfileError),
    Control(E),
}

#[derive(Debug, Clone, Default)]
pub(super) struct Constraints {
    patterns: Vec<Regex>,
    min: Option<i64>,
    max: Option<i64>,
}
impl Constraints {
    /// `qname` is the constrained type; `located` names the declaration (and
    /// slot) whose metadata declared the constraint, for diagnostics.
    fn extend(
        &mut self,
        catalog: &Catalog,
        qname: &str,
        located: &str,
        meta: &HashMap<String, Kind>,
        source: &ProfileSource,
    ) -> Result<(), ProfileError> {
        if let Some(Kind::Str(pattern)) = meta.get("pattern") {
            // Mirrors the codec context bound so a selection that fits also
            // compiles its callable contexts.
            if pattern.len() > super::callable::MAX_PATTERN_BYTES {
                return Err(resolve_error(
                    source,
                    located,
                    "pattern exceeds the 1024-byte selected bound",
                ));
            }
            self.patterns.push(
                regex::RegexBuilder::new(&format!("\\A(?:{pattern})\\z"))
                    .size_limit(1 << 20)
                    .dfa_size_limit(1 << 20)
                    .nest_limit(64)
                    .build()
                    .map_err(|_| {
                        resolve_error(source, located, "invalid or oversized selected pattern")
                    })?,
            );
        }
        for (name, target) in [("minVal", &mut self.min), ("maxVal", &mut self.max)] {
            if let Some(value) = meta.get(name) {
                if !catalog.derives_from(qname, "sys::Int") {
                    return Err(resolve_error(
                        source,
                        located,
                        "selected bounds require an Int context",
                    ));
                }
                // Parsed source literals are Number; binding them to the Int
                // constraint is not a coercion of any stored native value.
                let bound = match value {
                    Kind::Int(value) => *value,
                    Kind::Number(value)
                        if value.unit.is_none()
                            && value.val.is_finite()
                            && value.val.fract() == 0.0
                            && value.val.abs() <= 9_007_199_254_740_991.0 =>
                    {
                        value.val as i64
                    }
                    _ => {
                        return Err(resolve_error(
                            source,
                            located,
                            "inexact Int constraint literal",
                        ));
                    }
                };
                *target = Some(match (*target, name) {
                    (Some(prior), "minVal") => prior.max(bound),
                    (Some(prior), _) => prior.min(bound),
                    (None, _) => bound,
                });
            }
        }
        if self.min.zip(self.max).is_some_and(|(min, max)| min > max) {
            return Err(resolve_error(source, located, "empty Int constraint range"));
        }
        Ok(())
    }
}
#[derive(Debug, Clone)]
pub(super) struct CompiledSlot {
    pub(super) slot: Slot,
    constraints: Constraints,
}
#[derive(Debug, Clone, Default)]
pub(super) struct CompiledType {
    pub(super) slots: BTreeMap<String, CompiledSlot>,
    constraints: Constraints,
}

impl Catalog {
    pub(super) fn compile(&self) -> Result<BTreeMap<String, CompiledType>, ProfileError> {
        let mut result = BTreeMap::new();
        for (qname, entry) in &self.specs {
            let mut chain = Vec::new();
            let mut next = Some(entry);
            while let Some(current) = next {
                if chain.len() == 64 {
                    return Err(resolve_error(
                        &entry.source,
                        qname,
                        "selected inheritance depth exceeded",
                    ));
                }
                chain.push(current);
                next = current
                    .spec
                    .base
                    .as_ref()
                    .map(|base| {
                        self.specs.get(base).ok_or_else(|| {
                            resolve_error(&entry.source, qname, "missing admitted base")
                        })
                    })
                    .transpose()?;
            }
            let mut compiled = CompiledType::default();
            for current in chain.into_iter().rev() {
                compiled.constraints.extend(
                    self,
                    qname,
                    &current.spec.qname,
                    &current.spec.meta,
                    &current.source,
                )?;
                // Enum members are finite scalar keys, not Dict field schemas.
                if self.derives_from(qname, "sys::Enum") {
                    continue;
                }
                for slot in &current.spec.slots {
                    let located = format!("{}.{}", current.spec.qname, slot.name);
                    let mut merged = slot.clone();
                    let mut constraints = Constraints::default();
                    if let Some(inherited) = compiled.slots.get(&slot.name) {
                        if !self.derives_from(slot_type(slot), slot_type(&inherited.slot)) {
                            return Err(resolve_error(
                                &current.source,
                                &located,
                                "slot refinement widens or changes its base type",
                            ));
                        }
                        let mut meta = inherited.slot.meta.clone();
                        // A derived slot may become required, but never nullable
                        // when its inherited declaration is required.
                        if slot.is_maybe() && !inherited.slot.is_maybe() {
                            return Err(resolve_error(
                                &current.source,
                                &located,
                                "slot refinement widens nullability",
                            ));
                        }
                        meta.remove("maybe");
                        meta.extend(slot.meta.clone());
                        if let (Some(Kind::Ref(old)), Some(Kind::Ref(new))) =
                            (inherited.slot.meta.get("of"), slot.meta.get("of"))
                            && !self.derives_from(&new.val, &old.val)
                        {
                            return Err(resolve_error(
                                &current.source,
                                &located,
                                "slot refinement widens reference target",
                            ));
                        }
                        merged.meta = meta;
                        if merged.default.is_none() {
                            merged.default = inherited.slot.default.clone();
                        }
                        constraints = inherited.constraints.clone();
                    }
                    constraints.extend(
                        self,
                        slot_type(slot),
                        &located,
                        &slot.meta,
                        &current.source,
                    )?;
                    compiled.slots.insert(
                        slot.name.clone(),
                        CompiledSlot {
                            slot: merged,
                            constraints,
                        },
                    );
                }
            }
            result.insert(qname.clone(), compiled);
        }
        Ok(result)
    }

    /// Validate native data without applying defaults or rewriting values.
    /// Every present structural `spec` resolves in this catalog. Entity refs
    /// resolve through the caller's controlled record view, never the catalog.
    pub fn fit_entity<'a, E: FitEnvironment<'a>>(
        &self,
        qname: &str,
        row: &HDict,
        env: &mut E,
    ) -> Result<(), FitError<E::Error>> {
        env.retain(qname.len()).map_err(FitError::Control)?;
        let mut state = Fitter {
            catalog: self,
            env,
            active: Vec::new(),
        };
        state.dict(qname, row, qname, 0)
    }

    /// Explicit marker associations are part of the selected immutable catalog.
    pub fn marker_bindings(&self) -> impl Iterator<Item = (&str, &str)> {
        self.marker_bindings
            .iter()
            .map(|(marker, qname)| (marker.as_str(), qname.as_str()))
    }

    /// Effective (inherited) declaration default of one slot. This is value
    /// construction for callers that explicitly build new data; fitting and
    /// activation never consult it and never insert it into stored records.
    pub fn slot_default(&self, qname: &str, slot: &str) -> Option<&Kind> {
        self.compiled
            .get(qname)?
            .slots
            .get(slot)?
            .slot
            .default
            .as_ref()
    }

    /// Strict selected fitting for uncontrolled legacy filter evaluation.
    /// References resolve only through `forward`; without it they never fit.
    pub fn fits_selected<'a>(
        &self,
        qname: &str,
        entity: &HDict,
        forward: Option<&'a crate::xeto::EntityResolver<'a>>,
    ) -> bool {
        struct Forward<'r, 'a> {
            forward: Option<&'r crate::xeto::EntityResolver<'a>>,
            work: usize,
        }
        impl<'a> FitEnvironment<'a> for Forward<'_, 'a> {
            type Error = ();
            fn work(&mut self, amount: usize) -> Result<(), ()> {
                self.work = self.work.saturating_add(amount);
                if self.work > LEGACY_FIT_WORK {
                    Err(())
                } else {
                    Ok(())
                }
            }
            fn retain(&mut self, _: usize) -> Result<(), ()> {
                Ok(())
            }
            fn depth(&mut self, depth: usize) -> Result<(), ()> {
                if depth > 64 { Err(()) } else { Ok(()) }
            }
            fn record(&mut self, id: &HRef) -> Result<Option<FitRecord<'a>>, ()> {
                Ok(self
                    .forward
                    .and_then(|forward| forward(id))
                    .map(FitRecord::Borrowed))
            }
        }
        self.fit_entity(qname, entity, &mut Forward { forward, work: 0 })
            .is_ok()
    }
}

/// Uncontrolled legacy evaluation has no caller budget; bound selected
/// reference-heavy fitting so it cannot run unbounded and fails closed.
const LEGACY_FIT_WORK: usize = 1 << 20;

struct Fitter<'c, 'e, E> {
    catalog: &'c Catalog,
    env: &'e mut E,
    active: Vec<(String, String)>,
}
impl<'a, E: FitEnvironment<'a>> Fitter<'_, '_, E> {
    fn check(&mut self, depth: usize) -> Result<(), FitError<E::Error>> {
        self.env.work(1).map_err(FitError::Control)?;
        self.env.depth(depth).map_err(FitError::Control)?;
        if depth > 64 {
            return Err(self.invalid("catalog", "bounded selected value", "value depth exceeded"));
        }
        Ok(())
    }
    fn invalid(&self, path: &str, expected: &str, message: &str) -> FitError<E::Error> {
        FitError::Invalid(ProfileError::Fit {
            path: "catalog".into(),
            slot: path.into(),
            expected: expected.into(),
            message: message.into(),
        })
    }
    fn child(&mut self, path: &str, slot: &str) -> Result<String, FitError<E::Error>> {
        self.env.work(slot.len()).map_err(FitError::Control)?;
        self.env
            .retain(path.len().saturating_add(slot.len()).saturating_add(1))
            .map_err(FitError::Control)?;
        Ok(format!("{path}.{slot}"))
    }
    fn dict(
        &mut self,
        expected: &str,
        row: &HDict,
        path: &str,
        depth: usize,
    ) -> Result<(), FitError<E::Error>> {
        self.check(depth)?;
        if !self.catalog.derives_from(expected, "sys::Dict") {
            return Err(self.invalid(path, expected, "expected an admitted Dict declaration"));
        }
        // An absent or explicit-null structural spec names no narrower type;
        // the declared slot then applies its own nullable rule.
        let actual = match row.get("spec") {
            None | Some(Kind::Null) => expected,
            Some(Kind::Ref(spec))
                if self.catalog.derives_from(&spec.val, expected)
                    && self.catalog.declaration(&spec.val).is_some() =>
            {
                &spec.val
            }
            Some(_) => {
                return Err(self.invalid(
                    path,
                    expected,
                    "structural spec is unknown or incompatible",
                ));
            }
        };
        let compiled = &self.catalog.compiled[actual];
        for (name, field) in &compiled.slots {
            self.check(depth)?;
            let child = self.child(path, name)?;
            match row.get(name) {
                None if field.slot.is_maybe() => {}
                None => {
                    return Err(self.invalid(
                        &child,
                        slot_type(&field.slot),
                        "missing required slot",
                    ));
                }
                Some(Kind::Null) if field.slot.is_maybe() => {}
                Some(value) => {
                    self.value(slot_type(&field.slot), value, &child, depth + 1)?;
                    self.constraints(&field.constraints, value, &child, slot_type(&field.slot))?;
                    if let Some(Kind::Ref(of)) = field.slot.meta.get("of") {
                        self.of(&of.val, value, &child, depth + 1)?;
                    }
                }
            }
        }
        self.env.work(row.scan_bound()).map_err(FitError::Control)?;
        // Undeclared extension fields remain open, but structural annotations
        // and admitted nominal values inside them cannot bypass validation.
        for (name, value) in row.iter() {
            if name == "spec" || compiled.slots.contains_key(name) {
                continue;
            }
            let child = self.child(path, name)?;
            self.extension(value, &child, depth + 1)?;
        }
        Ok(())
    }
    fn value(
        &mut self,
        qname: &str,
        value: &Kind,
        path: &str,
        depth: usize,
    ) -> Result<(), FitError<E::Error>> {
        self.check(depth)?;
        let Some(compiled) = self.catalog.compiled.get(qname) else {
            return Err(self.invalid(path, qname, "type is not admitted"));
        };
        if matches!(value, Kind::Null) {
            return Err(self.invalid(path, qname, "null is not admitted here"));
        }
        if self.catalog.derives_from(qname, "sys::Dict") {
            return match value {
                Kind::Dict(row) => self.dict(qname, row, path, depth),
                _ => Err(self.invalid(path, qname, "expected native Dict")),
            };
        }
        let valid = match qname {
            "sys::Obj" => true,
            "sys::Scalar" => !matches!(value, Kind::List(_) | Kind::Dict(_) | Kind::Grid(_)),
            "sys::Collection" => matches!(value, Kind::List(_) | Kind::Dict(_) | Kind::Grid(_)),
            "sys::Int" => matches!(value, Kind::Int(_)),
            "sys::Number" => matches!(value, Kind::Number(_)),
            "sys::Marker" => matches!(value, Kind::Marker),
            "sys::None" => matches!(value, Kind::None),
            "sys::Bool" => matches!(value, Kind::Bool(_)),
            "sys::Str" => matches!(value, Kind::Str(_)),
            "sys::Ref" => matches!(value, Kind::Ref(_)),
            "sys::Uri" => matches!(value, Kind::Uri(_)),
            "sys::DateTime" => {
                matches!(value, Kind::DateTime(time) if !time.tz_name.is_empty() && time.tz_name.bytes().all(|b| b.is_ascii_alphanumeric() || b"_+-".contains(&b)))
            }
            "sys::List" => matches!(value, Kind::List(_)),
            "sys::Grid" => matches!(value, Kind::Grid(_)),
            "sys::Enum" => {
                matches!(value, Kind::Nominal(n) if self.catalog.enums.contains_key(n.spec()) && self.catalog.nominal_text(n.spec(), value).is_some_and(|text| self.catalog.enums[n.spec()].binary_search_by(|key| key.as_str().cmp(text)).is_ok()))
            }
            "sys.api::ApiVersion" => {
                matches!(value, Kind::Str(text) if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
            }
            _ if self.catalog.derives_from(qname, "sys::Ref") => matches!(value, Kind::Ref(_)),
            _ if self.catalog.derives_from(qname, "sys::Scalar") => {
                self.catalog.nominal_text(qname, value).is_some_and(|text| {
                    self.catalog.enums.get(qname).is_none_or(|keys| {
                        keys.binary_search_by(|key| key.as_str().cmp(text)).is_ok()
                    })
                })
            }
            _ => false,
        };
        if !valid {
            return Err(self.invalid(path, qname, "native kind or nominal identity does not fit"));
        }
        self.constraints(&compiled.constraints, value, path, qname)?;
        if matches!(
            qname,
            "sys::Obj" | "sys::Collection" | "sys::List" | "sys::Grid" | "sys::Scalar"
        ) {
            self.extension(value, path, depth + 1)?;
        }
        Ok(())
    }
    fn constraints(
        &mut self,
        constraints: &Constraints,
        value: &Kind,
        path: &str,
        qname: &str,
    ) -> Result<(), FitError<E::Error>> {
        if let Kind::Int(value) = value
            && (constraints.min.is_some_and(|min| *value < min)
                || constraints.max.is_some_and(|max| *value > max))
        {
            return Err(self.invalid(path, qname, "integer is outside selected bounds"));
        }
        // Native numeric types have already been decoded and checked; their
        // lexical source grammar is not reapplied through a lossy string cast.
        let text = match value {
            Kind::Str(value) => Some(value.as_str()),
            Kind::Ref(value) => Some(value.val.as_str()),
            Kind::Nominal(value) => Some(value.text()),
            Kind::Bool(value) => Some(if *value { "true" } else { "false" }),
            Kind::Marker => Some("✓"),
            Kind::None => Some("∅"),
            _ => None,
        };
        if let Some(text) = text {
            for pattern in &constraints.patterns {
                self.env
                    .work(text.len().saturating_add(1))
                    .map_err(FitError::Control)?;
                if !pattern.is_match(text) {
                    return Err(self.invalid(
                        path,
                        qname,
                        "scalar text does not match selected pattern",
                    ));
                }
            }
        }
        Ok(())
    }
    fn of(
        &mut self,
        target: &str,
        value: &Kind,
        path: &str,
        depth: usize,
    ) -> Result<(), FitError<E::Error>> {
        self.check(depth)?;
        match value {
            Kind::Ref(reference) if target == "sys::Spec" => {
                if self.catalog.declaration(&reference.val).is_none() {
                    return Err(self.invalid(path, target, "catalog reference does not resolve"));
                }
            }
            Kind::Ref(reference) => {
                self.env
                    .work(reference.val.len().saturating_add(target.len()))
                    .map_err(FitError::Control)?;
                let record = self
                    .env
                    .record(reference)
                    .map_err(FitError::Control)?
                    .ok_or_else(|| {
                        self.invalid(path, target, "entity reference does not resolve")
                    })?;
                let Some(Kind::Ref(actual)) = record.get("spec") else {
                    return Err(self.invalid(
                        path,
                        target,
                        "entity reference target has no structural spec",
                    ));
                };
                if !self.catalog.derives_from(&actual.val, target)
                    || self.catalog.declaration(&actual.val).is_none()
                {
                    return Err(self.invalid(
                        path,
                        target,
                        "entity reference target spec is incompatible",
                    ));
                }
                if record.id().is_none_or(|id| id.val != reference.val) {
                    return Err(self.invalid(
                        path,
                        target,
                        "entity reference identity does not match",
                    ));
                }
                if self
                    .active
                    .iter()
                    .any(|(id, expected)| id == &reference.val && expected == target)
                {
                    return Ok(());
                }
                self.env
                    .retain(
                        reference
                            .val
                            .len()
                            .saturating_add(target.len())
                            .saturating_add(std::mem::size_of::<(String, String)>()),
                    )
                    .map_err(FitError::Control)?;
                self.active.push((reference.val.clone(), target.into()));
                let fitted = self.dict(target, &record, path, depth + 1);
                self.active.pop();
                fitted?;
            }
            Kind::List(values) => {
                for item in values {
                    self.value(target, item, path, depth + 1)?;
                }
            }
            Kind::Grid(grid) => {
                for row in &grid.rows {
                    self.dict(target, row, path, depth + 1)?;
                }
            }
            _ => {
                return Err(self.invalid(
                    path,
                    target,
                    "of constraint requires Ref, List, or Grid",
                ));
            }
        }
        Ok(())
    }
    fn extension(
        &mut self,
        value: &Kind,
        path: &str,
        depth: usize,
    ) -> Result<(), FitError<E::Error>> {
        self.check(depth)?;
        match value {
            Kind::Dict(row) => self.dict("sys::Dict", row, path, depth + 1)?,
            Kind::List(values) => {
                for value in values {
                    self.extension(value, path, depth + 1)?;
                }
            }
            Kind::Grid(grid) => {
                self.dict("sys::Dict", &grid.meta, path, depth + 1)?;
                for column in &grid.cols {
                    self.dict("sys::Dict", &column.meta, path, depth + 1)?;
                }
                for row in &grid.rows {
                    self.dict("sys::Dict", row, path, depth + 1)?;
                }
            }
            Kind::Nominal(nominal) => self.value(nominal.spec(), value, path, depth + 1)?,
            _ => {}
        }
        Ok(())
    }
}

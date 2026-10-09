//! Executable support is a fixed application inventory of native handlers.
//! Each graph-published catalog observation supplies the signatures, metadata
//! and codec contexts those handlers are bound to; a binding never outlives or
//! mixes observations.
use crate::{
    ApiError, BudgetKind, CatalogKind, PolicySnapshot, ReadError, ReadOperation, budget::Budget,
    typed_http::WireProfile,
};
use haystack_core::{
    data::{HDict, HGrid},
    kinds::{HRef, Kind},
    xeto::catalog::{ActivatedCatalog, AdmittedSpec, Catalog},
};
use std::{collections::BTreeSet, sync::Arc};

/// The bounded supported-handler inventory. Routing is derived from this list,
/// never from a parsed catalog, so a newly admitted declaration cannot create
/// an executable route.
pub(crate) const BINDINGS: &[(&str, Handler)] = &[
    ("sys.api::readById", Handler::ReadById),
    ("sys.api::ops", Handler::Ops),
    ("sys.api::readByIds", Handler::ReadByIds),
    ("sys.api::read", Handler::Read),
    ("sys.api::readAll", Handler::ReadAll),
    ("sys.api::about", Handler::About),
    ("sys.api::close", Handler::Close),
    ("sys.api::libs", Handler::Libs),
    ("sys.api::filetypes", Handler::Filetypes),
];

/// Stable identity supplied to the explicit per-function execution decision.
/// Catalog visibility and coarse read permission do not grant this decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionIdentity {
    pub qname: String,
    pub library_version: String,
    pub catalog: String,
    pub revision: String,
    pub source_path: String,
    pub source_sha256: String,
}

/// Borrowed transport-neutral descriptor of an installed executable binding.
/// Enumeration is trusted application configuration, not caller authorization.
pub struct FunctionDescriptor<'a> {
    pub identity: &'a FunctionIdentity,
    pub name: &'a str,
    pub doc: Option<&'a str>,
    pub signature: &'a str,
    pub no_side_effects: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Handler {
    ReadById,
    ReadByIds,
    Read,
    ReadAll,
    About,
    Close,
    Libs,
    Filetypes,
    Ops,
}
pub(crate) struct Entry {
    pub identity: FunctionIdentity,
    name: String,
    doc: Option<String>,
    signature: String,
    op: bool,
    no_side_effects: bool,
    pub handler: Handler,
    pub wire: WireProfile,
}
impl Entry {
    fn visible(&self, policy: &dyn PolicySnapshot) -> bool {
        self.op
            && policy.operation(ReadOperation::Read)
            && policy.catalog(CatalogKind::Spec, &self.identity.qname)
            && policy.function(&self.identity)
    }
    pub fn permits_method(&self, post: bool) -> Result<(), ApiError> {
        if post || self.no_side_effects {
            Ok(())
        } else {
            Err(ApiError::MethodNotAllowed)
        }
    }
    fn descriptor(&self) -> FunctionDescriptor<'_> {
        FunctionDescriptor {
            identity: &self.identity,
            name: &self.name,
            doc: self.doc.as_deref(),
            signature: &self.signature,
            no_side_effects: self.no_side_effects,
        }
    }
}

pub(crate) struct Registry {
    observation: Arc<ActivatedCatalog>,
    entries: Vec<Entry>,
}

/// Retained, owned view of typed bindings for one catalog observation. The
/// descriptors borrow this view, never a temporary or permanent registry.
pub struct TypedFunctions(Option<Arc<Registry>>);
impl TypedFunctions {
    pub(crate) fn new(registry: Option<Arc<Registry>>) -> Self {
        Self(registry)
    }
    pub fn iter(&self) -> impl Iterator<Item = FunctionDescriptor<'_>> {
        self.0.iter().flat_map(|registry| registry.descriptors())
    }
    /// Digest of the observation these descriptors were bound to, if any.
    pub fn selection_identity(&self) -> Option<&str> {
        self.0
            .as_ref()
            .map(|registry| registry.observation.selection_identity())
    }
}
/// Why the fixed handler inventory cannot bind to an observation. Carries the
/// declaration identity rather than a generic service-limits error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BindError {
    pub declaration: String,
    pub reason: &'static str,
}
impl BindError {
    fn new(declaration: &str, reason: &'static str) -> Self {
        Self {
            declaration: declaration.into(),
            reason,
        }
    }
}
impl Registry {
    /// Bind the complete fixed inventory to one observation. Any missing or
    /// incompatible handler signature rejects the whole observation. Service
    /// construction keeps reporting this as `InvalidLimits`; activation uses
    /// [`Registry::bind_checked`] and reports the declaration.
    pub fn bind(observation: Arc<ActivatedCatalog>) -> Result<Self, ReadError> {
        Self::bind_checked(observation).map_err(|_| ReadError::InvalidLimits)
    }
    pub fn bind_checked(observation: Arc<ActivatedCatalog>) -> Result<Self, BindError> {
        Self::bind_with(observation, BINDINGS)
    }
    pub fn observation(&self) -> &Arc<ActivatedCatalog> {
        &self.observation
    }
    pub fn catalog(&self) -> &Catalog {
        self.observation.catalog()
    }
    fn bind_with(
        observation: Arc<ActivatedCatalog>,
        bindings: &[(&str, Handler)],
    ) -> Result<Self, BindError> {
        let entries = Self::entries(&observation, bindings)?;
        Ok(Self {
            observation,
            entries,
        })
    }
    fn entries(
        observation: &ActivatedCatalog,
        bindings: &[(&str, Handler)],
    ) -> Result<Vec<Entry>, BindError> {
        let profile = observation.catalog();
        let mut identities = BTreeSet::new();
        let mut entries = Vec::new();
        for &(qname, handler) in bindings {
            let declaration = profile.declaration(qname).ok_or_else(|| {
                BindError::new(qname, "supported handler declaration is not admitted")
            })?;
            if validate_binding(declaration, handler).is_err() {
                return Err(BindError::new(
                    qname,
                    "admitted signature does not match the supported handler",
                ));
            }
            let version = &profile
                .libraries()
                .find(|lib| lib.name == declaration.spec.lib)
                .ok_or_else(|| BindError::new(qname, "declaring library is not admitted"))?
                .version;
            if !identities.insert((qname, version.clone())) {
                return Err(BindError::new(qname, "duplicate handler binding"));
            }
            let signature = signature(declaration);
            entries.push(Entry {
                identity: FunctionIdentity {
                    qname: qname.into(),
                    library_version: version.clone(),
                    catalog: profile.provenance().repository.clone(),
                    revision: profile.provenance().commit.clone(),
                    source_path: declaration.source.path.clone(),
                    source_sha256: declaration.source.sha256.clone(),
                },
                name: declaration.spec.name.clone(),
                doc: Some(
                    declaration
                        .spec
                        .doc
                        .split('.')
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_owned(),
                )
                .filter(|doc| !doc.is_empty()),
                signature,
                op: declaration.spec.meta.get("op") == Some(&Kind::Marker),
                no_side_effects: declaration.spec.meta.get("noSideEffects") == Some(&Kind::Marker),
                handler,
                wire: observation
                    .callable(qname)
                    .ok_or_else(|| BindError::new(qname, "no compiled callable context"))?
                    .clone(),
            });
        }
        entries.sort_by(|a, b| a.identity.qname.cmp(&b.identity.qname));
        Ok(entries)
    }
    #[cfg(test)]
    pub(crate) fn pinned() -> Result<Self, ReadError> {
        let catalog = Catalog::load_http_pinned().map_err(|_| ReadError::InvalidLimits)?;
        let observation =
            ActivatedCatalog::new(catalog, None).map_err(|_| ReadError::InvalidLimits)?;
        Self::bind(Arc::new(observation))
    }
    #[cfg(test)]
    pub(crate) fn disable_read_get_for_test(&mut self) {
        self.entries
            .iter_mut()
            .find(|entry| entry.handler == Handler::ReadById)
            .unwrap()
            .no_side_effects = false;
    }
    pub fn descriptors(&self) -> impl Iterator<Item = FunctionDescriptor<'_>> {
        self.entries
            .iter()
            .filter(|entry| entry.op)
            .map(Entry::descriptor)
    }
    pub fn resolve(
        &self,
        name: &str,
        policy: &dyn PolicySnapshot,
        budget: &mut Budget,
    ) -> Result<&Entry, ApiError> {
        let mut found: Option<&Entry> = None;
        let mut candidates = Vec::new();
        for entry in &self.entries {
            // Scan and identity comparisons cost work even for hidden entries.
            budget.charge(BudgetKind::Candidates, 1)?;
            budget.charge(
                BudgetKind::Work,
                entry
                    .identity
                    .qname
                    .len()
                    .saturating_add(name.len())
                    .saturating_add(1),
            )?;
            if (entry.identity.qname == name || entry.name == name) && entry.visible(policy) {
                if let Some(previous) = found {
                    if candidates.is_empty() {
                        budget.charge(BudgetKind::Retained, 128)?;
                        candidates.push(budget.copy_string(&previous.identity.qname)?);
                    }
                    budget.charge(BudgetKind::Retained, 64)?;
                    candidates.push(budget.copy_string(&entry.identity.qname)?);
                } else {
                    found = Some(entry);
                }
            }
        }
        if !candidates.is_empty() {
            return Err(ApiError::AmbiguousFunction {
                name: budget.copy_string(name)?,
                candidates,
            });
        }
        match found {
            Some(entry) => Ok(entry),
            None => Err(ApiError::UnknownFunction(budget.copy_string(name)?)),
        }
    }
    pub fn ops(&self, policy: &dyn PolicySnapshot, budget: &mut Budget) -> Result<Kind, ApiError> {
        let mut rows = Vec::new();
        for entry in &self.entries {
            // All inspected entries, including denied entries, count. Copied
            // metadata is reserved from its admitted size, never body length.
            budget.charge(BudgetKind::Candidates, 1)?;
            budget.charge(
                BudgetKind::Work,
                entry.identity.qname.len().saturating_add(1),
            )?;
            if !entry.visible(policy) {
                continue;
            }
            if rows.len() >= budget.limits.max_rows {
                return Err(ReadError::Budget(BudgetKind::Rows).into());
            }
            budget.charge(BudgetKind::Retained, 2048)?;
            budget.charge(BudgetKind::Values, 5)?;
            let mut row = HDict::new();
            row.set(
                "qname",
                Kind::Str(budget.copy_string(&entry.identity.qname)?),
            );
            row.set(
                "signature",
                Kind::Str(budget.copy_string(&entry.signature)?),
            );
            if let Some(doc) = &entry.doc {
                row.set("doc", Kind::Str(budget.copy_string(doc)?));
            }
            if entry.no_side_effects {
                row.set("noSideEffects", Kind::Marker);
            }
            // Bound row fitting before the profile walks any produced fields.
            budget.charge(BudgetKind::Work, row.len().saturating_mul(8))?;
            rows.push(row);
        }
        let mut grid: HGrid = crate::output::grid(rows, true, None, budget)?;
        grid.meta.remove_tag("complete");
        budget.charge(BudgetKind::Retained, 128)?;
        grid.meta
            .set("of", Kind::Ref(HRef::from_val("sys.api::OpInfo")));
        Ok(Kind::Grid(Box::new(grid)))
    }
}
fn validate_binding(declaration: &AdmittedSpec, handler: Handler) -> Result<(), ReadError> {
    // Handler contracts are checked independently of provenance admission.
    let shape = |expected: &[(&str, &str, bool, bool, Option<&str>)]| {
        declaration.spec.slots.len() == expected.len()
            && expected.iter().all(|(name, ty, maybe, default_true, of)| {
                declaration
                    .spec
                    .slots
                    .iter()
                    .find(|s| s.name == *name)
                    .is_some_and(|slot| {
                        slot.type_ref.as_deref() == Some(*ty)
                            && slot.is_maybe() == *maybe
                            && if *default_true {
                                slot.default == Some(Kind::Bool(true))
                            } else {
                                slot.default.is_none()
                            }
                            && match (of, slot.meta.get("of")) {
                                (Some(of), Some(Kind::Ref(found))) => found.val == *of,
                                (None, None) => true,
                                _ => false,
                            }
                    })
            })
    };
    let slots = &declaration.spec.slots;
    let valid = declaration.member_of.as_deref() == Some("sys::Funcs")
        && declaration.spec.base.as_deref() == Some("sys::Func")
        && declaration.spec.meta.get("op") == Some(&Kind::Marker)
        && match handler {
            Handler::ReadById => {
                slots.len() == 3
                    && slots.iter().all(|s| match s.name.as_str() {
                        "id" => {
                            s.type_ref.as_deref() == Some("sys::Ref")
                                && s.is_maybe()
                                && s.default.is_none()
                        }
                        "checked" => {
                            s.type_ref.as_deref() == Some("sys::Bool")
                                && !s.is_maybe()
                                && s.default == Some(Kind::Bool(true))
                        }
                        "returns" => {
                            s.type_ref.as_deref() == Some("sys::Dict")
                                && s.is_maybe()
                                && s.default.is_none()
                        }
                        _ => false,
                    })
            }
            Handler::ReadByIds => {
                declaration.spec.qname == "sys.api::readByIds"
                    && shape(&[
                        ("ids", "sys::List", false, false, Some("sys::Ref")),
                        ("checked", "sys::Bool", false, true, None),
                        ("returns", "sys::Grid", false, false, None),
                    ])
            }
            Handler::Read => {
                declaration.spec.qname == "sys.api::read"
                    && shape(&[
                        ("filter", "sys::Filter", false, false, None),
                        ("checked", "sys::Bool", false, true, None),
                        ("returns", "sys::Dict", true, false, None),
                    ])
            }
            Handler::ReadAll => {
                declaration.spec.qname == "sys.api::readAll"
                    && shape(&[
                        ("filter", "sys::Filter", false, false, None),
                        ("opts", "sys::Dict", true, false, None),
                        ("returns", "sys::Grid", false, false, None),
                    ])
            }
            Handler::About => {
                declaration.spec.qname == "sys.api::about"
                    && shape(&[("returns", "sys.api::AboutInfo", false, false, None)])
            }
            Handler::Close => {
                declaration.spec.qname == "sys.api::close"
                    && shape(&[("returns", "sys::None", false, false, None)])
            }
            Handler::Libs => {
                declaration.spec.qname == "sys.api::libs"
                    && shape(&[(
                        "returns",
                        "sys::Grid",
                        false,
                        false,
                        Some("sys.api::LibInfo"),
                    )])
            }
            Handler::Filetypes => {
                declaration.spec.qname == "sys.api::filetypes"
                    && shape(&[(
                        "returns",
                        "sys::Grid",
                        false,
                        false,
                        Some("sys.api::FiletypeInfo"),
                    )])
            }
            Handler::Ops => {
                slots.len() == 1
                    && slots[0].name == "returns"
                    && slots[0].type_ref.as_deref() == Some("sys::Grid")
                    && !slots[0].is_maybe()
                    && slots[0].default.is_none()
                    && slots[0].meta.get("of")
                        == Some(&Kind::Ref(HRef::from_val("sys.api::OpInfo")))
            }
        };
    if valid {
        Ok(())
    } else {
        Err(ReadError::InvalidLimits)
    }
}
fn signature(declaration: &AdmittedSpec) -> String {
    let type_name = |slot: &haystack_core::xeto::spec::Slot| {
        let mut name = slot
            .type_ref
            .as_deref()
            .unwrap_or("sys::Marker")
            .rsplit("::")
            .next()
            .unwrap_or("")
            .to_owned();
        if let Some(Kind::Ref(of)) = slot.meta.get("of") {
            name.push_str(&format!(
                "<of:{}>",
                of.val.rsplit("::").next().unwrap_or("")
            ));
        }
        if slot.is_maybe() {
            name.push('?');
        }
        name
    };
    let args = declaration
        .spec
        .slots
        .iter()
        .filter(|slot| slot.name != "returns")
        .map(|slot| format!("{}: {}", slot.name, type_name(slot)))
        .collect::<Vec<_>>()
        .join(", ");
    let returns = declaration
        .spec
        .slots
        .iter()
        .find(|slot| slot.name == "returns")
        .expect("validated signature");
    format!("({args}) -> {}", type_name(returns))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AllowAll, CancellationToken, ReadLimits};
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };
    fn budget(limits: ReadLimits) -> Budget {
        Budget::new(
            Arc::new(limits),
            Instant::now() + Duration::from_secs(5),
            CancellationToken::new(),
        )
    }
    struct Rules {
        denied: Option<&'static str>,
        catalog: bool,
    }
    impl PolicySnapshot for Rules {
        fn scope_key(&self) -> &str {
            "registry-fixture"
        }
        fn function(&self, f: &FunctionIdentity) -> bool {
            self.denied
                .is_none_or(|denied| !f.qname.starts_with(denied))
        }
        fn operation(&self, _: ReadOperation) -> bool {
            true
        }
        fn entity(&self, _: &str) -> bool {
            true
        }
        fn tag(&self, _: &str, _: &str) -> bool {
            true
        }
        fn reference(&self, _: &str) -> bool {
            true
        }
        fn reference_display(&self, _: &str) -> bool {
            true
        }
        fn catalog(&self, _: CatalogKind, _: &str) -> bool {
            self.catalog
        }
        fn nominal_provenance(&self, _: &haystack_core::kinds::NominalScalar) -> bool {
            true
        }
    }
    // Synthetic lookup fixtures exercise collision and metadata decisions;
    // they are never admitted as an upstream library or executable handler.
    fn fixture_entry(qname: &str) -> Entry {
        let mut entry = Registry::pinned().unwrap().entries.remove(1);
        entry.identity.qname = qname.into();
        entry.name = qname.rsplit("::").next().unwrap().into();
        entry
    }
    #[test]
    fn duplicate_bindings_and_handler_shape_mismatch_fail_before_publication() {
        let observation = || Registry::pinned().unwrap().observation.clone();
        assert!(
            Registry::bind_with(
                observation(),
                &[
                    ("sys.api::ops", Handler::Ops),
                    ("sys.api::ops", Handler::Ops)
                ]
            )
            .is_err()
        );
        assert!(
            Registry::bind_with(observation(), &[("sys.api::ops", Handler::ReadById)]).is_err()
        );
        assert!(
            Registry::bind_with(observation(), &[("sys.api::readById", Handler::Ops)]).is_err()
        );
        assert!(Registry::bind_with(observation(), &[("sys.api::close", Handler::Ops)]).is_err());
        // An observation without the fixed supported signatures cannot bind.
        let read_by_id_only =
            Arc::new(ActivatedCatalog::new(Catalog::load_pinned().unwrap(), None).unwrap());
        assert!(Registry::bind(read_by_id_only.clone()).is_err());
        // Activation reports the first missing supported declaration.
        assert_eq!(
            Registry::bind_checked(read_by_id_only).err(),
            Some(BindError::new(
                "sys.api::ops",
                "supported handler declaration is not admitted"
            ))
        );
        assert_eq!(
            Registry::bind_with(observation(), &[("sys.api::close", Handler::Ops)])
                .err()
                .map(|error| error.reason),
            Some("admitted signature does not match the supported handler")
        );
        let mut declaration = observation()
            .catalog()
            .declaration("sys.api::readById")
            .unwrap()
            .clone();
        declaration.spec.meta.remove("op");
        assert!(validate_binding(&declaration, Handler::ReadById).is_err());
    }
    #[test]
    fn visible_simple_name_resolution_matches_discovery_and_ambiguity_reveals_only_visible_candidates()
     {
        let mut registry = Registry::pinned().unwrap();
        registry.entries = vec![
            fixture_entry("a::same"),
            fixture_entry("b::same"),
            fixture_entry("hidden::same"),
        ];
        let policy = Rules {
            denied: Some("hidden::"),
            catalog: true,
        };
        let fresh = || budget(ReadLimits::default());
        assert_eq!(
            registry
                .resolve("a::same", &policy, &mut fresh())
                .unwrap()
                .identity
                .qname,
            "a::same"
        );
        assert!(matches!(
            registry.resolve("hidden::same", &policy, &mut fresh()),
            Err(ApiError::UnknownFunction(_))
        ));
        let error = registry
            .resolve("same", &policy, &mut fresh())
            .err()
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&error.json()).unwrap();
        assert_eq!(
            value["candidates"],
            serde_json::json!(["a::same", "b::same"])
        );
        let Kind::Grid(grid) = registry.ops(&policy, &mut fresh()).unwrap() else {
            panic!()
        };
        assert_eq!(
            grid.rows
                .iter()
                .map(|row| row.get("qname").unwrap().clone())
                .collect::<Vec<_>>(),
            [Kind::Str("a::same".into()), Kind::Str("b::same".into())]
        );
        let policy = Rules {
            denied: Some("b::"),
            catalog: true,
        };
        registry.entries.pop();
        assert_eq!(
            registry
                .resolve("same", &policy, &mut fresh())
                .unwrap()
                .identity
                .qname,
            "a::same"
        );
        registry.entries[0].op = false;
        assert!(registry.resolve("same", &policy, &mut fresh()).is_err());
        let catalog_hidden = Rules {
            denied: None,
            catalog: false,
        };
        assert!(
            registry
                .resolve("b::same", &catalog_hidden, &mut fresh())
                .is_err()
        );
    }
    #[test]
    fn get_requires_exact_no_side_effects_marker() {
        let profile = Catalog::load_http_pinned().unwrap();
        let mut declaration = profile.declaration("sys.api::readById").unwrap().clone();
        declaration.spec.meta.remove("noSideEffects");
        validate_binding(&declaration, Handler::ReadById).unwrap();
        let mut entry = fixture_entry("fixture::writeLike");
        entry.no_side_effects = declaration.spec.meta.get("noSideEffects") == Some(&Kind::Marker);
        assert_eq!(entry.permits_method(false), Err(ApiError::MethodNotAllowed));
        assert_eq!(entry.permits_method(true), Ok(()));
        declaration
            .spec
            .meta
            .insert("noSideEffects".into(), Kind::Bool(true));
        entry.no_side_effects = declaration.spec.meta.get("noSideEffects") == Some(&Kind::Marker);
        assert_eq!(entry.permits_method(false), Err(ApiError::MethodNotAllowed));
    }
    #[test]
    fn hidden_scans_and_source_sized_metadata_are_charged_before_output() {
        let mut registry = Registry::pinned().unwrap();
        registry
            .ops(
                &AllowAll,
                &mut budget(ReadLimits {
                    max_retained_bytes: 65_536,
                    ..ReadLimits::default()
                }),
            )
            .unwrap();
        assert!(
            registry
                .ops(
                    &AllowAll,
                    &mut budget(ReadLimits {
                        max_rows: 1,
                        ..ReadLimits::default()
                    })
                )
                .is_err()
        );
        registry.entries[0].doc = Some("x".repeat(100_000));
        assert!(
            registry
                .ops(
                    &AllowAll,
                    &mut budget(ReadLimits {
                        max_retained_bytes: 65_536,
                        ..ReadLimits::default()
                    })
                )
                .is_err()
        );
        assert!(
            registry
                .ops(
                    &AllowAll,
                    &mut budget(ReadLimits {
                        max_work: 4_000,
                        ..ReadLimits::default()
                    })
                )
                .is_err()
        );
        registry.entries = (0..8)
            .map(|n| fixture_entry(&format!("hidden::entry{n}")))
            .collect();
        assert!(
            registry
                .ops(
                    &Rules {
                        denied: Some("hidden::"),
                        catalog: true
                    },
                    &mut budget(ReadLimits {
                        max_candidates: 3,
                        ..ReadLimits::default()
                    })
                )
                .is_err()
        );
        let mut cancelled = budget(ReadLimits::default());
        cancelled.cancel.cancel();
        assert!(registry.ops(&AllowAll, &mut cancelled).is_err());
    }
}

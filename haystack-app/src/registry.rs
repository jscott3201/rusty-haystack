//! Executable support is an application decision made once at construction.
//! The admitted catalog supplies signatures; immutable bindings supply handlers.
use crate::{
    ApiError, BudgetKind, CatalogKind, PolicySnapshot, ReadError, ReadOperation, budget::Budget,
    typed_http::WireProfile,
};
use haystack_core::{
    data::{HDict, HGrid},
    kinds::{HRef, Kind},
    xeto::read_by_id::{AdmittedSpec, ReadByIdProfile},
};
use std::collections::BTreeSet;

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
    pub profile: ReadByIdProfile,
    entries: Vec<Entry>,
}
impl Registry {
    pub fn pinned() -> Result<Self, ReadError> {
        let profile = ReadByIdProfile::load_http_pinned().map_err(|_| ReadError::InvalidLimits)?;
        Self::bind(
            profile,
            &[
                ("sys.api::readById", Handler::ReadById),
                ("sys.api::ops", Handler::Ops),
            ],
        )
    }
    fn bind(profile: ReadByIdProfile, bindings: &[(&str, Handler)]) -> Result<Self, ReadError> {
        let mut identities = BTreeSet::new();
        let mut entries = Vec::new();
        for &(qname, handler) in bindings {
            let declaration = profile.declaration(qname).ok_or(ReadError::InvalidLimits)?;
            validate_binding(declaration, handler)?;
            let version = &profile
                .libraries()
                .find(|lib| lib.name == declaration.spec.lib)
                .ok_or(ReadError::InvalidLimits)?
                .version;
            if !identities.insert((qname, version.clone())) {
                return Err(ReadError::InvalidLimits);
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
                wire: WireProfile::new(&profile, declaration)?,
            });
        }
        entries.sort_by(|a, b| a.identity.qname.cmp(&b.identity.qname));
        Ok(Self { profile, entries })
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
        let profile = || ReadByIdProfile::load_http_pinned().unwrap();
        assert!(
            Registry::bind(
                profile(),
                &[
                    ("sys.api::ops", Handler::Ops),
                    ("sys.api::ops", Handler::Ops)
                ]
            )
            .is_err()
        );
        assert!(Registry::bind(profile(), &[("sys.api::ops", Handler::ReadById)]).is_err());
        assert!(Registry::bind(profile(), &[("sys.api::readById", Handler::Ops)]).is_err());
        assert!(Registry::bind(profile(), &[("sys.api::close", Handler::Ops)]).is_err());
        let mut declaration = profile().declaration("sys.api::readById").unwrap().clone();
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
        let profile = ReadByIdProfile::load_http_pinned().unwrap();
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
                    max_retained_bytes: 16_384,
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
                        max_retained_bytes: 16_384,
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

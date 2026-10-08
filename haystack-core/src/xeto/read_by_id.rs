//! Admission of one pinned Xeto function closure, independent of the legacy
//! compatibility loader. This is not a complete `sys` or `sys.api` library.
//!
//! Source integrity, parsing, resolution and value fitting have separate errors.
//! The immutable handle owns the declarations used by both discovery and fitting.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use regex::Regex;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::data::HDict;
use crate::kinds::{HRef, Kind};

use super::XetoError;
use super::ast::{LibPragma, SlotDef, XetoFile};
use super::parser::parse_xeto;
use super::spec::{Slot, Spec, spec_from_def};

/// The compatibility pin; updating it requires a reviewed profile update.
pub const READ_BY_ID_UPSTREAM_COMMIT: &str = "873b922451d3ef4c0c9c08ef3daa542f352d69f3";
const PROFILE: &str = "pinned-xeto-readById";
const HTTP_PROFILE: &str = "pinned-xeto-readById-http";
const HTTP_TYPES: &[&str] = &["Number", "Int", "List"];
const HTTP_ERRORS: &[&str] = &[
    "ApiErr",
    "AuthErr",
    "InternalErr",
    "InvalidArgsErr",
    "InvalidPathErr",
    "NotAcceptableErr",
    "NotImplementedErr",
    "PermissionErr",
    "TimeoutErr",
    "UnavailableErr",
    "UnknownEntityErr",
    "UnknownFuncErr",
    "UnsupportedMediaTypeErr",
    "UnsupportedVersionErr",
];
const HTTP_META: &[&str] = &["of", "unitless"];
const HTTP_MANIFEST: &str = include_str!("../../xeto-profiles/read-by-id/http-manifest.json");
const HTTP_RAW: &[(&str, &str)] = &[
    (
        "src/xeto/sys.api/errs.xeto",
        include_str!("../../xeto-profiles/read-by-id/upstream/src/xeto/sys.api/errs.xeto"),
    ),
    (
        "src/xeto/sys.api/types.xeto",
        include_str!("../../xeto-profiles/read-by-id/upstream/src/xeto/sys.api/types.xeto"),
    ),
];
const FUNCTION: &str = "sys.api::readById";
const TYPES: &[&str] = &[
    "Obj",
    "Scalar",
    "Marker",
    "Str",
    "Bool",
    "Ref",
    "Collection",
    "Dict",
    "Func",
    "Funcs",
    "Interface",
];
const META: &[&str] = &[
    "abstract",
    "doc",
    "maybe",
    "mixin",
    "noSideEffects",
    "noInherit",
    "op",
    "pattern",
    "sealed",
    "val",
];
const MANIFEST: &str = include_str!("../../xeto-profiles/read-by-id/manifest.json");
const RAW: &[(&str, &str)] = &[
    (
        "LICENSE",
        include_str!("../../xeto-profiles/read-by-id/upstream/LICENSE"),
    ),
    (
        "src/xeto/xeto-build.props",
        include_str!("../../xeto-profiles/read-by-id/upstream/src/xeto/xeto-build.props"),
    ),
    (
        "src/xeto/sys/lib.xeto",
        include_str!("../../xeto-profiles/read-by-id/upstream/src/xeto/sys/lib.xeto"),
    ),
    (
        "src/xeto/sys.api/lib.xeto",
        include_str!("../../xeto-profiles/read-by-id/upstream/src/xeto/sys.api/lib.xeto"),
    ),
    (
        "src/xeto/sys/types.xeto",
        include_str!("../../xeto-profiles/read-by-id/upstream/src/xeto/sys/types.xeto"),
    ),
    (
        "src/xeto/sys/spec.xeto",
        include_str!("../../xeto-profiles/read-by-id/upstream/src/xeto/sys/spec.xeto"),
    ),
    (
        "src/xeto/sys.api/funcs.xeto",
        include_str!("../../xeto-profiles/read-by-id/upstream/src/xeto/sys.api/funcs.xeto"),
    ),
];

/// Full raw-file integrity and the exact inclusive, one-based extraction ranges.
#[derive(Debug, Clone, Deserialize)]
pub struct ProfileSource {
    pub path: String,
    pub sha256: String,
    pub role: String,
    pub library: String,
    pub lines: Vec<[usize; 2]>,
}

/// Provenance of this bootstrap subset. No field claims full-library support.
#[derive(Debug, Clone, Deserialize)]
pub struct ProfileProvenance {
    pub profile: String,
    pub repository: String,
    pub commit: String,
    pub license: String,
    pub complete_libraries: bool,
    pub extraction: String,
    pub files: Vec<ProfileSource>,
}

/// Failures identify the stage and source without embedding request values.
#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error("profile source {path}: {message}")]
    Source { path: String, message: String },
    #[error("profile parse {path}:{line}:{col}: {message}")]
    Parse {
        path: String,
        line: usize,
        col: usize,
        message: String,
    },
    #[error("profile resolve {path} [{declaration}]: {message}")]
    Resolve {
        path: String,
        declaration: String,
        message: String,
    },
    #[error("profile fit {path} [{slot}]: expected {expected}; {message}")]
    Fit {
        path: String,
        slot: String,
        expected: String,
        message: String,
    },
}

/// An admitted declaration and its declaring library/source membership.
#[derive(Debug, Clone)]
pub struct AdmittedSpec {
    pub spec: Spec,
    pub source: ProfileSource,
    /// Target of the source augmentation, e.g. `sys::Funcs`.
    pub member_of: Option<String>,
}

/// Selected metadata field from the upstream Spec schema. This does not admit
/// `sys::Spec` itself or claim its remaining fields are supported.
#[derive(Debug, Clone)]
pub struct AdmittedMetadata {
    pub slot: Slot,
    pub source: ProfileSource,
}

/// A library view generated from this handle's actual admitted declarations.
#[derive(Debug, Clone)]
pub struct AdmittedLibrary {
    pub name: String,
    pub version: String,
    pub maturity: String,
    pub depends: Vec<String>,
    pub declarations: Vec<String>,
    pub metadata_fields: Vec<String>,
    pub complete: bool,
}

/// An augmentation is retained separately from the type it augments.
#[derive(Debug, Clone)]
pub struct AdmittedAugmentation {
    pub library: String,
    pub target: String,
    pub meta: HashMap<String, Kind>,
    pub members: Vec<String>,
    pub source: ProfileSource,
}

/// Where a bound argument came from. An explicit null remains distinguishable
/// from an absent nullable parameter even though both bind to `Kind::Null`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgumentOrigin {
    Explicit,
    ParameterDefault,
    MissingNullable,
}

/// Bound native values plus their presence/default provenance.
#[derive(Debug, Clone)]
pub struct BoundArguments {
    values: HDict,
    origins: BTreeMap<String, ArgumentOrigin>,
}

impl BoundArguments {
    pub fn values(&self) -> &HDict {
        &self.values
    }
    pub fn origin(&self, name: &str) -> Option<ArgumentOrigin> {
        self.origins.get(name).copied()
    }
}

/// Immutable, checksum-verified catalog for the pinned readById admission profile.
#[derive(Debug)]
pub struct ReadByIdProfile {
    provenance: ProfileProvenance,
    libraries: BTreeMap<String, AdmittedLibrary>,
    specs: BTreeMap<String, AdmittedSpec>,
    metadata: BTreeMap<String, AdmittedMetadata>,
    augmentations: Vec<AdmittedAugmentation>,
}

impl ReadByIdProfile {
    /// Verify the retained upstream bytes, reproduce the selected source slices,
    /// expand their pinned build variables, parse and resolve the entire closure.
    /// No network access, Python process or mutable global namespace is involved.
    pub fn load_pinned() -> Result<Self, ProfileError> {
        let provenance: ProfileProvenance = serde_json::from_str(MANIFEST)
            .map_err(|e| source_error("manifest.json", e.to_string()))?;
        let sources = extract_sources(&provenance, RAW)?;
        Self::admit(provenance, sources)
    }

    /// Admit the reachable HTTP error closure in addition to the unchanged
    /// native signature. This remains an explicit subset of both libraries.
    pub fn load_http_pinned() -> Result<Self, ProfileError> {
        let provenance = serde_json::from_str(HTTP_MANIFEST)
            .map_err(|e| source_error("http-manifest.json", e.to_string()))?;
        let raw: Vec<_> = RAW.iter().chain(HTTP_RAW).copied().collect();
        let sources = extract_sources(&provenance, &raw)?;
        Self::admit(provenance, sources)
    }

    /// Fit a terminal error's declared fields, including inherited ApiErr fields.
    /// `spec` is structural wire information and is supplied as the qname.
    pub fn fit_api_error(&self, qname: &str, fields: &HDict) -> Result<(), ProfileError> {
        let concrete = self
            .specs
            .get(qname)
            .filter(|s| s.spec.lib == "sys.api" && HTTP_ERRORS.contains(&s.spec.name.as_str()))
            .ok_or_else(|| source_error(HTTP_PROFILE, "unadmitted error type"))?;
        let mut slots = BTreeMap::new();
        let mut entry = Some(concrete);
        while let Some(current) = entry {
            for slot in &current.spec.slots {
                slots.entry(slot.name.as_str()).or_insert(slot);
            }
            entry = current
                .spec
                .base
                .as_ref()
                .and_then(|name| self.specs.get(name));
        }
        for name in fields.tag_names() {
            if !slots.contains_key(name) {
                return Err(fit_error(
                    concrete,
                    name,
                    "declared field",
                    "unknown error field",
                ));
            }
        }
        for (name, slot) in slots {
            match fields.get(name) {
                None if slot.is_maybe() => {}
                None => {
                    return Err(fit_error(
                        concrete,
                        name,
                        slot_type(slot),
                        "missing error field",
                    ));
                }
                Some(value) => {
                    self.fit_slot(concrete, slot, value)?;
                    if let (Some(Kind::Ref(of)), Kind::List(values)) = (slot.meta.get("of"), value)
                    {
                        for item in values {
                            if !self.fits_type(&of.val, item) {
                                return Err(fit_error(
                                    concrete,
                                    name,
                                    &of.val,
                                    "invalid list item",
                                ));
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub fn provenance(&self) -> &ProfileProvenance {
        &self.provenance
    }
    pub fn libraries(&self) -> impl Iterator<Item = &AdmittedLibrary> {
        self.libraries.values()
    }
    pub fn declarations(&self) -> impl Iterator<Item = &AdmittedSpec> {
        self.specs.values()
    }
    pub fn declaration(&self, qname: &str) -> Option<&AdmittedSpec> {
        self.specs.get(qname)
    }
    pub fn metadata(&self) -> impl Iterator<Item = &AdmittedMetadata> {
        self.metadata.values()
    }
    pub fn augmentations(&self) -> &[AdmittedAugmentation] {
        &self.augmentations
    }

    /// Operations are derived from admitted +Funcs members and their own metadata.
    pub fn operations(&self) -> impl Iterator<Item = &AdmittedSpec> {
        self.specs.values().filter(|s| {
            s.member_of.as_deref() == Some("sys::Funcs") && s.spec.meta.contains_key("op")
        })
    }

    /// Effective metadata follows the admitted base chain. The selected schema's
    /// noInherit fields (abstract/sealed) are removed at each inheritance step.
    pub fn effective_metadata(&self, qname: &str) -> Option<HashMap<String, Kind>> {
        let mut chain = Vec::new();
        let mut next = Some(qname);
        while let Some(name) = next {
            let spec = &self.specs.get(name)?.spec;
            chain.push(spec);
            next = spec.base.as_deref();
        }
        let mut meta = HashMap::new();
        for spec in chain.into_iter().rev() {
            meta.retain(|name, _| {
                !self
                    .metadata
                    .get(name)
                    .is_some_and(|m| m.slot.meta.contains_key("noInherit"))
            });
            meta.extend(spec.meta.clone());
        }
        Some(meta)
    }

    /// Fit and bind native function arguments. Only defaults declared on the
    /// parameter itself apply; scalar construction defaults are never borrowed.
    /// Unknown arguments, including `returns`, are rejected. Input is unchanged.
    pub fn fit_arguments(
        &self,
        operation: &str,
        args: &HDict,
    ) -> Result<BoundArguments, ProfileError> {
        let function = self.operation(operation)?;
        let parameters: Vec<_> = function
            .spec
            .slots
            .iter()
            .filter(|s| s.name != "returns")
            .collect();
        for name in args.tag_names() {
            if !parameters.iter().any(|s| s.name == name) {
                return Err(fit_error(
                    function,
                    name,
                    "declared parameter",
                    "unknown argument",
                ));
            }
        }
        let mut values = HDict::new();
        let mut origins = BTreeMap::new();
        for slot in parameters {
            let (value, origin) = if let Some(value) = args.get(&slot.name) {
                (value.clone(), ArgumentOrigin::Explicit)
            } else if let Some(value) = &slot.default {
                (value.clone(), ArgumentOrigin::ParameterDefault)
            } else if slot.is_maybe() {
                (Kind::Null, ArgumentOrigin::MissingNullable)
            } else {
                return Err(fit_error(
                    function,
                    &slot.name,
                    slot_type(slot),
                    "missing required argument",
                ));
            };
            self.fit_slot(function, slot, &value)?;
            values.set(&slot.name, value);
            origins.insert(slot.name.clone(), origin);
        }
        Ok(BoundArguments { values, origins })
    }

    /// Fit the native result against the admitted returns member. A Dict is
    /// unconstrained here: rich Kind values within it are preserved as-is.
    pub fn fit_result(&self, operation: &str, value: &Kind) -> Result<(), ProfileError> {
        let function = self.operation(operation)?;
        let slot = function
            .spec
            .slots
            .iter()
            .find(|s| s.name == "returns")
            .expect("admission requires a returns member");
        self.fit_slot(function, slot, value)
    }

    fn operation(&self, qname: &str) -> Result<&AdmittedSpec, ProfileError> {
        self.operations()
            .find(|s| s.spec.qname == qname)
            .ok_or_else(|| ProfileError::Fit {
                path: PROFILE.into(),
                slot: qname.into(),
                expected: "admitted operation".into(),
                message: "operation is not in this profile".into(),
            })
    }

    fn fit_slot(
        &self,
        function: &AdmittedSpec,
        slot: &Slot,
        value: &Kind,
    ) -> Result<(), ProfileError> {
        if matches!(value, Kind::Null) && slot.is_maybe() {
            return Ok(());
        }
        if self.fits_type(slot_type(slot), value) {
            return Ok(());
        }
        Err(fit_error(
            function,
            &slot.name,
            slot_type(slot),
            "present value has the wrong kind or scalar encoding",
        ))
    }

    fn fits_type(&self, qname: &str, value: &Kind) -> bool {
        if matches!(value, Kind::Null) {
            return false;
        }
        let mut current = Some(qname);
        while let Some(name) = current {
            let Some(declaration) = self.specs.get(name) else {
                return false;
            };
            match (name, value) {
                ("sys::Int", Kind::Int(_)) | ("sys::Number", Kind::Number(_)) => return true,
                ("sys::Int" | "sys::Number", _) => return false,
                ("sys::List", Kind::List(_)) => return true,
                ("sys::List", _) => return false,
                ("sys.api::ApiVersion", Kind::Str(text)) => {
                    return !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
                }
                ("sys.api::ApiVersion", _) => return false,
                _ => {}
            }
            let scalar_text = match (name, value) {
                ("sys::Bool", Kind::Bool(b)) => Some(if *b { "true" } else { "false" }),
                ("sys::Ref", Kind::Ref(r)) => Some(r.val.as_str()),
                ("sys::Str", Kind::Str(s)) => Some(s.as_str()),
                ("sys::Marker", Kind::Marker) => Some("✓"),
                ("sys::Bool" | "sys::Ref" | "sys::Str" | "sys::Marker", _) => return false,
                ("sys::Dict", _) => return matches!(value, Kind::Dict(_)),
                ("sys::Obj", _) => return true,
                _ => None,
            };
            if let Some(text) = scalar_text {
                return match declaration.spec.meta.get("pattern") {
                    Some(Kind::Str(pattern)) => Regex::new(&format!("\\A(?:{pattern})\\z"))
                        .is_ok_and(|re| re.is_match(text)),
                    None => true,
                    _ => false,
                };
            }
            current = declaration.spec.base.as_deref();
        }
        false
    }

    // Private: callers cannot supply altered sources and still obtain a handle
    // which claims the pinned provenance. Tests exercise stages with local input.
    fn admit(
        provenance: ProfileProvenance,
        sources: Vec<ExtractedSource>,
    ) -> Result<Self, ProfileError> {
        let http = provenance.profile == HTTP_PROFILE;
        let parsed = sources
            .iter()
            .map(|s| s.parse().map(|ast| (s, ast)))
            .collect::<Result<Vec<_>, _>>()?;
        let mut pragmas = BTreeMap::new();
        let mut specs = BTreeMap::new();
        let mut metadata = BTreeMap::new();
        let mut augmentations = Vec::new();
        for (source, ast) in parsed {
            let identity = &source.identity;
            match identity.role.as_str() {
                "library" => {
                    if !ast.specs.is_empty() {
                        return Err(resolve_error(
                            identity,
                            "pragma",
                            "declarations in library metadata are not admitted",
                        ));
                    }
                    let pragma = ast.pragma.ok_or_else(|| {
                        resolve_error(identity, "pragma", "missing library pragma")
                    })?;
                    validate_pragma(identity, &pragma)?;
                    if pragmas
                        .insert(identity.library.clone(), (pragma, identity.clone()))
                        .is_some()
                    {
                        return Err(resolve_error(identity, "pragma", "duplicate library"));
                    }
                }
                "types" => {
                    reject_pragma(identity, &ast)?;
                    for def in ast.specs {
                        if identity.library != "sys"
                            || def.is_augmentation
                            || !(TYPES.contains(&def.name.as_str())
                                || http && HTTP_TYPES.contains(&def.name.as_str()))
                        {
                            return Err(resolve_error(
                                identity,
                                &def.name,
                                "declaration is not admitted by this profile",
                            ));
                        }
                        validate_slot_syntax(&def.slots, identity, &def.name)?;
                        let spec = spec_from_def(&def, &identity.library);
                        insert_spec(
                            &mut specs,
                            AdmittedSpec {
                                spec,
                                source: identity.clone(),
                                member_of: None,
                            },
                        )?;
                    }
                }
                "errors" | "api-types" if http => {
                    reject_pragma(identity, &ast)?;
                    for def in ast.specs {
                        if identity.library != "sys.api"
                            || def.is_augmentation
                            || !(identity.role == "errors"
                                && HTTP_ERRORS.contains(&def.name.as_str())
                                || identity.role == "api-types" && def.name == "ApiVersion")
                        {
                            return Err(resolve_error(
                                identity,
                                &def.name,
                                "unadmitted HTTP declaration",
                            ));
                        }
                        validate_slot_syntax(&def.slots, identity, &def.name)?;
                        insert_spec(
                            &mut specs,
                            AdmittedSpec {
                                spec: spec_from_def(&def, &identity.library),
                                source: identity.clone(),
                                member_of: None,
                            },
                        )?;
                    }
                }
                "metadata" => {
                    reject_pragma(identity, &ast)?;
                    if ast.specs.len() != 1
                        || ast.specs[0].name != "Spec"
                        || ast.specs[0].is_augmentation
                    {
                        return Err(resolve_error(
                            identity,
                            "Spec",
                            "expected the selected Spec metadata fields",
                        ));
                    }
                    let def = &ast.specs[0];
                    if def.base.as_deref() != Some("Dict")
                        || def.default.is_some()
                        || def.meta.len() != 1
                        || def.meta.get("sealed") != Some(&Kind::Marker)
                    {
                        return Err(resolve_error(
                            identity,
                            "Spec",
                            "unsupported metadata schema wrapper",
                        ));
                    }
                    for field in &def.slots {
                        if !(META.contains(&field.name.as_str())
                            || http && HTTP_META.contains(&field.name.as_str()))
                            || !field.children.is_empty()
                            || field.is_global
                            || field.is_query
                        {
                            return Err(resolve_error(
                                identity,
                                &field.name,
                                "metadata field is not admitted",
                            ));
                        }
                        if metadata
                            .insert(
                                field.name.clone(),
                                AdmittedMetadata {
                                    slot: Slot::from(field),
                                    source: identity.clone(),
                                },
                            )
                            .is_some()
                        {
                            return Err(resolve_error(
                                identity,
                                &field.name,
                                "duplicate metadata field",
                            ));
                        }
                    }
                }
                "functions" => {
                    reject_pragma(identity, &ast)?;
                    if ast.specs.len() != 1 {
                        return Err(resolve_error(
                            identity,
                            "+Funcs",
                            "expected one augmentation",
                        ));
                    }
                    let def = &ast.specs[0];
                    if identity.library != "sys.api"
                        || !def.is_augmentation
                        || def.name != "Funcs"
                        || def.base.is_some()
                        || def.default.is_some()
                        || !def.meta.is_empty()
                    {
                        return Err(resolve_error(
                            identity,
                            &def.name,
                            "only the selected +Funcs augmentation is admitted",
                        ));
                    }
                    let mut members = Vec::new();
                    validate_slot_syntax(&def.slots, identity, "+Funcs")?;
                    for member in &def.slots {
                        if member.name != "readById"
                            || member.is_global
                            || member.is_query
                            || member.is_marker
                            || member.is_maybe
                            || member.default.is_some()
                        {
                            return Err(resolve_error(
                                identity,
                                &member.name,
                                "function member is not admitted",
                            ));
                        }
                        let qname = format!("{}::{}", identity.library, member.name);
                        let mut spec = Spec::new(&qname, &identity.library, &member.name);
                        spec.base = member.type_ref.clone();
                        spec.meta = member.meta.clone();
                        spec.slots = member.children.iter().map(Slot::from).collect();
                        spec.doc = member.doc.clone();
                        members.push(qname);
                        insert_spec(
                            &mut specs,
                            AdmittedSpec {
                                spec,
                                source: identity.clone(),
                                member_of: Some("sys::Funcs".into()),
                            },
                        )?;
                    }
                    augmentations.push(AdmittedAugmentation {
                        library: identity.library.clone(),
                        target: "sys::Funcs".into(),
                        meta: HashMap::from([("mixin".into(), Kind::Marker)]),
                        members,
                        source: identity.clone(),
                    });
                }
                _ => return Err(resolve_error(identity, "source", "unsupported source role")),
            }
        }
        for required in ["sys", "sys.api"] {
            if !pragmas.contains_key(required) {
                return Err(ProfileError::Resolve {
                    path: format!("src/xeto/{required}/lib.xeto"),
                    declaration: required.into(),
                    message: "missing required library dependency".into(),
                });
            }
        }
        if pragmas.len() != 2 {
            return Err(source_error(PROFILE, "unadmitted library"));
        }
        for (name, (pragma, source)) in &pragmas {
            for dependency in &pragma.depends {
                let Some((dep, _)) = pragmas.get(dependency) else {
                    return Err(resolve_error(
                        source,
                        name,
                        format!("missing dependency '{dependency}'"),
                    ));
                };
                if dependency == name {
                    return Err(resolve_error(source, name, "cyclic library dependency"));
                }
                if dependency_version(pragma, dependency) != Some(dep.version.as_str()) {
                    return Err(resolve_error(
                        source,
                        name,
                        format!("unsupported dependency version for '{dependency}'"),
                    ));
                }
            }
        }
        if !pragmas["sys"].0.depends.is_empty() || pragmas["sys.api"].0.depends != ["sys"] {
            return Err(resolve_error(
                &pragmas["sys.api"].1,
                "sys.api",
                "profile requires exactly the sys dependency",
            ));
        }
        let mut names = BTreeSet::new();
        for entry in specs.values() {
            names.insert(entry.spec.qname.clone());
            collect_slot_names(
                &entry.spec.qname,
                &entry.spec.slots,
                &mut names,
                &entry.source,
            )?;
        }
        for entry in specs.values_mut() {
            if let Some(base) = &entry.spec.base {
                entry.spec.base = Some(resolve_name(
                    base,
                    &entry.spec.lib,
                    &pragmas,
                    &names,
                    &entry.source,
                    &entry.spec.qname,
                )?);
            }
            resolve_slots(
                &mut entry.spec.slots,
                &entry.spec.lib,
                &entry.spec.qname,
                &pragmas,
                &names,
                &entry.source,
            )?;
        }
        for field in metadata.values_mut() {
            resolve_slots(
                std::slice::from_mut(&mut field.slot),
                "sys",
                "sys::Spec",
                &pragmas,
                &names,
                &field.source,
            )?;
        }
        for augmentation in &augmentations {
            resolve_name(
                &augmentation.target,
                &augmentation.library,
                &pragmas,
                &names,
                &augmentation.source,
                "+Funcs",
            )?;
        }
        if http {
            for entry in specs.values_mut() {
                resolve_of(
                    &mut entry.spec.meta,
                    &entry.spec.lib,
                    &pragmas,
                    &names,
                    &entry.source,
                    &entry.spec.qname,
                    false,
                )?;
                resolve_slot_of(
                    &mut entry.spec.slots,
                    &entry.spec.lib,
                    &pragmas,
                    &names,
                    &entry.source,
                    &entry.spec.qname,
                )?;
            }
            for field in metadata.values_mut() {
                // The selected metadata schema is a bootstrap schema, never a
                // claim to have admitted complete sys::Spec.
                resolve_of(
                    &mut field.slot.meta,
                    "sys",
                    &pragmas,
                    &names,
                    &field.source,
                    &field.slot.name,
                    field.slot.name == "of",
                )?;
            }
        }
        validate_closure(&specs, &metadata, http)?;
        validate_cycles(&specs)?;
        // Decode declarations only after names and the complete base graph resolve.
        let type_graph = specs.clone();
        for entry in specs.values_mut() {
            validate_meta(
                &entry.spec.meta,
                &metadata,
                &entry.source,
                &entry.spec.qname,
            )?;
            if let Some(raw) = entry.spec.meta.get("val") {
                let value = decode_default(
                    raw,
                    &entry.spec.qname,
                    &type_graph,
                    &entry.source,
                    &entry.spec.qname,
                )?;
                entry.spec.meta.insert("val".into(), value);
            }
            decode_slots(
                &mut entry.spec.slots,
                &type_graph,
                &metadata,
                &entry.source,
                &entry.spec.qname,
            )?;
        }
        for field in metadata.values() {
            validate_meta(&field.slot.meta, &metadata, &field.source, &field.slot.name)?;
        }
        let mut libraries = BTreeMap::new();
        for (name, (pragma, _)) in pragmas {
            libraries.insert(
                name.clone(),
                AdmittedLibrary {
                    name: name.clone(),
                    version: pragma.version,
                    maturity: match pragma.meta.get("maturity") {
                        Some(Kind::Str(v)) => v.clone(),
                        _ => unreachable!(),
                    },
                    depends: pragma.depends,
                    declarations: specs
                        .values()
                        .filter(|s| s.spec.lib == name)
                        .map(|s| s.spec.qname.clone())
                        .collect(),
                    metadata_fields: metadata
                        .values()
                        .filter(|m| m.source.library == name)
                        .map(|m| m.slot.name.clone())
                        .collect(),
                    complete: false,
                },
            );
        }
        let profile = Self {
            provenance,
            libraries,
            specs,
            metadata,
            augmentations,
        };
        for entry in profile.specs.values() {
            if let Some(value) = entry.spec.meta.get("val")
                && !profile.fits_type(&entry.spec.qname, value)
            {
                return Err(resolve_error(
                    &entry.source,
                    &entry.spec.qname,
                    "default does not fit the declared type",
                ));
            }
            for slot in &entry.spec.slots {
                if let Some(value) = &slot.default
                    && !profile.fits_type(slot_type(slot), value)
                {
                    return Err(resolve_error(
                        &entry.source,
                        format!("{}.{}", entry.spec.qname, slot.name),
                        "default does not fit the declared type",
                    ));
                }
            }
        }
        Ok(profile)
    }
}

fn source_error(path: impl Into<String>, message: impl Into<String>) -> ProfileError {
    ProfileError::Source {
        path: path.into(),
        message: message.into(),
    }
}
fn resolve_error(
    source: &ProfileSource,
    declaration: impl Into<String>,
    message: impl Into<String>,
) -> ProfileError {
    ProfileError::Resolve {
        path: source.path.clone(),
        declaration: declaration.into(),
        message: message.into(),
    }
}
fn fit_error(function: &AdmittedSpec, slot: &str, expected: &str, message: &str) -> ProfileError {
    ProfileError::Fit {
        path: function.source.path.clone(),
        slot: format!("{}.{}", function.spec.qname, slot),
        expected: expected.into(),
        message: message.into(),
    }
}
fn slot_type(slot: &Slot) -> &str {
    slot.type_ref.as_deref().unwrap_or("sys::Marker")
}

#[derive(Debug, Clone)]
struct ExtractedSource {
    identity: ProfileSource,
    text: String,
    original_lines: Vec<usize>,
}
impl ExtractedSource {
    fn parse(&self) -> Result<XetoFile, ProfileError> {
        parse_xeto(&self.text).map_err(|error| match error {
            XetoError::Parse { line, col, message } => ProfileError::Parse {
                path: self.identity.path.clone(),
                line: self
                    .original_lines
                    .get(line.saturating_sub(1))
                    .copied()
                    .unwrap_or(line),
                col,
                message,
            },
            other => source_error(&self.identity.path, other.to_string()),
        })
    }
}

fn extract_sources(
    provenance: &ProfileProvenance,
    raw: &[(&str, &str)],
) -> Result<Vec<ExtractedSource>, ProfileError> {
    if provenance.commit != READ_BY_ID_UPSTREAM_COMMIT
        || !matches!(provenance.profile.as_str(), PROFILE | HTTP_PROFILE)
        || provenance.repository != "https://github.com/Project-Haystack/xeto"
        || provenance.complete_libraries
    {
        return Err(source_error(
            "manifest.json",
            "unsupported provenance or complete-library claim",
        ));
    }
    if raw.len() != provenance.files.len() {
        return Err(source_error("manifest.json", "source inventory mismatch"));
    }
    let mut paths = BTreeSet::new();
    for source in &provenance.files {
        if !paths.insert(&source.path) {
            return Err(source_error(&source.path, "duplicate source path"));
        }
        let text = raw
            .iter()
            .find(|(path, _)| *path == source.path)
            .ok_or_else(|| source_error(&source.path, "raw source is missing"))?
            .1;
        let hash: String = Sha256::digest(text.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        if hash != source.sha256 {
            return Err(source_error(&source.path, "SHA-256 mismatch"));
        }
    }
    let props = raw
        .iter()
        .find(|(path, _)| *path == "src/xeto/xeto-build.props")
        .ok_or_else(|| source_error(PROFILE, "missing pinned build properties"))?
        .1;
    let mut variables = BTreeMap::new();
    for line in props
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("//"))
    {
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| source_error("xeto-build.props", "malformed build property"))?;
        if variables.insert(key, value).is_some() {
            return Err(source_error("xeto-build.props", "duplicate build property"));
        }
    }
    let mut result = Vec::new();
    for source in &provenance.files {
        if matches!(source.role.as_str(), "license" | "build") {
            continue;
        }
        let raw = raw
            .iter()
            .find(|(path, _)| *path == source.path)
            .expect("source inventory verified")
            .1;
        let lines: Vec<_> = raw.lines().collect();
        let mut text = String::new();
        let mut original_lines = Vec::new();
        for &[first, last] in &source.lines {
            if first == 0 || last < first || last > lines.len() {
                return Err(source_error(&source.path, "invalid extraction range"));
            }
            for number in first..=last {
                text.push_str(lines[number - 1]);
                text.push('\n');
                original_lines.push(number);
            }
            text.push('\n');
            original_lines.push(last);
        }
        if text.is_empty() {
            return Err(source_error(&source.path, "empty source selection"));
        }
        text = expand_build_vars(&text, &variables, &source.path)?;
        result.push(ExtractedSource {
            identity: source.clone(),
            text,
            original_lines,
        });
    }
    Ok(result)
}

fn expand_build_vars(
    text: &str,
    variables: &BTreeMap<&str, &str>,
    path: &str,
) -> Result<String, ProfileError> {
    let pattern = Regex::new(r#"BuildVar\s+"([^"]+)""#).expect("constant expression");
    let mut output = String::new();
    let mut offset = 0;
    for capture in pattern.captures_iter(text) {
        let span = capture.get(0).expect("whole match");
        output.push_str(&text[offset..span.start()]);
        let value = variables.get(&capture[1]).ok_or_else(|| {
            source_error(path, format!("unresolved build variable '{}'", &capture[1]))
        })?;
        output.push_str(&serde_json::to_string(value).expect("string encoding"));
        offset = span.end();
    }
    output.push_str(&text[offset..]);
    Ok(output)
}

fn reject_pragma(source: &ProfileSource, ast: &XetoFile) -> Result<(), ProfileError> {
    if ast.pragma.is_some() {
        return Err(resolve_error(
            source,
            "pragma",
            "unexpected pragma in selected declarations",
        ));
    }
    Ok(())
}
fn insert_spec(
    specs: &mut BTreeMap<String, AdmittedSpec>,
    entry: AdmittedSpec,
) -> Result<(), ProfileError> {
    if specs.contains_key(&entry.spec.qname) {
        return Err(resolve_error(
            &entry.source,
            &entry.spec.qname,
            "duplicate declaration",
        ));
    }
    specs.insert(entry.spec.qname.clone(), entry);
    Ok(())
}

type Pragmas = BTreeMap<String, (LibPragma, ProfileSource)>;
fn validate_pragma(source: &ProfileSource, pragma: &LibPragma) -> Result<(), ProfileError> {
    if !matches!(source.library.as_str(), "sys" | "sys.api")
        || !pragma.name.is_empty()
        || pragma.version != "5.0.0"
        || pragma.meta.get("maturity") != Some(&Kind::Str("alpha".into()))
    {
        return Err(resolve_error(
            source,
            "pragma",
            "profile requires the pinned 5.0.0 alpha library metadata",
        ));
    }
    for name in pragma.meta.keys() {
        if ![
            "doc",
            "version",
            "maturity",
            "depends",
            "categories",
            "license",
            "org",
            "vcs",
        ]
        .contains(&name.as_str())
        {
            return Err(resolve_error(
                source,
                "pragma",
                format!("unsupported library metadata '{name}'"),
            ));
        }
    }
    if let Some(depends) = pragma.meta.get("depends") {
        let Kind::List(entries) = depends else {
            return Err(resolve_error(
                source,
                "depends",
                "expected a dependency list",
            ));
        };
        let mut seen = BTreeSet::new();
        for entry in entries {
            let Kind::Dict(dict) = entry else {
                return Err(resolve_error(
                    source,
                    "depends",
                    "expected dependency dictionary",
                ));
            };
            let Some(Kind::Str(lib)) = dict.get("lib") else {
                return Err(resolve_error(source, "depends", "missing dependency name"));
            };
            if !seen.insert(lib)
                || dict.len() != 2
                || !matches!(dict.get("versions"), Some(Kind::Str(_)))
            {
                return Err(resolve_error(
                    source,
                    "depends",
                    "unsupported or duplicate dependency declaration",
                ));
            }
        }
    }
    Ok(())
}
fn dependency_version<'a>(pragma: &'a LibPragma, name: &str) -> Option<&'a str> {
    let Kind::List(entries) = pragma.meta.get("depends")? else {
        return None;
    };
    entries.iter().find_map(|entry| {
        let Kind::Dict(dict) = entry else {
            return None;
        };
        if dict.get("lib") != Some(&Kind::Str(name.into())) {
            return None;
        }
        match dict.get("versions") {
            Some(Kind::Str(s)) => Some(s.as_str()),
            _ => None,
        }
    })
}

fn collect_slot_names(
    parent: &str,
    slots: &[Slot],
    names: &mut BTreeSet<String>,
    source: &ProfileSource,
) -> Result<(), ProfileError> {
    for slot in slots {
        let qname = format!("{parent}.{}", slot.name);
        if !names.insert(qname.clone()) {
            return Err(resolve_error(source, &qname, "duplicate member"));
        }
        collect_slot_names(&qname, &slot.children, names, source)?;
    }
    Ok(())
}
fn resolve_name(
    name: &str,
    library: &str,
    pragmas: &Pragmas,
    names: &BTreeSet<String>,
    source: &ProfileSource,
    declaration: &str,
) -> Result<String, ProfileError> {
    let (pragma, _) = pragmas.get(library).ok_or_else(|| {
        resolve_error(source, declaration, "declaring library has not been loaded")
    })?;
    if let Some((target_lib, _)) = name.split_once("::") {
        if (target_lib == library || pragma.depends.iter().any(|d| d == target_lib))
            && names.contains(name)
        {
            return Ok(name.into());
        }
        return Err(resolve_error(
            source,
            declaration,
            format!("unresolved or undeclared dependency reference '{name}'"),
        ));
    }
    let own = format!("{library}::{name}");
    if names.contains(&own) {
        return Ok(own);
    }
    let candidates: Vec<_> = pragma
        .depends
        .iter()
        .map(|dep| format!("{dep}::{name}"))
        .filter(|n| names.contains(n))
        .collect();
    if candidates.len() == 1 {
        return Ok(candidates[0].clone());
    }
    Err(resolve_error(
        source,
        declaration,
        format!("unresolved or ambiguous reference '{name}'"),
    ))
}
fn resolve_slots(
    slots: &mut [Slot],
    library: &str,
    parent: &str,
    pragmas: &Pragmas,
    names: &BTreeSet<String>,
    source: &ProfileSource,
) -> Result<(), ProfileError> {
    for slot in slots {
        let qname = format!("{parent}.{}", slot.name);
        if slot.is_query {
            return Err(resolve_error(
                source,
                &qname,
                "query slots are not admitted",
            ));
        }
        let name = slot
            .type_ref
            .as_deref()
            .unwrap_or(if slot.is_marker { "Marker" } else { "" });
        slot.type_ref = Some(resolve_name(name, library, pragmas, names, source, &qname)?);
        resolve_slots(&mut slot.children, library, &qname, pragmas, names, source)?;
    }
    Ok(())
}
fn resolve_of(
    meta: &mut HashMap<String, Kind>,
    library: &str,
    pragmas: &Pragmas,
    names: &BTreeSet<String>,
    source: &ProfileSource,
    declaration: &str,
    schema: bool,
) -> Result<(), ProfileError> {
    if let Some(value) = meta.get("of") {
        let Kind::Str(name) = value else {
            return Err(resolve_error(
                source,
                declaration,
                "of requires a type reference",
            ));
        };
        let qname = if schema && name == "Spec" {
            "sys::Spec".into()
        } else {
            resolve_name(name, library, pragmas, names, source, declaration)?
        };
        meta.insert("of".into(), Kind::Ref(HRef::from_val(qname)));
    }
    Ok(())
}
fn resolve_slot_of(
    slots: &mut [Slot],
    library: &str,
    pragmas: &Pragmas,
    names: &BTreeSet<String>,
    source: &ProfileSource,
    parent: &str,
) -> Result<(), ProfileError> {
    for slot in slots {
        let qname = format!("{parent}.{}", slot.name);
        resolve_of(
            &mut slot.meta,
            library,
            pragmas,
            names,
            source,
            &qname,
            false,
        )?;
        resolve_slot_of(&mut slot.children, library, pragmas, names, source, &qname)?;
    }
    Ok(())
}
fn validate_closure(
    specs: &BTreeMap<String, AdmittedSpec>,
    metadata: &BTreeMap<String, AdmittedMetadata>,
    http: bool,
) -> Result<(), ProfileError> {
    for name in TYPES {
        if !specs.contains_key(&format!("sys::{name}")) {
            return Err(source_error(
                PROFILE,
                format!("missing admitted type sys::{name}"),
            ));
        }
    }
    for name in META {
        if !metadata.contains_key(*name) {
            return Err(source_error(
                PROFILE,
                format!("missing metadata definition '{name}'"),
            ));
        }
    }
    if http {
        for name in HTTP_TYPES {
            if !specs.contains_key(&format!("sys::{name}")) {
                return Err(source_error(HTTP_PROFILE, "missing HTTP carrier"));
            }
        }
        for name in HTTP_ERRORS.iter().chain([&"ApiVersion"]) {
            if !specs.contains_key(&format!("sys.api::{name}")) {
                return Err(source_error(HTTP_PROFILE, "missing HTTP declaration"));
            }
        }
        for name in HTTP_META {
            if !metadata.contains_key(*name) {
                return Err(source_error(HTTP_PROFILE, "missing HTTP metadata"));
            }
        }
    }
    for entry in specs.values() {
        let allowed: &[&str] = match entry.spec.qname.as_str() {
            "sys::Func" => &["returns"],
            FUNCTION => &["id", "checked", "returns"],
            "sys.api::ApiErr" if http => &["status", "dis", "errTrace"],
            "sys.api::UnknownEntityErr" if http => &["id"],
            "sys.api::UnknownFuncErr" if http => &["funcName"],
            "sys.api::UnsupportedVersionErr" if http => &["allow"],
            _ => &[],
        };
        for slot in &entry.spec.slots {
            if !allowed.contains(&slot.name.as_str()) || !slot.children.is_empty() {
                return Err(resolve_error(
                    &entry.source,
                    format!("{}.{}", entry.spec.qname, slot.name),
                    "nested constraints or additional members are not admitted",
                ));
            }
        }
    }
    let function = specs
        .get(FUNCTION)
        .ok_or_else(|| source_error(PROFILE, "missing readById function"))?;
    if function.spec.base.as_deref() != Some("sys::Func") {
        return Err(resolve_error(
            &function.source,
            FUNCTION,
            "function must derive from sys::Func",
        ));
    }
    let names: BTreeSet<_> = function
        .spec
        .slots
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    if names != BTreeSet::from(["id", "checked", "returns"]) {
        return Err(resolve_error(
            &function.source,
            FUNCTION,
            "only id, checked, and returns members are admitted",
        ));
    }
    Ok(())
}
fn validate_cycles(specs: &BTreeMap<String, AdmittedSpec>) -> Result<(), ProfileError> {
    for entry in specs.values() {
        let mut seen = BTreeSet::new();
        let mut next = Some(entry.spec.qname.as_str());
        while let Some(name) = next {
            if !seen.insert(name) {
                return Err(resolve_error(
                    &entry.source,
                    &entry.spec.qname,
                    "cyclic type inheritance",
                ));
            }
            let spec = specs.get(name).ok_or_else(|| {
                resolve_error(
                    &entry.source,
                    &entry.spec.qname,
                    format!("base '{name}' is not an admitted type"),
                )
            })?;
            next = spec.spec.base.as_deref();
        }
    }
    Ok(())
}
fn validate_meta(
    meta: &HashMap<String, Kind>,
    schema: &BTreeMap<String, AdmittedMetadata>,
    source: &ProfileSource,
    declaration: &str,
) -> Result<(), ProfileError> {
    for (name, value) in meta {
        let field = schema.get(name).ok_or_else(|| {
            resolve_error(
                source,
                declaration,
                format!("metadata '{name}' is not admitted"),
            )
        })?;
        let valid = match slot_type(&field.slot) {
            "sys::Marker" => matches!(value, Kind::Marker),
            "sys::Str" => matches!(value, Kind::Str(_)),
            "sys::Obj" => true,
            "sys::Ref" => matches!(value, Kind::Ref(_)),
            _ => false,
        };
        if !valid {
            return Err(resolve_error(
                source,
                declaration,
                format!("invalid value for metadata '{name}'"),
            ));
        }
        if name == "pattern" {
            let Kind::Str(pattern) = value else {
                unreachable!()
            };
            Regex::new(&format!("\\A(?:{pattern})\\z"))
                .map_err(|_| resolve_error(source, declaration, "unsupported scalar pattern"))?;
        }
    }
    Ok(())
}
fn decode_default(
    raw: &Kind,
    type_name: &str,
    specs: &BTreeMap<String, AdmittedSpec>,
    source: &ProfileSource,
    declaration: &str,
) -> Result<Kind, ProfileError> {
    let Kind::Str(text) = raw else {
        return Err(resolve_error(
            source,
            declaration,
            "default must use the admitted quoted scalar encoding",
        ));
    };
    let mut next = Some(type_name);
    while let Some(name) = next {
        let value = match name {
            "sys::Bool" => match text.as_str() {
                "true" => Some(Kind::Bool(true)),
                "false" => Some(Kind::Bool(false)),
                _ => return Err(resolve_error(source, declaration, "malformed Bool default")),
            },
            "sys::Int" => Some(Kind::Int(text.parse().map_err(|_| {
                resolve_error(source, declaration, "malformed Int default")
            })?)),
            "sys::Number" => Some(Kind::Number(crate::kinds::Number::unitless(
                text.parse()
                    .map_err(|_| resolve_error(source, declaration, "malformed Number default"))?,
            ))),
            "sys::Str" => Some(Kind::Str(text.clone())),
            "sys::Ref" => Some(Kind::Ref(HRef::from_val(text))),
            "sys::Marker" if text == "✓" => Some(Kind::Marker),
            _ => None,
        };
        if let Some(value) = value {
            return Ok(value);
        }
        next = specs.get(name).and_then(|s| s.spec.base.as_deref());
    }
    Err(resolve_error(
        source,
        declaration,
        "default type is not an admitted scalar",
    ))
}
fn decode_slots(
    slots: &mut [Slot],
    types: &BTreeMap<String, AdmittedSpec>,
    schema: &BTreeMap<String, AdmittedMetadata>,
    source: &ProfileSource,
    parent: &str,
) -> Result<(), ProfileError> {
    for slot in slots {
        let qname = format!("{parent}.{}", slot.name);
        validate_meta(&slot.meta, schema, source, &qname)?;
        if slot.default.is_some() && slot.meta.contains_key("val") {
            return Err(resolve_error(
                source,
                &qname,
                "duplicate default declaration",
            ));
        }
        if let Some(default) = slot.default.as_ref().or_else(|| slot.meta.get("val")) {
            let value = decode_default(default, slot_type(slot), types, source, &qname)?;
            slot.default = Some(value.clone());
            slot.meta.insert("val".into(), value);
        }
        decode_slots(&mut slot.children, types, schema, source, &qname)?;
    }
    Ok(())
}

fn validate_slot_syntax(
    slots: &[SlotDef],
    source: &ProfileSource,
    parent: &str,
) -> Result<(), ProfileError> {
    for slot in slots {
        let qname = format!("{parent}.{}", slot.name);
        if slot.is_global || slot.is_query {
            return Err(resolve_error(
                source,
                &qname,
                "global and query slots are not admitted",
            ));
        }
        validate_slot_syntax(&slot.children, source, &qname)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> (ProfileProvenance, Vec<ExtractedSource>) {
        let provenance = serde_json::from_str(MANIFEST).unwrap();
        let sources = extract_sources(&provenance, RAW).unwrap();
        (provenance, sources)
    }

    fn altered(role: &str, before: &str, after: &str) -> ProfileError {
        let (provenance, mut sources) = inputs();
        let source = sources
            .iter_mut()
            .find(|s| s.identity.role == role)
            .unwrap();
        assert!(
            source.text.contains(before),
            "test mutation target must exist"
        );
        source.text = source.text.replacen(before, after, 1);
        ReadByIdProfile::admit(provenance, sources).unwrap_err()
    }

    #[test]
    fn malformed_syntax_keeps_original_upstream_source_line() {
        let error = altered("functions", "checked: Bool", "checked: !Bool");
        assert!(matches!(error, ProfileError::Parse { path, line: 24, .. }
            if path == "src/xeto/sys.api/funcs.xeto"));
    }

    #[test]
    fn source_integrity_precedes_parsing() {
        let provenance = serde_json::from_str(MANIFEST).unwrap();
        let mut raw = RAW.to_vec();
        let index = raw
            .iter()
            .position(|(path, _)| path.ends_with("funcs.xeto"))
            .unwrap();
        raw[index].1 = "not the retained upstream source";
        assert!(
            matches!(extract_sources(&provenance, &raw), Err(ProfileError::Source { path, message })
            if path.ends_with("funcs.xeto") && message == "SHA-256 mismatch")
        );
    }

    #[test]
    fn extraction_rejects_unknown_build_variables() {
        let variables = BTreeMap::from([("ph.version", "5.0.0")]);
        assert_eq!(
            expand_build_vars("version: BuildVar \"ph.version\"", &variables, "lib.xeto").unwrap(),
            "version: \"5.0.0\""
        );
        assert!(
            matches!(expand_build_vars("version: BuildVar \"missing\"", &variables, "lib.xeto"),
            Err(ProfileError::Source { path, message }) if path == "lib.xeto" && message.contains("unresolved build variable"))
        );
    }

    #[test]
    fn malformed_bool_default_is_a_resolution_error() {
        assert!(
            matches!(altered("functions", "Bool \"true\"", "Bool \"truthy\""),
            ProfileError::Resolve { path, declaration, message }
            if path.ends_with("funcs.xeto") && declaration.ends_with(".checked") && message == "malformed Bool default")
        );
    }

    #[test]
    fn malformed_ref_construction_default_does_not_enter_catalog() {
        assert!(matches!(altered("types", "\"x\"", "\"bad ref\""),
            ProfileError::Resolve { declaration, message, .. }
            if declaration == "sys::Ref" && message.contains("default does not fit")));
    }

    #[test]
    fn qualified_references_are_resolved_against_admitted_targets() {
        let (provenance, mut sources) = inputs();
        let functions = sources
            .iter_mut()
            .find(|s| s.identity.role == "functions")
            .unwrap();
        functions.text = functions
            .text
            .replace("id: Ref?", "id: sys::Ref?")
            .replace("checked: Bool", "checked: sys::Bool");
        let profile = ReadByIdProfile::admit(provenance, sources).unwrap();
        assert_eq!(
            profile
                .fit_arguments(FUNCTION, &HDict::new())
                .unwrap()
                .values()
                .get("checked"),
            Some(&Kind::Bool(true))
        );
        assert!(
            matches!(altered("functions", "id: Ref?", "id: sys::Missing?"),
            ProfileError::Resolve { declaration, message, .. }
            if declaration.ends_with(".id") && message.contains("sys::Missing"))
        );
        assert!(
            matches!(altered("functions", "id: Ref?", "id: other::Ref?"),
            ProfileError::Resolve { message, .. } if message.contains("undeclared dependency"))
        );
    }

    #[test]
    fn unresolved_nested_member_reports_full_path() {
        assert!(
            matches!(altered("functions", "id: Ref?", "id: Dict { child: sys::Missing }"),
            ProfileError::Resolve { declaration, message, .. }
            if declaration == "sys.api::readById.id.child" && message.contains("sys::Missing"))
        );
        // Even fully resolvable nested constraints are rejected: the selected
        // fitter does not claim recursive structural constraints beyond Dict.
        assert!(
            matches!(altered("functions", "id: Ref?", "id: Dict { child: sys::Ref }"),
            ProfileError::Resolve { message, .. } if message.contains("nested constraints"))
        );
    }

    #[test]
    fn member_target_existence_and_dependency_visibility_are_checked() {
        let (_, sources) = inputs();
        let mut pragmas = Pragmas::new();
        for source in sources.iter().filter(|s| s.identity.role == "library") {
            pragmas.insert(
                source.identity.library.clone(),
                (
                    source.parse().unwrap().pragma.unwrap(),
                    source.identity.clone(),
                ),
            );
        }
        let source = &sources
            .iter()
            .find(|s| s.identity.role == "functions")
            .unwrap()
            .identity;
        let names = BTreeSet::from([
            "sys::Func.returns".into(),
            "sys.api::readById.checked".into(),
        ]);
        assert_eq!(
            resolve_name(
                "sys::Func.returns",
                "sys.api",
                &pragmas,
                &names,
                source,
                "test"
            )
            .unwrap(),
            "sys::Func.returns"
        );
        assert!(
            resolve_name(
                "sys::Func.absent",
                "sys.api",
                &pragmas,
                &names,
                source,
                "test"
            )
            .is_err()
        );
        // sys cannot search sys.api merely because it is loaded.
        assert!(
            resolve_name(
                "sys.api::readById.checked",
                "sys",
                &pragmas,
                &names,
                source,
                "test"
            )
            .is_err()
        );
        assert!(resolve_name("readById.checked", "sys", &pragmas, &names, source, "test").is_err());
    }

    #[test]
    fn missing_and_wrong_version_dependencies_fail_resolution() {
        let (provenance, mut sources) = inputs();
        sources.retain(|s| !(s.identity.role == "library" && s.identity.library == "sys"));
        assert!(
            matches!(ReadByIdProfile::admit(provenance, sources), Err(ProfileError::Resolve { message, .. })
            if message.contains("missing required library dependency"))
        );
        assert!(
            matches!(altered("library", "version: \"5.0.0\"", "version: \"6.0.0\""),
            ProfileError::Resolve { message, .. } if message.contains("pinned 5.0.0"))
        );
        let (provenance, mut sources) = inputs();
        let api = sources
            .iter_mut()
            .find(|s| s.identity.role == "library" && s.identity.library == "sys.api")
            .unwrap();
        api.text = api
            .text
            .replace("versions: \"5.0.0\"", "versions: \"6.0.0\"");
        assert!(
            matches!(ReadByIdProfile::admit(provenance, sources), Err(ProfileError::Resolve { message, .. })
            if message.contains("dependency version"))
        );
    }

    #[test]
    fn unsupported_declarations_and_augmentations_are_not_filtered() {
        for error in [
            altered("types", "Obj: <", "Outside: <"),
            altered("functions", "+Funcs", "+Unknown"),
            altered("functions", "readById:", "readByIds:"),
            altered("functions", "noSideEffects>", "noSideEffects, async>"),
            altered("functions", "id: Ref?", "*id: Ref?"),
            altered("functions", "id: Ref?", "id: Query<of:Ref>"),
        ] {
            assert!(matches!(error, ProfileError::Resolve { .. }), "{error}");
        }
    }

    #[test]
    fn duplicate_members_and_inheritance_cycles_are_rejected() {
        assert!(
            matches!(altered("functions", "id: Ref?", "id: Ref?, id: Ref?"),
            ProfileError::Resolve { message, .. } if message == "duplicate member")
        );
        assert!(matches!(altered("types", "Scalar: Obj", "Scalar: Ref"),
            ProfileError::Resolve { message, .. } if message == "cyclic type inheritance"));
    }

    #[test]
    fn required_missing_parameter_does_not_borrow_type_default() {
        let (provenance, mut sources) = inputs();
        let source = sources
            .iter_mut()
            .find(|s| s.identity.role == "functions")
            .unwrap();
        source.text = source
            .text
            .replace("checked: Bool \"true\"", "checked: Bool");
        let profile = ReadByIdProfile::admit(provenance, sources).unwrap();
        assert!(
            matches!(profile.fit_arguments(FUNCTION, &HDict::new()), Err(ProfileError::Fit { slot, message, .. })
            if slot.ends_with(".checked") && message == "missing required argument")
        );
    }

    #[test]
    fn explicit_val_parameter_default_is_typed_and_not_an_invariant() {
        let (provenance, mut sources) = inputs();
        let source = sources
            .iter_mut()
            .find(|s| s.identity.role == "functions")
            .unwrap();
        source.text = source
            .text
            .replace("checked: Bool \"true\"", "checked: Bool <val: \"true\">");
        let profile = ReadByIdProfile::admit(provenance, sources).unwrap();
        assert_eq!(
            profile
                .fit_arguments(FUNCTION, &HDict::new())
                .unwrap()
                .values()
                .get("checked"),
            Some(&Kind::Bool(true))
        );
        let mut args = HDict::new();
        args.set("checked", Kind::Bool(false));
        assert_eq!(
            profile
                .fit_arguments(FUNCTION, &args)
                .unwrap()
                .values()
                .get("checked"),
            Some(&Kind::Bool(false))
        );
    }
}

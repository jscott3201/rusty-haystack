use super::*;
use crate::codecs::jeto;
/// Codec-context failures name the declaration (and slot, when known) being
/// compiled and its retained source, never a generic catalog error.
fn unsupported(profile: &Catalog, owner: &str, slot: Option<&str>, message: &str) -> ProfileError {
    ProfileError::Resolve {
        path: profile
            .declaration(owner)
            .map_or_else(|| "catalog".into(), |entry| entry.source.path.clone()),
        declaration: match slot {
            Some(slot) => format!("{owner}.{slot}"),
            None => owner.into(),
        },
        message: format!("callable codec context: {message}"),
    }
}
/// Jeto context bounds mirrored by selection admission (`with_project`).
pub(super) const MAX_CODEC_DEFINITIONS: usize = 256;
pub(super) const MAX_PATTERN_BYTES: usize = 1024;
pub const ARGUMENTS: &str = "rusty.call::Arguments";
/// A codec-only argument container derived from the actual admitted function
/// slots. It does not register a function or admit a broader Xeto catalog.
#[derive(Debug, Clone)]
pub struct CallableContext {
    pub context: jeto::Context,
    pub parameters: std::collections::BTreeMap<String, String>,
    pub result: String,
    pub strict_arguments: bool,
    pub function: String,
}
impl CallableContext {
    pub fn new(profile: &Catalog, declaration: &AdmittedSpec) -> Result<Self, ProfileError> {
        use std::collections::{BTreeMap, BTreeSet};
        fn ty(
            profile: &Catalog,
            name: &str,
            definitions: &mut Vec<jeto::Definition>,
            seen: &mut BTreeSet<String>,
        ) -> Result<String, ProfileError> {
            if name == "sys.api::ApiVersion" {
                return Ok("sys::Str".into());
            }
            if jeto::Context::standard().contains(name) {
                return Ok(name.into());
            }
            if !seen.insert(name.into()) {
                return Ok(name.into());
            }
            let definition = if let Some(keys) = profile.enum_keys(name) {
                jeto::Definition::Enum {
                    name: name.into(),
                    keys: keys.to_vec(),
                }
            } else if matches!(name, "sys::Filter" | "sys::Version") {
                jeto::Definition::Nominal {
                    name: name.into(),
                    pattern: if name == "sys::Filter" {
                        "(?s:.*)"
                    } else {
                        "[0-9]+(?:\\.[0-9]+)*"
                    }
                    .into(),
                }
            } else {
                profile
                    .declaration(name)
                    .ok_or_else(|| unsupported(profile, name, None, "type is not admitted"))?;
                if profile.derives_from(name, "sys::Scalar") {
                    if profile.derives_from(name, "sys::Ref") {
                        jeto::Definition::Ref { name: name.into() }
                    } else {
                        let metadata = profile.effective_metadata(name).ok_or_else(|| {
                            unsupported(profile, name, None, "metadata is not resolvable")
                        })?;
                        let pattern = match metadata.get("pattern") {
                            Some(Kind::Str(value)) => value.clone(),
                            _ => "(?s:.*)".into(),
                        };
                        jeto::Definition::Nominal {
                            name: name.into(),
                            pattern,
                        }
                    }
                } else if profile.derives_from(name, "sys::Dict") {
                    let mut members = BTreeMap::new();
                    for field in profile.compiled[name].slots.values() {
                        let member = &field.slot;
                        if member.name == "spec" {
                            continue;
                        }
                        members.insert(
                            member.name.clone(),
                            slot(profile, name, member, definitions, seen)?,
                        );
                    }
                    jeto::Definition::Dict {
                        name: name.into(),
                        members,
                    }
                } else {
                    return Err(unsupported(
                        profile,
                        name,
                        None,
                        "type is neither a Scalar nor a Dict",
                    ));
                }
            };
            definitions.push(definition);
            Ok(name.into())
        }
        fn slot(
            profile: &Catalog,
            owner: &str,
            slot: &super::super::spec::Slot,
            definitions: &mut Vec<jeto::Definition>,
            seen: &mut BTreeSet<String>,
        ) -> Result<String, ProfileError> {
            let name = super::slot_type(slot);
            if name == "sys::Ref" || profile.derives_from(name, "sys::Ref") {
                return ty(profile, name, definitions, seen);
            }
            if let Some(Kind::Ref(of)) = slot.meta.get("of") {
                let of = ty(profile, &of.val, definitions, seen)?;
                if name == "sys::List" {
                    let name = format!(
                        "rusty.call::{}_{}",
                        owner.rsplit("::").next().unwrap_or(""),
                        slot.name
                    );
                    definitions.push(jeto::Definition::List {
                        name: name.clone(),
                        of,
                    });
                    return Ok(name);
                }
                if name != "sys::Grid" {
                    return Err(unsupported(
                        profile,
                        owner,
                        Some(&slot.name),
                        "of constraint is supported only on List and Grid",
                    ));
                }
            }
            ty(profile, name, definitions, seen)
        }
        let mut parameters = BTreeMap::new();
        let mut definitions = Vec::new();
        let mut seen = BTreeSet::new();
        let mut result = None;
        for member in &declaration.spec.slots {
            let name = slot(
                profile,
                &declaration.spec.qname,
                member,
                &mut definitions,
                &mut seen,
            )?;
            if member.name == "returns" {
                result = Some(name);
            } else {
                parameters.insert(member.name.clone(), name);
            }
        }
        // Generic Dict results may contain any selected data declaration. The
        // codec context uses the same source provenance as native fitting.
        for admitted in profile.declarations() {
            let name = &admitted.spec.qname;
            if admitted.member_of.is_none()
                && !admitted.spec.is_abstract
                && name != "sys::This"
                && !profile.derives_from(name, "sys::Func")
                && (profile.derives_from(name, "sys::Dict")
                    || profile.derives_from(name, "sys::Scalar"))
            {
                ty(profile, name, &mut definitions, &mut seen)?;
            }
        }
        definitions.push(jeto::Definition::Dict {
            name: ARGUMENTS.into(),
            members: parameters.clone(),
        });
        let function = declaration.spec.qname.as_str();
        if definitions.len() > MAX_CODEC_DEFINITIONS {
            return Err(unsupported(
                profile,
                function,
                None,
                &format!(
                    "selection needs {} codec definitions; the bound is {MAX_CODEC_DEFINITIONS}",
                    definitions.len()
                ),
            ));
        }
        let context = jeto::Context::new(
            &profile.provenance().repository,
            &profile.provenance().commit,
            definitions,
        )
        .map_err(|error| unsupported(profile, function, None, error.0))?;
        let result =
            result.ok_or_else(|| unsupported(profile, function, None, "missing returns slot"))?;
        if !context.contains(&result) {
            return Err(unsupported(
                profile,
                function,
                Some("returns"),
                "result type is not representable",
            ));
        }
        Ok(Self {
            context,
            function: declaration.spec.qname.clone(),
            parameters,
            result,
            strict_arguments: declaration.spec.qname != "sys.api::readById",
        })
    }
}

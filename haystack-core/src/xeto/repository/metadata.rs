//! Selected source/props agreement; convenience AST dependency lists are not proof.
use super::*;
use crate::kinds::Kind;
use std::collections::{BTreeMap, BTreeSet};

fn decode(text: &str) -> Result<String, PackageError> {
    let mut chars = text.chars().peekable();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let escape = chars
            .next()
            .ok_or(PackageError::Unsupported("props continuation"))?;
        out.push(match escape {
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            '\\' => '\\',
            'u' => {
                fn unit(chars: &mut impl Iterator<Item = char>) -> Result<u32, PackageError> {
                    let mut value = 0;
                    for _ in 0..4 {
                        value = value * 16
                            + chars
                                .next()
                                .and_then(|c| c.to_digit(16))
                                .ok_or(PackageError::Invalid("props Unicode escape"))?;
                    }
                    Ok(value)
                }
                let first = unit(&mut chars)?;
                let code = if (0xd800..=0xdbff).contains(&first) {
                    if chars.next() != Some('\\') || chars.next() != Some('u') {
                        return Err(PackageError::Invalid("props surrogate"));
                    }
                    let second = unit(&mut chars)?;
                    if !(0xdc00..=0xdfff).contains(&second) {
                        return Err(PackageError::Invalid("props surrogate"));
                    }
                    0x10000 + ((first - 0xd800) << 10) + second - 0xdc00
                } else {
                    first
                };
                char::from_u32(code).ok_or(PackageError::Invalid("props Unicode scalar"))?
            }
            _ => return Err(PackageError::Unsupported("props escape")),
        });
    }
    Ok(out)
}
fn props(text: &str) -> Result<BTreeMap<String, String>, PackageError> {
    let mut values = BTreeMap::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('#')
            || line.contains("/*")
            || line.starts_with("//")
            || line
                .as_bytes()
                .windows(3)
                .any(|w| w[0].is_ascii_whitespace() && &w[1..] == b"//")
        {
            return Err(PackageError::Unsupported("props comments"));
        }
        let (key, value) = line
            .split_once('=')
            .ok_or(PackageError::Invalid("props assignment"))?;
        let key = decode(key.trim())?;
        let value = decode(value.trim())?;
        if !["name", "version", "maturity", "depends", "doc"].contains(&key.as_str()) {
            return Err(PackageError::Unsupported("props field"));
        }
        if values.insert(key, value).is_some() {
            return Err(PackageError::Invalid("duplicate props field"));
        }
    }
    Ok(values)
}
pub(super) fn canonical(text: &str) -> Result<LibraryVersion, PackageError> {
    let version: LibraryVersion = text.parse()?;
    if version.to_string() != text {
        return Err(PackageError::Unsupported("noncanonical version"));
    }
    Ok(version)
}
fn maturity(text: &str) -> Result<Maturity, PackageError> {
    match text {
        "alpha" => Ok(Maturity::Alpha),
        "beta" => Ok(Maturity::Beta),
        "stable" => Ok(Maturity::Stable),
        _ => Err(PackageError::Invalid("maturity")),
    }
}
pub(super) fn validate(
    pin: &LibraryPin,
    properties: &str,
    source: &str,
    digest: Sha256Digest,
) -> Result<(RepoLibVersion, Vec<LibraryPin>, usize, usize), PackageError> {
    let values = props(properties)?;
    let required = |key| {
        values
            .get(key)
            .map(String::as_str)
            .ok_or(PackageError::Invalid("required props field"))
    };
    if required("name")? != pin.lib() || canonical(required("version")?)? != pin.version() {
        return Err(PackageError::Invalid("package identity"));
    }
    let ast =
        crate::xeto::parse_xeto(source).map_err(|_| PackageError::Invalid("source syntax"))?;
    fn count_slots(slots: &[crate::xeto::SlotDef]) -> usize {
        slots
            .iter()
            .map(|slot| 1 + count_slots(&slot.children))
            .sum()
    }
    let declaration_count = ast.specs.len();
    let slot_count = ast.specs.iter().map(|spec| count_slots(&spec.slots)).sum();
    let pragma = ast.pragma.ok_or(PackageError::Invalid("source pragma"))?;
    if pragma
        .meta
        .keys()
        .any(|key| !["version", "depends", "maturity", "doc"].contains(&key.as_str()))
    {
        return Err(PackageError::Unsupported("source library metadata"));
    }
    if canonical(&pragma.version)? != pin.version() {
        return Err(PackageError::Invalid("source version"));
    }
    let source_maturity = match pragma.meta.get("maturity") {
        Some(Kind::Str(text)) => maturity(text)?,
        None => Maturity::Stable,
        _ => return Err(PackageError::Invalid("source maturity")),
    };
    if maturity(required("maturity")?)? != source_maturity {
        return Err(PackageError::Invalid("maturity agreement"));
    }
    let doc = match pragma.meta.get("doc") {
        Some(Kind::Str(text)) => text.as_str(),
        None => "",
        _ => return Err(PackageError::Invalid("source doc")),
    };
    if values.get("doc").map_or("", String::as_str) != doc {
        return Err(PackageError::Invalid("doc agreement"));
    }
    let dependencies = dependencies(&pragma)?;
    let expected = dependencies
        .iter()
        .map(|dep| format!("{} {}", dep.lib(), dep.version()))
        .collect::<Vec<_>>()
        .join(";");
    if required("depends")? != expected {
        return Err(PackageError::Invalid("dependency agreement"));
    }
    let depends = dependencies
        .iter()
        .map(|dep| {
            Ok(RepoDependency {
                lib: dep.lib().into(),
                versions: dep.version().to_string().parse()?,
            })
        })
        .collect::<Result<_, PackageError>>()?;
    Ok((
        RepoLibVersion {
            pin: pin.clone(),
            maturity: source_maturity,
            availability: Availability::Available,
            availability_msg: None,
            doc: Some(doc.into()),
            depends: Some(depends),
            digest: Some(digest),
        },
        dependencies,
        declaration_count,
        slot_count,
    ))
}

/// Read the raw declaration so convenience parsing cannot discard an edge.
pub(super) fn dependencies(
    pragma: &crate::xeto::LibPragma,
) -> Result<Vec<LibraryPin>, PackageError> {
    let mut dependencies = Vec::new();
    let mut seen = BTreeSet::new();
    if let Some(value) = pragma.meta.get("depends") {
        let Kind::List(entries) = value else {
            return Err(PackageError::Invalid("dependency list"));
        };
        if entries.len() > 32 {
            return Err(PackageError::Limit("direct dependencies"));
        }
        for entry in entries {
            let Kind::Dict(dict) = entry else {
                return Err(PackageError::Invalid("dependency dictionary"));
            };
            let (Some(Kind::Str(lib)), Some(Kind::Str(version))) =
                (dict.get("lib"), dict.get("versions"))
            else {
                return Err(PackageError::Invalid("dependency fields"));
            };
            if dict.len() != 2 || !seen.insert(lib.clone()) {
                return Err(PackageError::Invalid("duplicate or unsupported dependency"));
            }
            dependencies.push(LibraryPin::new(lib, canonical(version)?)?);
        }
    }
    Ok(dependencies)
}

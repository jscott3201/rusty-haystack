//! Fallible query evaluation over an application-supplied, already masked view.
//! No transport, identity, policy implementation, lock, or runtime is owned here.
use super::{FilterNode, Path};
use crate::{
    data::HDict,
    kinds::{HRef, Kind},
    ontology::{DefNamespace, SpecTerm},
    xeto::{Slot, Spec},
};
use std::{collections::HashSet, sync::Arc};

/// Catalog namespaces have different identities even when their names coincide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CatalogKind {
    Definition,
    Library,
    Spec,
}

/// The application supplies authorized records and charges work before producing
/// them. An inverse implementation must charge every raw edge, including denied
/// edges, and bound its output vector before appending. A stop is always `Err`.
pub trait QueryEnvironment {
    type Error;
    fn work(&mut self, amount: usize) -> Result<(), Self::Error>;
    fn retain(&mut self, bytes: usize) -> Result<(), Self::Error>;
    fn depth(&mut self, depth: usize) -> Result<(), Self::Error>;
    fn comparison(&mut self, left: &Kind, right: &Kind) -> Result<(), Self::Error>;
    fn forward(&mut self, id: &HRef) -> Result<Option<Arc<HDict>>, Self::Error>;
    fn inverse(&mut self, target: &HRef, tag: &str) -> Result<Vec<Arc<HDict>>, Self::Error>;
    fn catalog_visible(&mut self, kind: CatalogKind, name: &str) -> Result<bool, Self::Error>;
    fn regex_size_limit(&self) -> usize;
    fn regex_limit_error(&self) -> Self::Error;
}

fn visible_def<E: QueryEnvironment>(
    ns: &DefNamespace,
    name: &str,
    env: &mut E,
) -> Result<bool, E::Error> {
    env.work(1)?;
    let Some(def) = ns.get_def(name) else {
        return Ok(false);
    };
    Ok(env.catalog_visible(CatalogKind::Definition, name)?
        && env.catalog_visible(CatalogKind::Library, &def.lib)?)
}
fn visible_spec<E: QueryEnvironment>(spec: &Spec, env: &mut E) -> Result<bool, E::Error> {
    Ok(env.catalog_visible(CatalogKind::Spec, &spec.qname)?
        && env.catalog_visible(CatalogKind::Library, &spec.lib)?)
}

/// Hidden and absent catalog terms have exactly the same result. Use this for
/// validation before matching so an unavailable term cannot become a typo match.
pub fn catalog_term_available<E: QueryEnvironment>(
    ns: Option<&DefNamespace>,
    term: &str,
    env: &mut E,
) -> Result<bool, E::Error> {
    env.work(term.len().saturating_add(1))?;
    env.retain(term.len().saturating_mul(4))?;
    let Some(ns) = ns else {
        return Ok(false);
    };
    match ns.resolve_spec_term(term) {
        Some(SpecTerm::Def(name)) => visible_def(ns, &name, env),
        Some(SpecTerm::Spec(spec)) => visible_spec(spec, env),
        None => Ok(false),
    }
}

/// Match against records already authorized/masked by the environment.
pub fn matches_controlled<E: QueryEnvironment>(
    node: &FilterNode,
    entity: Arc<HDict>,
    ns: Option<&DefNamespace>,
    env: &mut E,
) -> Result<bool, E::Error> {
    matches_at(node, entity, ns, env, 1)
}
fn matches_at<E: QueryEnvironment>(
    node: &FilterNode,
    entity: Arc<HDict>,
    ns: Option<&DefNamespace>,
    env: &mut E,
    depth: usize,
) -> Result<bool, E::Error> {
    env.depth(depth)?;
    env.work(1)?;
    Ok(match node {
        FilterNode::Has(path) => resolve_path(entity, path, env)?
            .is_some_and(|e| e.has(path.0.last().expect("nonempty path"))),
        FilterNode::Missing(path) => resolve_path(entity, path, env)?
            .is_none_or(|e| e.missing(path.0.last().expect("nonempty path"))),
        FilterNode::Cmp { path, op, val } => {
            let owner = resolve_path(entity, path, env)?;
            match owner
                .as_ref()
                .and_then(|e| path.0.last().and_then(|tag| e.get(tag)))
            {
                Some(actual) => {
                    env.comparison(actual, val)?;
                    super::eval::compare(actual, op, val)
                }
                None => false,
            }
        }
        FilterNode::And(l, r) => {
            matches_at(l, entity.clone(), ns, env, depth + 1)?
                && matches_at(r, entity, ns, env, depth + 1)?
        }
        FilterNode::Or(l, r) => {
            matches_at(l, entity.clone(), ns, env, depth + 1)?
                || matches_at(r, entity, ns, env, depth + 1)?
        }
        FilterNode::SpecMatch(term) => match ns {
            Some(ns) => fits_term(&entity, term, ns, env, true, depth + 1)?,
            None => false,
        },
    })
}
fn resolve_path<E: QueryEnvironment>(
    mut entity: Arc<HDict>,
    path: &Path,
    env: &mut E,
) -> Result<Option<Arc<HDict>>, E::Error> {
    if path.0.is_empty() {
        return Ok(None);
    }
    for tag in &path.0[..path.0.len() - 1] {
        env.work(1)?;
        let Some(Kind::Ref(id)) = entity.get(tag) else {
            return Ok(None);
        };
        let Some(next) = env.forward(id)? else {
            return Ok(None);
        };
        entity = next;
    }
    Ok(Some(entity))
}

fn fits_term<E: QueryEnvironment>(
    entity: &HDict,
    term: &str,
    ns: &DefNamespace,
    env: &mut E,
    queries: bool,
    depth: usize,
) -> Result<bool, E::Error> {
    env.depth(depth)?;
    if !catalog_term_available(Some(ns), term, env)? {
        return Ok(false);
    }
    match ns.resolve_spec_term(term) {
        Some(SpecTerm::Def(name)) => member_of(entity, &name, ns, env, depth + 1),
        Some(SpecTerm::Spec(spec)) => fits_spec(entity, spec, ns, env, queries, depth + 1),
        None => Ok(false),
    }
}
fn member_of<E: QueryEnvironment>(
    entity: &HDict,
    wanted: &str,
    ns: &DefNamespace,
    env: &mut E,
    depth: usize,
) -> Result<bool, E::Error> {
    env.depth(depth)?;
    if !visible_def(ns, wanted, env)? {
        return Ok(false);
    }
    if let Some(parts) = ns.conjunct_parts(wanted) {
        if parts.is_empty() {
            return Ok(false);
        }
        for part in parts {
            env.work(1)?;
            if !member_of(entity, part, ns, env, depth + 1)? {
                return Ok(false);
            }
        }
        return Ok(true);
    }
    for (tag, value) in entity.iter() {
        env.work(1)?;
        if !matches!(value, Kind::Marker) {
            continue;
        }
        let mut todo = Vec::new();
        let mut seen = HashSet::new();
        env.retain(64)?;
        todo.push(tag);
        while let Some(name) = todo.pop() {
            env.work(1)?;
            if seen.contains(name) {
                continue;
            }
            env.retain(64)?;
            seen.insert(name);
            if !visible_def(ns, name, env)? {
                continue;
            }
            if name == wanted {
                return Ok(true);
            }
            if let Some(def) = ns.get_def(name) {
                for parent in &def.is_ {
                    env.work(1)?;
                    env.retain(64)?;
                    todo.push(parent.as_str());
                }
            }
        }
    }
    Ok(false)
}
fn fits_spec<E: QueryEnvironment>(
    entity: &HDict,
    spec: &Spec,
    ns: &DefNamespace,
    env: &mut E,
    queries: bool,
    depth: usize,
) -> Result<bool, E::Error> {
    env.depth(depth)?;
    let mut current = Some(spec);
    let mut seen = HashSet::new();
    while let Some(base) = current {
        env.work(1)?;
        if seen.contains(base.qname.as_str()) {
            break;
        }
        env.retain(64)?;
        seen.insert(base.qname.as_str());
        if !visible_spec(base, env)? {
            return Ok(false);
        }
        for slot in &base.slots {
            env.work(1)?;
            if slot.is_marker && !slot.is_maybe() && entity.missing(&slot.name) {
                return Ok(false);
            }
        }
        current = match &base.base {
            Some(name) => match ns.get_spec(name) {
                Some(next) => Some(next),
                None => return Ok(false),
            },
            None => None,
        };
    }
    for slot in &spec.slots {
        env.work(1)?;
        if slot.is_query {
            if queries && !query_slot(entity, spec, slot, ns, env, depth + 1)? {
                return Ok(false);
            }
        } else if !slot.is_marker
            && let Some(value) = entity.get(&slot.name)
        {
            if !slot.is_maybe() && !slot_type_matches(value, slot.type_ref.as_deref()) {
                return Ok(false);
            }
            if !constraints(value, slot, env)? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}
fn slot_type_matches(v: &Kind, name: Option<&str>) -> bool {
    match name {
        Some("Str") => matches!(v, Kind::Str(_)),
        Some("Number") => matches!(v, Kind::Number(_)),
        Some("Ref") => matches!(v, Kind::Ref(_)),
        Some("Bool") => matches!(v, Kind::Bool(_)),
        Some("Date") => matches!(v, Kind::Date(_)),
        Some("Time") => matches!(v, Kind::Time(_)),
        Some("DateTime") => matches!(v, Kind::DateTime(_)),
        Some("Uri") => matches!(v, Kind::Uri(_)),
        Some("Coord") => matches!(v, Kind::Coord(_)),
        Some("List") => matches!(v, Kind::List(_)),
        Some("Dict") => matches!(v, Kind::Dict(_)),
        Some("Grid") => matches!(v, Kind::Grid(_)),
        Some("Marker") => matches!(v, Kind::Marker),
        _ => true,
    }
}
fn constraints<E: QueryEnvironment>(
    value: &Kind,
    slot: &Slot,
    env: &mut E,
) -> Result<bool, E::Error> {
    if let Kind::Number(number) = value {
        for (key, lower) in [("minVal", true), ("maxVal", false)] {
            env.work(1)?;
            if let Some(Kind::Number(bound)) = slot.meta.get(key)
                && ((lower && number.val < bound.val) || (!lower && number.val > bound.val))
            {
                return Ok(false);
            }
        }
        if slot.meta.contains_key("unitless") && number.unit.is_some() {
            return Ok(false);
        }
        if let Some(Kind::Str(unit)) = slot.meta.get("unit") {
            env.work(unit.len().saturating_add(1))?;
            if number.unit.as_deref() != Some(unit) {
                return Ok(false);
            }
        }
    }
    let size = match value {
        Kind::Str(s) => Some(s.len()),
        Kind::List(v) => Some(v.len()),
        _ => None,
    };
    if let Some(size) = size {
        for (key, lower) in [("minSize", true), ("maxSize", false)] {
            env.work(1)?;
            if let Some(Kind::Number(bound)) = slot.meta.get(key)
                && ((lower && (size as f64) < bound.val) || (!lower && (size as f64) > bound.val))
            {
                return Ok(false);
            }
        }
    }
    if let Kind::Str(text) = value {
        env.work(text.len().saturating_add(1))?;
        if slot.meta.contains_key("nonEmpty") && text.trim().is_empty() {
            return Ok(false);
        }
        if let Some(Kind::Str(pattern)) = slot.meta.get("pattern") {
            env.work(
                pattern
                    .len()
                    .saturating_add(1)
                    .saturating_mul(text.len().saturating_add(1)),
            )?;
            env.retain(env.regex_size_limit())?;
            let regex = regex::RegexBuilder::new(pattern)
                .size_limit(env.regex_size_limit())
                .dfa_size_limit(env.regex_size_limit())
                .build();
            match regex {
                Ok(regex) if regex.is_match(text) => {}
                Ok(_) | Err(regex::Error::Syntax(_)) => return Ok(false),
                Err(_) => return Err(env.regex_limit_error()),
            }
        }
    }
    Ok(true)
}
fn text_meta<'a>(slot: &'a Slot, key: &str) -> Option<&'a str> {
    match slot.meta.get(key) {
        Some(Kind::Str(s)) => Some(s),
        _ => None,
    }
}
fn query_slot<E: QueryEnvironment>(
    entity: &HDict,
    enclosing: &Spec,
    slot: &Slot,
    ns: &DefNamespace,
    env: &mut E,
    depth: usize,
) -> Result<bool, E::Error> {
    env.depth(depth)?;
    let (tag, transitive, inverse) = if let Some(via) = text_meta(slot, "via") {
        (
            via.strip_suffix('+').unwrap_or(via),
            via.ends_with('+'),
            false,
        )
    } else if let Some(reference) = text_meta(slot, "inverse") {
        let Some((name, slot_name)) = reference.rsplit_once('.') else {
            return Ok(false);
        };
        let Some(spec) = ns.get_spec(name) else {
            return Ok(false);
        };
        if !visible_spec(spec, env)? {
            return Ok(false);
        }
        let mut via = None;
        for candidate in &spec.slots {
            env.work(1)?;
            if candidate.name == slot_name && candidate.is_query {
                via = text_meta(candidate, "via");
                break;
            }
        }
        let Some(via) = via else {
            return Ok(false);
        };
        (
            via.strip_suffix('+').unwrap_or(via),
            via.ends_with('+'),
            true,
        )
    } else {
        return Ok(true);
    };
    env.work(tag.len().saturating_add(1))?;
    let mut todo = Vec::new();
    let mut seen = HashSet::new();
    let first = if inverse {
        entity.id()
    } else {
        match entity.get(tag) {
            Some(Kind::Ref(r)) => Some(r),
            _ => None,
        }
    };
    if let Some(id) = first {
        env.retain(id.val.len().saturating_add(96))?;
        todo.push(HRef::from_val(&id.val));
    }
    let mut matching = false;
    while let Some(id) = todo.pop() {
        env.work(1)?;
        if seen.contains(&id.val) {
            continue;
        }
        env.retain(id.val.len().saturating_add(64))?;
        seen.insert(id.val.clone());
        let reached = if inverse {
            env.inverse(&id, tag)?
        } else {
            match env.forward(&id)? {
                Some(entity) => {
                    env.retain(32)?;
                    vec![entity]
                }
                None => Vec::new(),
            }
        };
        for reached in reached {
            env.work(1)?;
            let is_match = match text_meta(slot, "of") {
                Some(of) => {
                    env.retain(
                        enclosing
                            .lib
                            .len()
                            .saturating_add(of.len())
                            .saturating_add(2),
                    )?;
                    let local = format!("{}::{of}", enclosing.lib);
                    let resolved = if !of.contains("::") && ns.get_spec(&local).is_some() {
                        local.as_str()
                    } else {
                        of
                    };
                    fits_term(&reached, resolved, ns, env, false, depth + 1)?
                }
                None => true,
            };
            matching |= is_match;
            // Complete the traversal even after a match: explicit work ceilings
            // cover all visited raw edges and cannot be hidden by optional slots.
            if transitive {
                let next = if inverse {
                    reached.id()
                } else {
                    match reached.get(tag) {
                        Some(Kind::Ref(r)) => Some(r),
                        _ => None,
                    }
                };
                if let Some(next) = next
                    && !seen.contains(&next.val)
                {
                    env.retain(next.val.len().saturating_add(96))?;
                    todo.push(HRef::from_val(&next.val));
                }
            }
        }
        if !transitive {
            break;
        }
    }
    Ok(matching || slot.is_maybe())
}

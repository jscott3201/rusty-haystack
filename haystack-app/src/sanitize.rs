use crate::{BudgetKind, PolicySnapshot, ReadError, budget::Budget};
use haystack_core::{
    data::{HCol, HDict, HGrid},
    filter::{CatalogKind, QueryEnvironment},
    graph::EntityGraph,
    kinds::{HRef, Kind},
};
use std::sync::Arc;

/// Work over one coherent borrowed graph/catalog snapshot. No raw entity is
/// passed into a predicate, projection, or response-building helper.
pub(crate) struct View<'a> {
    pub graph: &'a EntityGraph,
    pub policy: &'a dyn PolicySnapshot,
    pub budget: &'a mut Budget,
}
impl View<'_> {
    pub fn entity(&mut self, id: &str) -> Result<Option<Arc<HDict>>, ReadError> {
        self.budget.check()?;
        if !self.policy.entity(id) {
            return Ok(None);
        }
        let Some(raw) = self.graph.get(id) else {
            return Ok(None);
        };
        record(id, raw, self.policy, self.budget).map(|row| row.map(Arc::new))
    }
    pub fn incoming(
        &mut self,
        target: &HRef,
        tag: Option<&str>,
    ) -> Result<Vec<Arc<HDict>>, ReadError> {
        let mut result = Vec::new();
        for (edge_tag, source) in self.graph.incoming_edges(&target.val) {
            self.budget.charge(BudgetKind::Inverse, 1)?;
            self.budget.charge(BudgetKind::Work, 1)?;
            if tag.is_some_and(|t| edge_tag != t) {
                continue;
            }
            let Some(row) = self.entity(source)? else {
                continue;
            };
            // Index membership alone is insufficient: the relation must survive
            // recursive field/reference masking on its source record.
            if !matches!(row.get(edge_tag), Some(Kind::Ref(r)) if r.val == target.val) {
                continue;
            }
            self.budget.charge(BudgetKind::Retained, 32)?;
            result.push(row);
        }
        Ok(result)
    }
}
/// Apply the same current entity/tag/reference/nominal visibility to retained
/// preimages as to live records. Missing live rows do not bypass current policy.
pub(crate) fn record(
    id: &str,
    raw: &HDict,
    policy: &dyn PolicySnapshot,
    budget: &mut Budget,
) -> Result<Option<HDict>, ReadError> {
    budget.check()?;
    if !policy.entity(id) {
        return Ok(None);
    }
    budget.charge(BudgetKind::Retained, 128)?;
    let mut out = HDict::new();
    for (tag, value) in raw.iter() {
        budget.charge(BudgetKind::Work, tag.len().saturating_add(1))?;
        if !policy.tag(id, tag) {
            continue;
        }
        let own_id = tag == "id" && matches!(value, Kind::Ref(r) if r.val == id);
        // Check all nested refs even when a nested tag will itself be hidden.
        // Any denied nested identity removes this entire top-level tag.
        if !member_visible(tag, value, own_id, policy, budget, 1)? {
            continue;
        }
        let name = budget.copy_string(tag)?;
        budget.charge(BudgetKind::Retained, 512)?;
        let value = copy_value(value, id, policy, budget, 1)?;
        out.set(name, value);
    }
    Ok(Some(out))
}
impl QueryEnvironment for View<'_> {
    type Error = ReadError;
    fn work(&mut self, amount: usize) -> Result<(), ReadError> {
        self.budget.charge(BudgetKind::Work, amount)
    }
    fn retain(&mut self, bytes: usize) -> Result<(), ReadError> {
        self.budget.charge(BudgetKind::Retained, bytes)
    }
    fn depth(&mut self, depth: usize) -> Result<(), ReadError> {
        self.budget.depth(depth)
    }
    fn comparison(&mut self, left: &Kind, right: &Kind) -> Result<(), ReadError> {
        measure(left, self.budget, 1)?;
        measure(right, self.budget, 1)
    }
    fn forward(&mut self, id: &HRef) -> Result<Option<Arc<HDict>>, ReadError> {
        self.budget.charge(BudgetKind::Forward, 1)?;
        if !self.policy.reference(&id.val) {
            return Ok(None);
        }
        self.entity(&id.val)
    }
    fn inverse(&mut self, target: &HRef, tag: &str) -> Result<Vec<Arc<HDict>>, ReadError> {
        self.incoming(target, Some(tag))
    }
    fn catalog_visible(&mut self, kind: CatalogKind, name: &str) -> Result<bool, ReadError> {
        self.budget
            .charge(BudgetKind::Work, name.len().saturating_add(1))?;
        Ok(self.policy.catalog(kind, name))
    }
    fn regex_source_limit(&self) -> usize {
        self.budget.limits.max_regex_source_bytes
    }
    fn regex_size_limit(&self) -> usize {
        self.budget.limits.max_regex_bytes
    }
    fn regex_limit_error(&self) -> ReadError {
        ReadError::Budget(BudgetKind::Regex)
    }
}

fn scalar_bytes(value: &Kind) -> usize {
    match value {
        Kind::Str(s) => s.len(),
        Kind::Ref(r) => r
            .val
            .len()
            .saturating_add(r.dis.as_ref().map_or(0, String::len)),
        Kind::Uri(s) => s.val().len(),
        Kind::Symbol(s) => s.val().len(),
        Kind::Number(n) => n.unit.as_ref().map_or(0, String::len),
        Kind::DateTime(d) => d.tz_name.len(),
        Kind::XStr(x) => x.type_name.len().saturating_add(x.val.len()),
        Kind::Buf(bytes) => bytes.len(),
        Kind::Nominal(n) => n
            .spec()
            .len()
            .saturating_add(n.catalog().len())
            .saturating_add(n.revision().len())
            .saturating_add(n.text().len()),
        _ => 0,
    }
}
fn visit_dict(
    dict: &HDict,
    policy: &dyn PolicySnapshot,
    budget: &mut Budget,
    depth: usize,
) -> Result<bool, ReadError> {
    for (tag, value) in dict.iter() {
        budget.charge(BudgetKind::Work, tag.len().saturating_add(1))?;
        if !member_visible(tag, value, false, policy, budget, depth)? {
            return Ok(false);
        }
    }
    Ok(true)
}
/// Structural `spec`/`of` annotations hold `lib::Name` catalog references,
/// resolved against the catalog rather than the entity store. They follow the
/// policy's catalog visibility for that declaration and its library, never
/// entity visibility: an entity allow-list does not strip them, and a
/// catalog-hidden declaration name is never disclosed.
fn catalog_reference<'a>(tag: &str, value: &'a Kind) -> Option<(&'a str, &'a str)> {
    if !matches!(tag, "spec" | "of") {
        return None;
    }
    let Kind::Ref(reference) = value else {
        return None;
    };
    let (library, name) = reference.val.split_once("::")?;
    (!library.is_empty() && !name.is_empty()).then_some((reference.val.as_str(), library))
}
fn member_visible(
    tag: &str,
    value: &Kind,
    own_id: bool,
    policy: &dyn PolicySnapshot,
    budget: &mut Budget,
    depth: usize,
) -> Result<bool, ReadError> {
    match catalog_reference(tag, value) {
        Some((qname, library)) => {
            budget.depth(depth)?;
            budget.charge(BudgetKind::Values, 1)?;
            budget.charge(BudgetKind::Work, qname.len().saturating_add(1))?;
            Ok(policy.catalog(CatalogKind::Spec, qname)
                && policy.catalog(CatalogKind::Library, library))
        }
        None => visible(value, own_id, policy, budget, depth),
    }
}
fn visible(
    value: &Kind,
    own_id: bool,
    policy: &dyn PolicySnapshot,
    budget: &mut Budget,
    depth: usize,
) -> Result<bool, ReadError> {
    budget.depth(depth)?;
    budget.charge(BudgetKind::Values, 1)?;
    budget.charge(BudgetKind::Work, scalar_bytes(value).saturating_add(1))?;
    match value {
        Kind::Ref(r) => Ok(own_id || (policy.entity(&r.val) && policy.reference(&r.val))),
        Kind::Nominal(n) => Ok(policy.catalog(CatalogKind::Spec, n.spec())
            && n.spec()
                .split_once("::")
                .is_some_and(|(lib, _)| policy.catalog(CatalogKind::Library, lib))
            && policy.nominal_provenance(n)),
        Kind::List(items) => {
            for value in items {
                if !visible(value, false, policy, budget, depth + 1)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        Kind::Dict(dict) => visit_dict(dict, policy, budget, depth + 1),
        Kind::Grid(grid) => {
            if !visit_dict(&grid.meta, policy, budget, depth + 1)? {
                return Ok(false);
            }
            for col in &grid.cols {
                budget.charge(BudgetKind::Work, col.name.len().saturating_add(1))?;
                if !visit_dict(&col.meta, policy, budget, depth + 1)? {
                    return Ok(false);
                }
            }
            for row in &grid.rows {
                if !visit_dict(row, policy, budget, depth + 1)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        _ => Ok(true),
    }
}
fn copy_dict(
    dict: &HDict,
    owner: &str,
    policy: &dyn PolicySnapshot,
    budget: &mut Budget,
    depth: usize,
) -> Result<HDict, ReadError> {
    budget.depth(depth)?;
    let mut out = HDict::new();
    for (tag, value) in dict.iter() {
        budget.charge(BudgetKind::Work, tag.len().saturating_add(1))?;
        if !policy.tag(owner, tag) {
            continue;
        }
        budget.charge(BudgetKind::Retained, 512)?;
        out.set(
            budget.copy_string(tag)?,
            copy_value(value, owner, policy, budget, depth + 1)?,
        );
    }
    Ok(out)
}
fn copy_value(
    value: &Kind,
    owner: &str,
    policy: &dyn PolicySnapshot,
    budget: &mut Budget,
    depth: usize,
) -> Result<Kind, ReadError> {
    budget.depth(depth)?;
    budget.charge(BudgetKind::Values, 1)?;
    budget.charge(BudgetKind::Retained, 128)?;
    match value {
        Kind::Ref(r) => {
            let display = if policy.reference_display(&r.val) {
                r.dis
                    .as_deref()
                    .map(|s| budget.copy_string(s))
                    .transpose()?
            } else {
                None
            };
            Ok(Kind::Ref(HRef::new(budget.copy_string(&r.val)?, display)))
        }
        Kind::List(items) => {
            let mut out = Vec::new();
            for value in items {
                out.push(copy_value(value, owner, policy, budget, depth + 1)?);
            }
            Ok(Kind::List(out))
        }
        Kind::Dict(dict) => Ok(Kind::Dict(Box::new(copy_dict(
            dict,
            owner,
            policy,
            budget,
            depth + 1,
        )?))),
        Kind::Grid(grid) => {
            let meta = copy_dict(&grid.meta, owner, policy, budget, depth + 1)?;
            let mut cols = Vec::new();
            for col in &grid.cols {
                budget.charge(BudgetKind::Work, 1)?;
                if !policy.tag(owner, &col.name) {
                    continue;
                }
                budget.charge(BudgetKind::Retained, 256)?;
                cols.push(HCol::with_meta(
                    budget.copy_string(&col.name)?,
                    copy_dict(&col.meta, owner, policy, budget, depth + 1)?,
                ));
            }
            let mut rows = Vec::new();
            for row in &grid.rows {
                budget.charge(BudgetKind::Retained, 128)?;
                rows.push(copy_dict(row, owner, policy, budget, depth + 1)?);
            }
            Ok(Kind::Grid(Box::new(HGrid::from_parts(meta, cols, rows))))
        }
        _ => {
            let bytes = scalar_bytes(value);
            budget.charge(BudgetKind::Work, bytes.saturating_add(1))?;
            budget.charge(BudgetKind::Retained, bytes)?;
            Ok(value.clone())
        }
    }
}
/// Charge comparisons and request literals without copying or applying policy to
/// caller-authored values. Actual records have already passed `visible`.
pub(crate) fn measure(value: &Kind, budget: &mut Budget, depth: usize) -> Result<(), ReadError> {
    budget.depth(depth)?;
    budget.charge(BudgetKind::Values, 1)?;
    budget.charge(BudgetKind::Work, scalar_bytes(value).saturating_add(1))?;
    match value {
        Kind::List(items) => {
            for value in items {
                measure(value, budget, depth + 1)?;
            }
        }
        Kind::Dict(dict) => {
            for (tag, value) in dict.iter() {
                budget.charge(BudgetKind::Work, tag.len())?;
                measure(value, budget, depth + 1)?;
            }
        }
        Kind::Grid(grid) => {
            for dict in std::iter::once(&grid.meta)
                .chain(grid.cols.iter().map(|c| &c.meta))
                .chain(grid.rows.iter())
            {
                for (tag, value) in dict.iter() {
                    budget.charge(BudgetKind::Work, tag.len())?;
                    measure(value, budget, depth + 1)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

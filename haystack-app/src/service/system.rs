//! Bounded native handlers for the exact admitted system-function subset.
use super::*;
use crate::{ApiError, registry::Handler};
use haystack_core::{
    data::HCol,
    kinds::{HDateTime, HRef, NominalScalar},
};
use std::sync::OnceLock;

pub(super) struct SystemInfo {
    pub boot: OnceLock<HDateTime>,
    pub name: Mutex<String>,
}
impl SystemInfo {
    pub fn new(managed: bool) -> Self {
        let boot = OnceLock::new();
        if !managed {
            let _ = boot.set(now());
        }
        Self {
            boot,
            name: Mutex::new("rusty-haystack".into()),
        }
    }
}
fn now() -> HDateTime {
    HDateTime::new(chrono::Utc::now().fixed_offset(), "UTC")
}
impl ReadService {
    pub(crate) fn start_system_clock(&self) {
        let _ = self.inner.system.boot.set(now());
    }
    pub(crate) fn set_server_name(&self, name: String) -> Result<(), ReadError> {
        if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
            return Err(ReadError::InvalidLimits);
        }
        *self.inner.system.name.lock() = name;
        Ok(())
    }
}
impl Inner {
    pub(super) fn system_read(
        &self,
        registry: &crate::registry::Registry,
        handler: Handler,
        args: &HDict,
        policy: &dyn PolicySnapshot,
        budget: &mut Budget,
    ) -> Result<Kind, ApiError> {
        let checked = matches!(args.get("checked"), Some(Kind::Bool(true)));
        if handler == Handler::ReadByIds {
            let Some(Kind::List(ids)) = args.get("ids") else {
                return Err(ApiError::Internal);
            };
            if ids.len() > budget.limits.max_ids {
                return Err(ReadError::Budget(BudgetKind::Ids).into());
            }
            if ids.len() > budget.limits.max_rows {
                return Err(ReadError::Budget(BudgetKind::Rows).into());
            }
            budget.charge(BudgetKind::Retained, ids.len().saturating_mul(128))?;
            loop {
                if let Some(result) = self.graph.read_for(budget.wait_quantum()?, |graph| {
                    current_observation(graph, registry)?;
                    let mut view = View {
                        graph,
                        policy,
                        budget,
                    };
                    let mut rows = Vec::with_capacity(ids.len());
                    for id in ids {
                        view.budget.charge(BudgetKind::Candidates, 1)?;
                        let Kind::Ref(id) = id else {
                            return Err(ApiError::Internal);
                        };
                        match view.entity(&id.val)? {
                            Some(row) => {
                                rows.push(Arc::try_unwrap(row).map_err(|_| ApiError::Internal)?)
                            }
                            None if checked => return Err(ApiError::UnknownEntity),
                            None => rows.push(HDict::new()),
                        }
                    }
                    let mut grid = output::grid(rows, true, None, view.budget)?;
                    grid.meta.remove_tag("complete");
                    if !ids.is_empty() && grid.col("id").is_none() {
                        view.budget.charge(BudgetKind::Retained, 256)?;
                        grid.cols.insert(0, HCol::new("id"));
                    }
                    Ok(Kind::Grid(Box::new(grid)))
                }) {
                    return result;
                }
            }
        }
        let Some(Kind::Nominal(source)) = args.get("filter") else {
            return Err(ApiError::Internal);
        };
        let text = source.text();
        budget.charge(
            BudgetKind::Retained,
            text.len().saturating_mul(512).saturating_add(1024),
        )?;
        budget.charge(BudgetKind::Work, text.len().saturating_add(1))?;
        let ast = filter::parse_filter_controlled(
            text,
            FilterParseLimits {
                max_bytes: budget.limits.max_input_bytes,
                max_nodes: budget.limits.max_ast_nodes,
                max_depth: budget.limits.max_ast_depth,
            },
            &mut || budget.check().map_err(|_| FilterError::Interrupted),
        )
        .map_err(|err| match err {
            FilterError::Limit => ReadError::Budget(BudgetKind::Ast),
            FilterError::Interrupted => budget.check().err().unwrap_or(ReadError::Cancelled),
            FilterError::Parse { .. } => ReadError::InvalidQuery("invalid filter"),
        })?;
        let query = NormalizedQuery::Filter(Some(ast));
        let mut limit = if handler == Handler::Read {
            Some(1)
        } else {
            None
        };
        let mut sort = false;
        if let Some(Kind::Dict(opts)) = args.get("opts") {
            for (name, value) in opts.iter() {
                match name {
                    "limit" => {
                        let n = match value {
                            Kind::Int(n) if *n >= 0 => usize::try_from(*n).ok(),
                            Kind::Float(n)
                                if n.value().is_finite()
                                    && n.value() >= 0.0
                                    && n.value().fract() == 0.0
                                    && n.value() <= budget.limits.max_rows as f64 =>
                            {
                                Some(n.value() as usize)
                            }
                            Kind::Number(n)
                                if n.unit.is_none()
                                    && n.val.is_finite()
                                    && n.val >= 0.0
                                    && n.val.fract() == 0.0
                                    && n.val <= budget.limits.max_rows as f64 =>
                            {
                                Some(n.val as usize)
                            }
                            _ => None,
                        }
                        .ok_or(ApiError::InvalidArgs)?;
                        if n > budget.limits.max_rows {
                            return Err(ApiError::InvalidArgs);
                        }
                        limit = Some(n);
                    }
                    "sort" => sort = true,
                    // Valid structural Dict metadata is retained by decoding;
                    // it is not an option. Other spec values remain invalid.
                    "spec" if matches!(value, Kind::Ref(spec) if spec.val == "sys::Dict") => {}
                    _ => return Err(ApiError::InvalidArgs),
                }
            }
        }
        loop {
            if let Some(result) = self.graph.read_for(budget.wait_quantum()?, |graph| {
                current_observation(graph, registry)?;
                let mut view = View {
                    graph,
                    policy,
                    budget,
                };
                validate_query(&query, &mut view)?;
                let NormalizedQuery::Filter(Some(ast)) = &query else {
                    return Err(ApiError::Internal);
                };
                let mut rows = Vec::new();
                for (id, _) in graph.entities_after(None) {
                    if limit == Some(rows.len()) {
                        break;
                    }
                    view.budget.charge(BudgetKind::Candidates, 1)?;
                    let Some(row) = view.entity(id)? else {
                        continue;
                    };
                    if !filter::matches_controlled(ast, row.clone(), graph.namespace(), &mut view)?
                    {
                        continue;
                    }
                    if rows.len() >= view.budget.limits.max_rows {
                        return Err(ReadError::Budget(BudgetKind::Rows).into());
                    }
                    view.budget.charge(BudgetKind::Retained, 128)?;
                    rows.push(Arc::try_unwrap(row).map_err(|_| ApiError::Internal)?);
                }
                if handler == Handler::Read {
                    return match rows.pop() {
                        Some(row) => Ok(Kind::Dict(Box::new(row))),
                        None if checked => Err(ApiError::UnknownEntity),
                        None => Ok(Kind::Null),
                    };
                }
                if sort {
                    // Reserve a conservative comparison bound before native sort.
                    let longest = rows
                        .iter()
                        .map(|r| display(r).len().saturating_add(id(r).len()))
                        .max()
                        .unwrap_or(0);
                    let comparisons = rows
                        .len()
                        .saturating_mul(rows.len().max(1).ilog2() as usize + 1)
                        .saturating_mul(4);
                    view.budget.charge(
                        BudgetKind::Work,
                        comparisons.saturating_mul(longest.saturating_add(1)),
                    )?;
                    rows.sort_by(|a, b| display(a).cmp(display(b)).then_with(|| id(a).cmp(id(b))));
                    view.budget.check()?;
                }
                let mut grid = output::grid(rows, true, None, view.budget)?;
                grid.meta.remove_tag("complete");
                Ok(Kind::Grid(Box::new(grid)))
            }) {
                return result;
            }
        }
    }
    fn nominal(
        registry: &crate::registry::Registry,
        spec: &str,
        text: &str,
        budget: &mut Budget,
    ) -> Result<Kind, ApiError> {
        let provenance = registry.catalog().provenance();
        budget.charge(BudgetKind::Retained, 512)?;
        Ok(Kind::Nominal(
            NominalScalar::new(
                budget.copy_string(spec)?,
                budget.copy_string(&provenance.repository)?,
                budget.copy_string(&provenance.commit)?,
                budget.copy_string(text)?,
            )
            .map_err(|_| ApiError::Internal)?,
        ))
    }
    pub(super) fn about(
        &self,
        registry: &crate::registry::Registry,
        principal: &Principal,
        budget: &mut Budget,
    ) -> Result<Kind, ApiError> {
        budget.charge(BudgetKind::Retained, 8192)?;
        budget.charge(BudgetKind::Values, 16)?;
        let mut info = HDict::new();
        info.set(
            "serverName",
            Kind::Str(budget.copy_string(&self.system.name.lock())?),
        );
        info.set("serverTime", Kind::DateTime(now()));
        info.set(
            "serverBootTime",
            Kind::DateTime(self.system.boot.get().ok_or(ApiError::Unavailable)?.clone()),
        );
        info.set(
            "tz",
            Self::nominal(registry, "sys::TimeZone", "UTC", budget)?,
        );
        info.set(
            "protocolVersions",
            Kind::List(vec![Kind::Str("4".into()), Kind::Str("5".into())]),
        );
        info.set("productName", Kind::Str("rusty-haystack".into()));
        info.set(
            "productVersion",
            Kind::Str(env!("CARGO_PKG_VERSION").into()),
        );
        if let Principal::Authenticated { subject, .. } | Principal::TrustedEmbedding { subject } =
            principal
        {
            info.set("whoami", Kind::Str(budget.copy_string(subject)?));
        }
        Ok(Kind::Dict(Box::new(info)))
    }
    pub(super) fn libraries(
        &self,
        registry: &crate::registry::Registry,
        policy: &dyn PolicySnapshot,
        budget: &mut Budget,
    ) -> Result<Kind, ApiError> {
        let mut rows = Vec::new();
        // Only libraries admitted by the retained observation are advertised.
        for library in registry.catalog().libraries() {
            budget.charge(BudgetKind::Candidates, 1)?;
            budget.charge(BudgetKind::Work, library.name.len().saturating_add(1))?;
            if !policy.catalog(CatalogKind::Library, &library.name) {
                continue;
            }
            if rows.len() >= budget.limits.max_rows {
                return Err(ReadError::Budget(BudgetKind::Rows).into());
            }
            budget.charge(BudgetKind::Retained, 2048)?;
            budget.charge(BudgetKind::Values, 4)?;
            let mut row = HDict::new();
            row.set("name", Kind::Str(budget.copy_string(&library.name)?));
            row.set(
                "version",
                Self::nominal(registry, "sys::Version", &library.version, budget)?,
            );
            if !library.doc.is_empty() {
                row.set(
                    "doc",
                    Kind::Str(
                        budget.copy_string(library.doc.split('.').next().unwrap_or("").trim())?,
                    ),
                );
            }
            rows.push(row);
        }
        typed_grid(rows, "sys.api::LibInfo", budget)
    }
    pub(super) fn filetypes(&self, version: &str, budget: &mut Budget) -> Result<Kind, ApiError> {
        let mut rows = Vec::new();
        for (name, dis, mime, ext, spec) in crate::typed_http::filetypes(version) {
            budget.charge(BudgetKind::Candidates, 1)?;
            if rows.len() >= budget.limits.max_rows {
                return Err(ReadError::Budget(BudgetKind::Rows).into());
            }
            budget.charge(BudgetKind::Retained, 4096)?;
            budget.charge(BudgetKind::Values, 8)?;
            let mut row = HDict::new();
            for (tag, value) in [
                ("name", name),
                ("dis", dis),
                ("mime", mime),
                ("fileExt", ext),
            ] {
                row.set(tag, Kind::Str(budget.copy_string(value)?));
            }
            row.set(
                "fileSpec",
                Kind::Ref(HRef::from_val(budget.copy_string(spec)?)),
            );
            row.set("canRead", Kind::Bool(true));
            row.set("canWrite", Kind::Bool(true));
            rows.push(row);
        }
        typed_grid(rows, "sys.api::FiletypeInfo", budget)
    }
}
fn id(row: &HDict) -> &str {
    match row.get("id") {
        Some(Kind::Ref(id)) => &id.val,
        _ => "",
    }
}
fn display(row: &HDict) -> &str {
    row.dis().unwrap_or_else(|| id(row))
}
fn typed_grid(rows: Vec<HDict>, of: &str, budget: &mut Budget) -> Result<Kind, ApiError> {
    let mut grid = output::grid(rows, true, None, budget)?;
    grid.meta.remove_tag("complete");
    grid.meta
        .set("of", Kind::Ref(HRef::from_val(budget.copy_string(of)?)));
    Ok(Kind::Grid(Box::new(grid)))
}

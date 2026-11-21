//! Typed `ph.api::nav` over the shared authorized read view.
//!
//! The selected profile mirrors scoped H4 navigation: the root lists the
//! caller's visible sites, a child level is one authorized inverse hop, rows
//! are ordered by entity id and use the H4 `nav_row` shape. A hidden, missing
//! or otherwise unsupported navigation target is an empty complete page, so
//! the response is never an existence oracle. One request returns at most
//! `max_rows` rows; a larger level is truncated deterministically with
//! `complete: false`. Request budgets still bind (atomically, never as partial
//! rows). There is no typed continuation cursor.
use super::*;
use crate::ApiError;
use haystack_core::data::{HCol, HGrid};
use std::collections::BTreeSet;

/// Request-grid controls the H4 nav route interprets for paging and
/// projection. The typed function has neither, so they reject rather than
/// being silently ignored.
const H4_PAGE_CONTROLS: &[&str] = &["limit", "cursor", "select"];

impl Inner {
    pub(super) fn nav(
        &self,
        registry: &crate::registry::Registry,
        args: &HDict,
        policy: &dyn PolicySnapshot,
        budget: &mut Budget,
    ) -> Result<Kind, ApiError> {
        let Some(Kind::Grid(request)) = args.get("req") else {
            return Err(ApiError::Internal);
        };
        let target = target(request, budget)?;
        loop {
            if let Some(result) = self.graph.read_for(budget.wait_quantum()?, |graph| {
                // The retained observation must still be current before any
                // graph evaluation; a newer activation yields Unavailable.
                current_observation(graph, registry)?;
                let mut view = View {
                    graph,
                    policy,
                    budget,
                };
                let (rows, complete) = match target.as_deref() {
                    None => sites(&mut view)?,
                    Some(parent) => children(parent, &mut view)?,
                };
                let mut grid = output::grid(rows, complete, None, view.budget)?;
                // Navigation children always carry a navId column, including
                // an empty level and rows whose id tag is masked (leaves).
                if grid.col("navId").is_none() {
                    view.budget.charge(BudgetKind::Retained, 256)?;
                    grid.cols.push(HCol::new("navId"));
                }
                Ok::<_, ApiError>(Kind::Grid(Box::new(grid)))
            }) {
                return result;
            }
        }
    }
}

/// The selected request shape: an empty grid (whatever placeholder columns
/// its format needs, such as Zinc's `empty`) or one row with only a `navId`
/// column. An empty grid and an empty string select the root, as in H4; a
/// null navId also selects it here (scoped H4 rejects null). A Str or Ref
/// navId selects that entity; other kinds, more rows, other columns and H4
/// page controls are invalid.
fn target(request: &HGrid, budget: &mut Budget) -> Result<Option<String>, ApiError> {
    budget.charge(
        BudgetKind::Work,
        request
            .cols
            .len()
            .saturating_add(request.meta.len())
            .saturating_add(1),
    )?;
    if request.rows.len() > 1 || H4_PAGE_CONTROLS.iter().any(|name| request.meta.has(name)) {
        return Err(ApiError::InvalidArgs);
    }
    let Some(row) = request.rows.first() else {
        return Ok(None);
    };
    if request.cols.iter().any(|col| col.name != "navId")
        || row.tag_names().any(|name| name != "navId")
    {
        return Err(ApiError::InvalidArgs);
    }
    match row.get("navId") {
        None | Some(Kind::Null) => Ok(None),
        Some(Kind::Str(id)) if id.is_empty() => Ok(None),
        Some(Kind::Str(id)) => Ok(Some(budget.copy_string(id)?)),
        Some(Kind::Ref(id)) => Ok(Some(budget.copy_string(&id.val)?)),
        Some(_) => Err(ApiError::InvalidArgs),
    }
}

/// Visible sites in entity id order, after current entity/tag masking.
///
/// Every entity costs one candidate, but only a possible site is copied
/// through the masked View. Masking only removes tags, so a record whose raw
/// form lacks `site`, or whose entity or `site` tag the policy denies, can
/// never yield a masked row with `site`; skipping it changes no result. The
/// masked row stays authoritative, since the tag's value may still be masked.
fn sites(view: &mut View<'_>) -> Result<(Vec<HDict>, bool), ApiError> {
    let graph = view.graph;
    let mut rows = Vec::new();
    for (id, raw) in graph.entities_after(None) {
        view.budget.charge(BudgetKind::Candidates, 1)?;
        if !raw.has("site") || !view.policy.entity(id) || !view.policy.tag(id, "site") {
            continue;
        }
        let Some(row) = view.entity(id)? else {
            continue;
        };
        if !row.has("site") {
            continue;
        }
        if !accept(row, &mut rows, view.budget)? {
            return Ok((rows, false));
        }
    }
    Ok((rows, true))
}

/// One authorized inverse hop. The parent must be visible and referenceable;
/// each candidate is masked first and its relation must survive masking (any
/// top-level non-id Ref to the parent, the H4 nav relation). Inverse edges,
/// including denied and duplicate ones, are charged before any record copy.
/// Single-hop traversal never revisits a level, so reference cycles are safe.
fn children(parent: &str, view: &mut View<'_>) -> Result<(Vec<HDict>, bool), ApiError> {
    if !view.policy.entity(parent) || !view.policy.reference(parent) || !view.graph.contains(parent)
    {
        return Ok((Vec::new(), true));
    }
    let graph = view.graph;
    let mut sources = BTreeSet::new();
    for (_, source) in graph.incoming_edges(parent) {
        view.budget.charge(BudgetKind::Inverse, 1)?;
        view.budget.charge(BudgetKind::Work, 1)?;
        if sources.insert(source) {
            view.budget.charge(BudgetKind::Retained, 32)?;
        }
    }
    let mut rows = Vec::new();
    // BTreeSet<&str> iterates in the graph's entity-id (B-tree) order.
    for id in sources {
        view.budget.charge(BudgetKind::Candidates, 1)?;
        let Some(row) = view.entity(id)? else {
            continue;
        };
        let mut related = false;
        for (tag, value) in row.iter() {
            view.budget.charge(BudgetKind::Work, 1)?;
            if tag != "id" && matches!(value, Kind::Ref(r) if r.val == parent) {
                related = true;
                break;
            }
        }
        if related && !accept(row, &mut rows, view.budget)? {
            return Ok((rows, false));
        }
    }
    Ok((rows, true))
}

/// Append one navigation row, or report that the bounded page is full and a
/// further visible row exists.
fn accept(row: Arc<HDict>, rows: &mut Vec<HDict>, budget: &mut Budget) -> Result<bool, ApiError> {
    if rows.len() >= budget.limits.max_rows {
        return Ok(false);
    }
    let row = Arc::try_unwrap(row).map_err(|_| ApiError::Internal)?;
    rows.push(nav_row(row, budget)?);
    budget.charge(BudgetKind::Retained, 128)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AllowAll, FunctionIdentity, TypedInvocationInput};
    use haystack_core::{
        graph::EntityGraph,
        kinds::{HRef, NominalScalar},
        xeto::catalog::Catalog,
    };
    use std::time::Duration;

    fn entity(id: &str, tags: &[(&str, Kind)]) -> HDict {
        let mut row = HDict::new();
        row.set("id", Kind::Ref(HRef::from_val(id)));
        for (tag, value) in tags {
            row.set(*tag, value.clone());
        }
        row
    }
    fn reference(id: &str) -> Kind {
        Kind::Ref(HRef::from_val(id))
    }
    fn nav(target: Option<&str>) -> TypedInvocationInput {
        let body = match target {
            None => "ver:\"3.0\"\nnavId\n".to_owned(),
            Some(id) => format!("ver:\"3.0\"\nnavId\n\"{id}\"\n"),
        };
        TypedInvocationInput {
            operation: "ph.api::nav".into(),
            versions: vec!["5".into()],
            post: true,
            content_types: vec!["text/zinc".into()],
            body: body.into_bytes(),
            ..Default::default()
        }
    }
    fn context(subject: Option<&str>) -> ReadContext {
        let principal = match subject {
            Some(subject) => Principal::TrustedEmbedding {
                subject: subject.into(),
            },
            None => Principal::Anonymous,
        };
        ReadContext::with_timeout(principal, Duration::from_secs(5))
    }
    fn ids(body: &[u8]) -> (Vec<String>, serde_json::Value) {
        let value: serde_json::Value = serde_json::from_slice(body).unwrap();
        let ids = value["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["navId"].as_str().unwrap_or("<leaf>").to_owned())
            .collect();
        (ids, value)
    }

    /// Navigation is a distinct coarse operation; its denial hides the
    /// typed binding exactly as it forbids the H4 route.
    struct DenyNav;
    impl ReadPolicy for DenyNav {
        fn snapshot(&self, _: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
            Ok(Arc::new(Self))
        }
    }
    impl PolicySnapshot for DenyNav {
        fn scope_key(&self) -> &str {
            "deny-nav"
        }
        fn function(&self, _: &FunctionIdentity) -> bool {
            true
        }
        fn operation(&self, operation: ReadOperation) -> bool {
            operation != ReadOperation::Nav
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
        fn catalog(&self, kind: CatalogKind, name: &str) -> bool {
            AllowAll.catalog(kind, name)
        }
        fn nominal_provenance(&self, _: &NominalScalar) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn nav_operation_denial_hides_the_binding_like_the_h4_route() {
        let graph = SharedGraph::new(EntityGraph::new());
        graph
            .add(entity("site-1", &[("site", Kind::Marker)]))
            .unwrap();
        let reads = ReadService::new(graph, Arc::new(DenyNav), ReadLimits::default()).unwrap();
        let error = reads
            .begin(context(None))
            .await
            .unwrap()
            .invoke_wire(nav(None))
            .await
            .err()
            .unwrap();
        assert_eq!(error, ApiError::UnknownFunction("ph.api::nav".into()));
        let ops = reads
            .begin(context(None))
            .await
            .unwrap()
            .invoke_wire(TypedInvocationInput {
                operation: "ops".into(),
                versions: vec!["5".into()],
                ..Default::default()
            })
            .await
            .unwrap();
        let ops = String::from_utf8(ops.body).unwrap();
        assert!(!ops.contains("ph.api::nav"), "{ops}");
        assert!(ops.contains("sys.api::readById"), "{ops}");
    }

    #[test]
    fn request_shape_selects_root_or_one_target_and_rejects_page_controls() {
        let mut budget = Budget::new(
            Arc::new(ReadLimits::default()),
            Instant::now() + Duration::from_secs(5),
            CancellationToken::new(),
        );
        let request = |cols: &[&str], rows: Vec<HDict>, meta: &[(&str, Kind)]| {
            let mut grid = HGrid::from_parts(
                HDict::new(),
                cols.iter().map(|c| HCol::new(*c)).collect(),
                rows,
            );
            for (name, value) in meta {
                grid.meta.set(*name, value.clone());
            }
            grid
        };
        let row = |value: Kind| {
            let mut row = HDict::new();
            row.set("navId", value);
            row
        };
        for (grid, expected) in [
            (request(&[], vec![], &[]), None),
            (request(&["navId"], vec![], &[]), None),
            // An empty grid selects the root whatever placeholder columns it has.
            (request(&["empty"], vec![], &[]), None),
            (request(&["navId"], vec![HDict::new()], &[]), None),
            (request(&["navId"], vec![row(Kind::Null)], &[]), None),
            (
                request(&["navId"], vec![row(Kind::Str(String::new()))], &[]),
                None,
            ),
            (
                request(&["navId"], vec![row(Kind::Str("a".into()))], &[]),
                Some("a"),
            ),
            (
                request(
                    &["navId"],
                    vec![row(Kind::Ref(HRef::new("b", Some("B".into()))))],
                    &[],
                ),
                Some("b"),
            ),
            // Unrelated metadata, such as the H4 version tag, is not interpreted.
            (
                request(&["navId"], vec![], &[("ver", Kind::Str("3.0".into()))]),
                None,
            ),
        ] {
            assert_eq!(target(&grid, &mut budget).unwrap().as_deref(), expected);
        }
        let mut extra = row(Kind::Str("a".into()));
        extra.set("limit", Kind::Int(1));
        for grid in [
            request(
                &["navId"],
                vec![row(Kind::Str("a".into())), row(Kind::Str("b".into()))],
                &[],
            ),
            request(&["navId", "limit"], vec![extra.clone()], &[]),
            request(&["navId"], vec![extra], &[]),
            request(&["navId"], vec![row(Kind::Marker)], &[]),
            request(&["navId"], vec![row(Kind::Int(3))], &[]),
            request(&["navId"], vec![], &[("limit", Kind::Int(1))]),
            request(&["navId"], vec![], &[("cursor", Kind::Str("x".into()))]),
            request(&["navId"], vec![], &[("select", Kind::List(vec![]))]),
        ] {
            assert_eq!(target(&grid, &mut budget), Err(ApiError::InvalidArgs));
        }
    }

    /// Activation publishes a new observation and bumps the catalog
    /// generation. A nav request that captured the previous observation is
    /// stopped before graph evaluation and never returns rows from it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn activation_makes_a_retained_nav_request_unavailable_before_evaluation() {
        let graph = SharedGraph::new(EntityGraph::new());
        graph
            .add(entity("site-1", &[("site", Kind::Marker)]))
            .unwrap();
        graph
            .add(entity("equip-1", &[("siteRef", reference("site-1"))]))
            .unwrap();
        let reads =
            ReadService::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default()).unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let release = std::sync::Mutex::new(release_rx);
        let mut admission = reads.begin(context(None)).await.unwrap();
        admission.budget_mut().typed_lookup_hook = Some(Arc::new(move || {
            entered_tx.send(()).unwrap();
            release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(3))
                .unwrap();
        }));
        let stale = tokio::spawn(admission.invoke_wire(nav(Some("site-1"))));
        tokio::task::spawn_blocking(move || entered_rx.recv_timeout(Duration::from_secs(3)))
            .await
            .unwrap()
            .unwrap();
        let generation = graph.state().catalog_generation;
        let published = reads
            .activate_catalog(
                context(Some("owner")),
                Catalog::load_protocol_pinned().unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(published.catalog_generation, generation + 1);
        release_tx.send(()).unwrap();
        assert!(matches!(stale.await.unwrap(), Err(ApiError::Unavailable)));
        // A fresh request binds the new observation and navigates normally.
        let fresh = reads
            .begin(context(None))
            .await
            .unwrap()
            .invoke_wire(nav(Some("site-1")))
            .await
            .unwrap();
        assert_eq!(ids(&fresh.body).0, ["equip-1"]);
    }

    /// A masked `id` tag leaves a visible row with no navigation identity:
    /// it is a leaf, and the result still carries the navId column.
    struct MaskId;
    impl ReadPolicy for MaskId {
        fn snapshot(&self, _: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
            Ok(Arc::new(Self))
        }
    }
    impl PolicySnapshot for MaskId {
        fn scope_key(&self) -> &str {
            "mask-id"
        }
        fn function(&self, _: &FunctionIdentity) -> bool {
            true
        }
        fn operation(&self, _: ReadOperation) -> bool {
            true
        }
        fn entity(&self, _: &str) -> bool {
            true
        }
        fn tag(&self, entity: &str, tag: &str) -> bool {
            !(entity == "b" && tag == "id")
        }
        fn reference(&self, _: &str) -> bool {
            true
        }
        fn reference_display(&self, _: &str) -> bool {
            true
        }
        fn catalog(&self, kind: CatalogKind, name: &str) -> bool {
            AllowAll.catalog(kind, name)
        }
        fn nominal_provenance(&self, _: &NominalScalar) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn masked_identity_is_a_leaf_row_and_navid_column_remains() {
        let graph = SharedGraph::new(EntityGraph::new());
        for id in ["a", "b"] {
            graph
                .add(entity(
                    id,
                    &[("site", Kind::Marker), ("dis", Kind::Str(id.into()))],
                ))
                .unwrap();
        }
        let reads = ReadService::new(graph, Arc::new(MaskId), ReadLimits::default()).unwrap();
        let body = reads
            .begin(context(None))
            .await
            .unwrap()
            .invoke_wire(nav(None))
            .await
            .unwrap()
            .body;
        let (ids, value) = ids(&body);
        assert_eq!(ids, ["a", "<leaf>"]);
        assert_eq!(value["rows"][1], serde_json::json!({"dis": "b"}));
        assert_eq!(
            value["cols"],
            serde_json::json!([{"name": "dis"}, {"name": "id"}, {"name": "navId"}])
        );
    }

    #[tokio::test]
    async fn single_hop_cycles_and_self_references_terminate_in_id_order() {
        let graph = SharedGraph::new(EntityGraph::new());
        for row in [
            entity("a", &[("site", Kind::Marker), ("peerRef", reference("b"))]),
            entity(
                "b",
                &[("peerRef", reference("a")), ("otherRef", reference("a"))],
            ),
            entity(
                "loop",
                &[("selfRef", reference("loop")), ("site", Kind::Marker)],
            ),
        ] {
            graph.add(row).unwrap();
        }
        let reads = ReadService::new(graph, Arc::new(AllowAll), ReadLimits::default()).unwrap();
        let call = |target: Option<&'static str>| {
            let reads = reads.clone();
            async move {
                ids(&reads
                    .begin(context(None))
                    .await
                    .unwrap()
                    .invoke_wire(nav(target))
                    .await
                    .unwrap()
                    .body)
                .0
            }
        };
        assert_eq!(call(None).await, ["a", "loop"]);
        // b relates to a through two tags but appears once.
        assert_eq!(call(Some("a")).await, ["b"]);
        assert_eq!(call(Some("b")).await, ["a"]);
        assert_eq!(call(Some("loop")).await, ["loop"]);
    }
}

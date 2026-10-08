use haystack_app::*;
use haystack_core::{
    codecs::codec_for,
    data::{HCol, HDict, HGrid},
    graph::{EntityGraph, SharedGraph},
    kinds::{HRef, Kind, NominalScalar, Number},
    ontology::DefNamespace,
    xeto::{Slot, Spec},
};
use std::{
    sync::{
        Arc, Barrier,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone)]
struct Rules {
    generation: Arc<AtomicU64>,
}
struct Snapshot {
    scope: String,
}
impl ReadPolicy for Rules {
    fn snapshot(&self, _: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
        Ok(Arc::new(Snapshot {
            scope: format!("scope-{}", self.generation.load(Ordering::SeqCst)),
        }))
    }
}
impl PolicySnapshot for Snapshot {
    fn scope_key(&self) -> &str {
        &self.scope
    }
    fn operation(&self, _: ReadOperation) -> bool {
        true
    }
    fn entity(&self, id: &str) -> bool {
        id != "denied" && !id.starts_with("d-")
    }
    fn tag(&self, _: &str, tag: &str) -> bool {
        tag != "secret"
    }
    fn reference(&self, target: &str) -> bool {
        target != "denied" && target != "ref-blocked"
    }
    fn reference_display(&self, _: &str) -> bool {
        false
    }
    fn catalog(&self, _: CatalogKind, name: &str) -> bool {
        !name.starts_with("hidden")
    }
    fn nominal_provenance(&self, value: &NominalScalar) -> bool {
        value.catalog() != "secret-catalog"
    }
}
fn rules() -> Rules {
    Rules {
        generation: Arc::new(AtomicU64::new(0)),
    }
}
fn context() -> ReadContext {
    ReadContext::with_timeout(
        Principal::TrustedEmbedding {
            subject: "fixture-owner".into(),
        },
        Duration::from_secs(5),
    )
}
fn row(id: &str) -> HDict {
    let mut row = HDict::new();
    row.set(
        "id",
        Kind::Ref(HRef::new(id, Some("hidden display".into()))),
    );
    row.set("site", Kind::Marker);
    row
}
fn make_graph(ids: &[&str]) -> SharedGraph {
    let graph = SharedGraph::new(EntityGraph::new());
    for id in ids {
        graph.add(row(id)).unwrap();
    }
    graph
}
fn service(graph: SharedGraph, limits: ReadLimits) -> ReadService {
    ReadService::new(graph, Arc::new(rules()), limits).unwrap()
}
fn request(query: ReadQuery) -> ReadRequest {
    ReadRequest::new(query, OutputProfile::Typed)
}
fn typed(page: ReadPage) -> HGrid {
    let ReadOutput::Typed(grid) = page.output else {
        panic!("typed profile");
    };
    grid
}
fn ids(grid: &HGrid) -> Vec<&str> {
    grid.rows
        .iter()
        .map(|r| r.id().unwrap().val.as_str())
        .collect()
}

#[tokio::test]
async fn ids_are_sorted_deduplicated_and_denied_equals_missing() {
    let svc = service(
        make_graph(&["z", "a", "b", "denied"]),
        ReadLimits::default(),
    );
    let result = typed(
        svc.read(
            context(),
            request(ReadQuery::Ids(vec![
                "z".into(),
                "a".into(),
                "a".into(),
                "denied".into(),
                "missing".into(),
                "b".into(),
            ])),
        )
        .await
        .unwrap(),
    );
    assert_eq!(ids(&result), ["a", "b", "z"]);
    assert!(result.rows.iter().all(|r| r.id().unwrap().dis.is_none()));
    let denied = typed(
        svc.read(context(), request(ReadQuery::Ids(vec!["denied".into()])))
            .await
            .unwrap(),
    );
    let missing = typed(
        svc.read(context(), request(ReadQuery::Ids(vec!["missing".into()])))
            .await
            .unwrap(),
    );
    assert_eq!(denied, missing);
}

#[tokio::test]
async fn recursive_masking_precedes_predicates_and_keeps_only_root_identity_exemption() {
    let graph = make_graph(&["allowed-target", "ref-blocked", "denied"]);
    let mut entity = row("a");
    entity.set("secret", Kind::Str("do not disclose".into()));
    entity.set(
        "good",
        Kind::Ref(HRef::new(
            "allowed-target",
            Some("hidden ref display".into()),
        )),
    );
    entity.set("bad", Kind::Ref(HRef::from_val("denied")));
    entity.set(
        "nestedList",
        Kind::List(vec![Kind::Ref(HRef::from_val("denied"))]),
    );
    let mut nested = HDict::new();
    nested.set("id", Kind::Ref(HRef::from_val("ref-blocked")));
    entity.set("nestedId", Kind::Dict(Box::new(nested)));
    for (tag, where_) in [("gridMeta", 0), ("columnMeta", 1), ("gridRow", 2)] {
        let mut denied = HDict::new();
        denied.set("x", Kind::Ref(HRef::from_val("denied")));
        let grid = match where_ {
            0 => HGrid::from_parts(denied, vec![], vec![]),
            1 => HGrid::from_parts(HDict::new(), vec![HCol::with_meta("x", denied)], vec![]),
            _ => HGrid::from_parts(HDict::new(), vec![], vec![denied]),
        };
        entity.set(tag, Kind::Grid(Box::new(grid)));
    }
    entity.set(
        "hiddenType",
        Kind::Nominal(NominalScalar::new("hidden::Serial", "catalog", "rev", "v").unwrap()),
    );
    entity.set(
        "hiddenProvenance",
        Kind::Nominal(NominalScalar::new("demo::Serial", "secret-catalog", "rev", "v").unwrap()),
    );
    graph.add(entity).unwrap();
    let svc = service(graph, ReadLimits::default());
    let out = typed(
        svc.read(context(), request(ReadQuery::Filter("not secret".into())))
            .await
            .unwrap(),
    );
    assert_eq!(ids(&out), ["a", "allowed-target", "ref-blocked"]);
    let a = &out.rows[0];
    for tag in [
        "secret",
        "bad",
        "nestedList",
        "nestedId",
        "gridMeta",
        "columnMeta",
        "gridRow",
        "hiddenType",
        "hiddenProvenance",
    ] {
        assert!(a.missing(tag), "{tag}");
    }
    assert!(
        matches!(a.get("good"),Some(Kind::Ref(r)) if r.val=="allowed-target" && r.dis.is_none())
    );
    assert!(out.rows[2].id().is_some());
    assert!(
        typed(
            svc.read(context(), request(ReadQuery::Filter("secret".into())))
                .await
                .unwrap()
        )
        .is_empty()
    );
}

#[tokio::test]
async fn pages_bind_every_state_and_authority_component_and_do_not_expose_cursor_data() {
    for change in 0..10 {
        let graph = make_graph(&["z", "a", "b", "denied"]);
        let policy = rules();
        let svc = ReadService::new(
            graph.clone(),
            Arc::new(policy.clone()),
            ReadLimits::default(),
        )
        .unwrap();
        let mut req = request(ReadQuery::Filter(" * ".into()));
        req.page_size = 1;
        let first = svc.read(context(), req.clone()).await.unwrap();
        assert!(!first.complete);
        let token = first.cursor.unwrap();
        assert_eq!(token.len(), 64);
        assert!(!token.contains("fixture-owner"));
        assert!(!token.contains("denied"));
        req.cursor = Some(token);
        let mut ctx = context();
        match change {
            0 => {
                graph.add(row("c")).unwrap();
            }
            1 => graph.set_namespace(DefNamespace::new()),
            2 => graph.write(|g| *g = EntityGraph::new()),
            3 => ctx.principal = Principal::Anonymous,
            4 => policy.generation.store(1, Ordering::SeqCst),
            5 => req.profile = OutputProfile::H4(H4Codec::Json),
            6 => req.projection = vec!["id".into()],
            7 => req.query = ReadQuery::Filter("site".into()),
            8 => req.page_size = 2,
            9 => {
                let other = service(graph, ReadLimits::default());
                assert_eq!(
                    other.read(ctx, req).await.unwrap_err(),
                    ReadError::StaleCursor
                );
                continue;
            }
            _ => unreachable!(),
        }
        assert_eq!(
            svc.read(ctx, req).await.unwrap_err(),
            ReadError::StaleCursor,
            "change {change}"
        );
    }
}

#[tokio::test]
async fn pages_are_stable_and_completion_does_not_count_denied_rows() {
    let svc = service(
        make_graph(&["z", "a", "b", "denied"]),
        ReadLimits::default(),
    );
    let mut req = request(ReadQuery::Filter("*".into()));
    req.page_size = 1;
    let mut all = Vec::new();
    loop {
        let page = svc.read(context(), req.clone()).await.unwrap();
        let next = page.cursor.clone();
        let complete = page.complete;
        all.extend(ids(&typed(page)).into_iter().map(str::to_owned));
        if complete {
            assert!(next.is_none());
            break;
        }
        req.cursor = next;
    }
    assert_eq!(all, ["a", "b", "z"]);
    let denied_tail = service(make_graph(&["a", "denied"]), ReadLimits::default());
    assert!(
        denied_tail
            .read(context(), req_with_limit(1))
            .await
            .unwrap()
            .complete
    );
}
fn req_with_limit(n: usize) -> ReadRequest {
    let mut r = request(ReadQuery::Filter("*".into()));
    r.page_size = n;
    r
}

#[tokio::test]
async fn h4_is_actual_codec_output_and_rich_projection_is_explicit() {
    let graph = make_graph(&["a"]);
    let mut changes = HDict::new();
    changes.set("null", Kind::Null);
    changes.set("number", Kind::Number(Number::unitless(-0.0)));
    graph.update("a", changes).unwrap();
    let svc = service(graph.clone(), ReadLimits::default());
    for codec in [H4Codec::Zinc, H4Codec::Json, H4Codec::JsonV3] {
        let req = ReadRequest::new(ReadQuery::Filter("*".into()), OutputProfile::H4(codec));
        let output = svc.read(context(), req).await.unwrap();
        let ReadOutput::H4 {
            body,
            codec: observed,
        } = output.output
        else {
            panic!("H4 profile");
        };
        assert_eq!(observed, codec);
        let decoded = codec_for(codec.mime())
            .unwrap()
            .decode_grid(std::str::from_utf8(&body).unwrap())
            .unwrap();
        assert_eq!(ids(&decoded), ["a"]);
        assert_eq!(decoded.meta.get("complete"), Some(&Kind::Bool(true)));
        if codec == H4Codec::Zinc {
            assert!(decoded.rows[0].missing("null"));
        }
    }
    let mut changes = HDict::new();
    changes.set("integer", Kind::Int(42));
    graph.update("a", changes).unwrap();
    assert_eq!(
        svc.read(
            context(),
            ReadRequest::new(
                ReadQuery::Filter("*".into()),
                OutputProfile::H4(H4Codec::Zinc)
            )
        )
        .await
        .unwrap_err(),
        ReadError::Projection
    );
    assert_eq!(
        typed(
            svc.read(context(), request(ReadQuery::Filter("*".into())))
                .await
                .unwrap()
        )
        .rows[0]
            .get("integer"),
        Some(&Kind::Int(42))
    );
    assert_eq!(
        svc.read(
            context(),
            ReadRequest::new(
                ReadQuery::Filter("*".into()),
                OutputProfile::H4(H4Codec::Trio)
            )
        )
        .await
        .unwrap_err(),
        ReadError::InvalidQuery("codec cannot carry page metadata")
    );
}

fn slot(name: &str, marker: bool, query: bool, meta: &[(&str, &str)]) -> Slot {
    Slot {
        name: name.into(),
        type_ref: None,
        meta: meta
            .iter()
            .map(|(k, v)| (k.to_string(), Kind::Str(v.to_string())))
            .collect(),
        default: None,
        is_marker: marker,
        is_query: query,
        children: vec![],
    }
}
fn query_graph(denied_count: usize, cycle: bool) -> SharedGraph {
    let mut ns = DefNamespace::new();
    let mut source = Spec::new("demo::Source", "demo", "Source");
    source.slots = vec![
        slot("source", true, false, &[]),
        slot("link", false, true, &[("via", "parentRef+")]),
    ];
    // Optional query slots must still propagate an exhausted traversal budget.
    source.slots[1].meta.insert("maybe".into(), Kind::Marker);
    ns.register_spec(source);
    let mut root = Spec::new("demo::Root", "demo", "Root");
    root.slots = vec![
        slot("root", true, false, &[]),
        slot(
            "children",
            false,
            true,
            &[("inverse", "demo::Source.link"), ("of", "Source")],
        ),
    ];
    ns.register_spec(root);
    let graph = SharedGraph::new(EntityGraph::with_namespace(ns));
    let mut root = row("root");
    root.set("root", Kind::Marker);
    if cycle {
        root.set("parentRef", Kind::Ref(HRef::from_val("source")));
    }
    graph.add(root).unwrap();
    let mut source = row("source");
    source.set("source", Kind::Marker);
    source.set("parentRef", Kind::Ref(HRef::from_val("root")));
    graph.add(source).unwrap();
    for i in 0..denied_count {
        let mut row = row(&format!("d-{i}"));
        row.set("parentRef", Kind::Ref(HRef::from_val("root")));
        graph.add(row).unwrap();
    }
    graph
}
#[tokio::test]
async fn inverse_work_counts_denied_edges_and_transitive_cycles_exactly() {
    for (denied, cycle, edges) in [(9, false, 10), (0, true, 2)] {
        let graph = query_graph(denied, cycle);
        let enough = service(
            graph.clone(),
            ReadLimits {
                max_inverse_edges: edges,
                ..ReadLimits::default()
            },
        );
        assert_eq!(
            ids(&typed(
                enough
                    .read(context(), request(ReadQuery::Filter("demo::Root".into())))
                    .await
                    .unwrap()
            )),
            ["root"]
        );
        let short = service(
            graph,
            ReadLimits {
                max_inverse_edges: edges - 1,
                ..ReadLimits::default()
            },
        );
        assert_eq!(
            short
                .read(context(), request(ReadQuery::Filter("demo::Root".into())))
                .await
                .unwrap_err(),
            ReadError::Budget(BudgetKind::Inverse)
        );
    }
}
#[tokio::test]
async fn exhaustion_is_not_missing_or_optional_query_success() {
    let graph = query_graph(0, false);
    let svc = service(
        graph,
        ReadLimits {
            max_forward_edges: 0,
            ..ReadLimits::default()
        },
    );
    assert_eq!(
        svc.read(
            context(),
            request(ReadQuery::Filter("not parentRef->site or source".into()))
        )
        .await
        .unwrap_err(),
        ReadError::Budget(BudgetKind::Forward)
    );
    assert_eq!(
        svc.read(context(), request(ReadQuery::Filter("demo::Source".into())))
            .await
            .unwrap_err(),
        ReadError::Budget(BudgetKind::Forward)
    );
}

#[tokio::test]
async fn catalog_hidden_unknown_and_budgets_have_consistent_outcomes() {
    let mut ns = DefNamespace::new();
    ns.register_spec(Spec::new("demo::Visible", "demo", "Visible"));
    ns.register_spec(Spec::new("hidden::Secret", "hidden", "Secret"));
    let graph = SharedGraph::new(EntityGraph::with_namespace(ns));
    graph.add(row("a")).unwrap();
    let svc = service(graph.clone(), ReadLimits::default());
    let mut errors = Vec::new();
    for name in ["hidden::Secret", "missing::Secret"] {
        errors.push(
            svc.read(context(), request(ReadQuery::Filter(name.into())))
                .await
                .unwrap_err(),
        );
    }
    assert_eq!(errors[0], errors[1]);
    let a = svc
        .read(context(), request(ReadQuery::Spec("hidden::Secret".into())))
        .await
        .unwrap_err();
    let b = svc
        .read(
            context(),
            request(ReadQuery::Spec("missing::Secret".into())),
        )
        .await
        .unwrap_err();
    assert_eq!(a, b);
    let visible = typed(
        svc.read(context(), request(ReadQuery::Specs { library: None }))
            .await
            .unwrap(),
    );
    assert_eq!(visible.rows.len(), 1);
    assert_eq!(
        visible.rows[0].get("qname"),
        Some(&Kind::Str("demo::Visible".into()))
    );
    let bounded = service(
        graph,
        ReadLimits {
            max_candidates: 1,
            ..ReadLimits::default()
        },
    );
    assert_eq!(
        bounded
            .read(context(), request(ReadQuery::Specs { library: None }))
            .await
            .unwrap_err(),
        ReadError::Budget(BudgetKind::Candidates)
    );
}

#[tokio::test]
async fn oversized_values_filters_and_unused_request_metadata_fail_boundedly() {
    let graph = make_graph(&["a"]);
    let mut changes = HDict::new();
    changes.set("huge", Kind::Str("x".repeat(200_000)));
    graph.update("a", changes).unwrap();
    let svc = service(
        graph,
        ReadLimits {
            max_retained_bytes: 100_000,
            ..ReadLimits::default()
        },
    );
    assert!(matches!(
        svc.read(context(), request(ReadQuery::Filter("*".into())))
            .await,
        Err(ReadError::Budget(_))
    ));
    let svc = service(
        make_graph(&["a"]),
        ReadLimits {
            max_ast_nodes: 4,
            ..ReadLimits::default()
        },
    );
    assert_eq!(
        svc.read(
            context(),
            request(ReadQuery::Filter("a and b and c".into()))
        )
        .await
        .unwrap_err(),
        ReadError::Budget(BudgetKind::Ast)
    );
    let body = format!(
        "ver:\"3.0\" unused:{}N{}\nfilter\n\"site\"\n",
        "[".repeat(70),
        "]".repeat(70)
    );
    assert_eq!(
        svc.read_wire(
            context(),
            ReadOperation::Read,
            body.into_bytes(),
            H4Codec::Zinc,
            H4Codec::Zinc
        )
        .await
        .unwrap_err(),
        ReadError::InvalidQuery("invalid request grid")
    );
}

async fn wait_load(svc: &ReadService, wanted: ReadLoad) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while svc.load() != wanted {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn admission_covers_body_phase_queue_cancel_deadline_and_recovery() {
    let svc = service(
        make_graph(&["a"]),
        ReadLimits {
            max_concurrent: 1,
            max_queued: 1,
            ..ReadLimits::default()
        },
    );
    let cancelled_before = context();
    cancelled_before.cancellation.cancel();
    assert_eq!(
        svc.read(cancelled_before, req_with_limit(1))
            .await
            .unwrap_err(),
        ReadError::Cancelled
    );
    let held = svc.begin(context()).await.unwrap();
    assert_eq!(svc.load().admitted, 1);
    let mut cancelled = context();
    let token = cancelled.cancellation.clone();
    cancelled.deadline = Instant::now() + Duration::from_secs(2);
    let waiting = {
        let svc = svc.clone();
        tokio::spawn(async move {
            svc.read(cancelled, request(ReadQuery::Filter("*".into())))
                .await
        })
    };
    wait_load(
        &svc,
        ReadLoad {
            admitted: 1,
            waiting: 1,
        },
    )
    .await;
    assert_eq!(
        svc.read(context(), request(ReadQuery::Filter("*".into())))
            .await
            .unwrap_err(),
        ReadError::Capacity
    );
    token.cancel();
    assert_eq!(waiting.await.unwrap().unwrap_err(), ReadError::Cancelled);
    drop(held);
    wait_load(
        &svc,
        ReadLoad {
            admitted: 0,
            waiting: 0,
        },
    )
    .await;
    let expired = ReadContext::new(
        Principal::Anonymous,
        Instant::now(),
        CancellationToken::new(),
    );
    assert_eq!(
        svc.read(expired, req_with_limit(1)).await.unwrap_err(),
        ReadError::Deadline
    );
    let admission = svc.begin(context()).await.unwrap();
    admission.cancellation().cancel();
    assert_eq!(
        admission.read(req_with_limit(1)).await.unwrap_err(),
        ReadError::Cancelled
    );
    assert_eq!(
        svc.read(context(), req_with_limit(1))
            .await
            .unwrap()
            .row_count,
        1
    );
}

struct PausePolicy {
    entered: std::sync::mpsc::Sender<()>,
    release: Arc<Barrier>,
    calls: AtomicUsize,
    panic_first: bool,
}
impl ReadPolicy for PausePolicy {
    fn snapshot(&self, _: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            if self.panic_first {
                panic!("controlled worker panic");
            }
            self.entered.send(()).unwrap();
            self.release.wait();
        }
        Ok(Arc::new(AllowAll))
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropped_caller_retains_worker_permit_until_real_exit_and_panic_recovers() {
    let (tx, rx) = std::sync::mpsc::channel();
    let release = Arc::new(Barrier::new(2));
    let svc = ReadService::new(
        make_graph(&["a"]),
        Arc::new(PausePolicy {
            entered: tx,
            release: release.clone(),
            calls: AtomicUsize::new(0),
            panic_first: false,
        }),
        ReadLimits {
            max_concurrent: 1,
            max_queued: 0,
            ..ReadLimits::default()
        },
    )
    .unwrap();
    let task = {
        let svc = svc.clone();
        tokio::spawn(async move { svc.read(context(), req_with_limit(1)).await })
    };
    rx.recv_timeout(Duration::from_secs(2)).unwrap();
    task.abort();
    let _ = task.await;
    assert_eq!(svc.load().admitted, 1);
    assert_eq!(
        svc.read(context(), req_with_limit(1)).await.unwrap_err(),
        ReadError::Capacity
    );
    release.wait();
    wait_load(
        &svc,
        ReadLoad {
            admitted: 0,
            waiting: 0,
        },
    )
    .await;
    assert_eq!(
        svc.read(context(), req_with_limit(1))
            .await
            .unwrap()
            .row_count,
        1
    );
    let (tx, _rx) = std::sync::mpsc::channel();
    let panics = ReadService::new(
        make_graph(&["a"]),
        Arc::new(PausePolicy {
            entered: tx,
            release: Arc::new(Barrier::new(1)),
            calls: AtomicUsize::new(0),
            panic_first: true,
        }),
        ReadLimits::default(),
    )
    .unwrap();
    assert_eq!(
        panics.read(context(), req_with_limit(1)).await.unwrap_err(),
        ReadError::Unavailable
    );
    assert_eq!(panics.load().admitted, 0);
    assert_eq!(
        panics
            .read(context(), req_with_limit(1))
            .await
            .unwrap()
            .row_count,
        1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lock_wait_is_in_deadline_and_does_not_hold_admission_after_exit() {
    let graph = make_graph(&["a"]);
    let svc = service(graph.clone(), ReadLimits::default());
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let writer = {
        let entered = entered.clone();
        let release = release.clone();
        std::thread::spawn(move || {
            graph.write(|_| {
                entered.wait();
                release.wait();
            })
        })
    };
    entered.wait();
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        svc.read(
            ReadContext::with_timeout(Principal::Anonymous, Duration::from_millis(20)),
            req_with_limit(1),
        ),
    )
    .await;
    release.wait();
    writer.join().unwrap();
    assert_eq!(result.unwrap().unwrap_err(), ReadError::Deadline);
    // The response is prompt; worker release is a separate completion event.
    wait_load(
        &svc,
        ReadLoad {
            admitted: 0,
            waiting: 0,
        },
    )
    .await;
}

struct PauseEntity {
    entered: std::sync::mpsc::Sender<()>,
    release: Arc<Barrier>,
    calls: AtomicUsize,
}
struct EntityPolicy(Arc<PauseEntity>);
impl ReadPolicy for EntityPolicy {
    fn snapshot(&self, _: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
        Ok(self.0.clone())
    }
}
impl PolicySnapshot for PauseEntity {
    fn scope_key(&self) -> &str {
        "snapshot-fixture"
    }
    fn operation(&self, _: ReadOperation) -> bool {
        true
    }
    fn entity(&self, _: &str) -> bool {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            self.entered.send(()).unwrap();
            self.release.wait();
        }
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
        true
    }
    fn nominal_provenance(&self, _: &NominalScalar) -> bool {
        true
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn catalog_publication_cannot_split_a_read_snapshot_and_invalidates_continuation() {
    let namespace = |marker: &str| {
        let mut ns = DefNamespace::new();
        let mut spec = Spec::new("demo::Thing", "demo", "Thing");
        spec.slots.push(slot(marker, true, false, &[]));
        ns.register_spec(spec);
        ns
    };
    let graph = SharedGraph::new(EntityGraph::with_namespace(namespace("site")));
    graph.add(row("a")).unwrap();
    graph.add(row("b")).unwrap();
    let (entered, rx) = std::sync::mpsc::channel();
    let release = Arc::new(Barrier::new(2));
    let svc = ReadService::new(
        graph.clone(),
        Arc::new(EntityPolicy(Arc::new(PauseEntity {
            entered,
            release: release.clone(),
            calls: AtomicUsize::new(0),
        }))),
        ReadLimits::default(),
    )
    .unwrap();
    let mut query = request(ReadQuery::Filter("demo::Thing".into()));
    query.page_size = 1;
    let pending = {
        let svc = svc.clone();
        let query = query.clone();
        tokio::spawn(async move { svc.read(context(), query).await })
    };
    rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let (intent_tx, intent_rx) = std::sync::mpsc::channel();
    let writer = {
        let graph = graph.clone();
        let next = namespace("equip");
        std::thread::spawn(move || {
            intent_tx.send(()).unwrap();
            graph.set_namespace(next);
        })
    };
    intent_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    release.wait();
    let first = pending.await.unwrap().unwrap();
    assert!(!first.complete);
    assert_eq!(first.row_count, 1);
    query.cursor = first.cursor.clone();
    assert_eq!(ids(&typed(first)), ["a"]);
    writer.join().unwrap();
    assert_eq!(
        svc.read(context(), query.clone()).await.unwrap_err(),
        ReadError::StaleCursor
    );
    query.cursor = None;
    let second = svc.read(context(), query).await.unwrap();
    assert_eq!(second.row_count, 0);
    assert!(second.complete);
}

#[tokio::test]
async fn cursor_tampering_capacity_and_expiry_are_explicit_and_recover() {
    let svc = service(
        make_graph(&["a", "b", "c"]),
        ReadLimits {
            cursor_capacity: 1,
            cursor_ttl: Duration::from_millis(300),
            ..ReadLimits::default()
        },
    );
    let mut req = req_with_limit(1);
    let page = svc.read(context(), req.clone()).await.unwrap();
    let original = page.cursor.unwrap();
    let mut forged = original.clone().into_bytes();
    forged[0] = if forged[0] == b'A' { b'B' } else { b'A' };
    req.cursor = Some(String::from_utf8(forged).unwrap());
    assert_eq!(
        svc.read(context(), req.clone()).await.unwrap_err(),
        ReadError::StaleCursor
    );
    req.cursor = None;
    assert_eq!(
        svc.read(context(), req.clone()).await.unwrap_err(),
        ReadError::Capacity
    );
    tokio::time::sleep(Duration::from_millis(350)).await;
    req.cursor = Some(original);
    assert_eq!(
        svc.read(context(), req.clone()).await.unwrap_err(),
        ReadError::StaleCursor
    );
    req.cursor = None;
    assert!(svc.read(context(), req).await.unwrap().cursor.is_some());
}

fn inherited_query_graph(base_present: bool) -> SharedGraph {
    let mut namespace = DefNamespace::new();
    if base_present {
        let mut base = Spec::new("hidden::Base", "hidden", "Base");
        base.slots.push(slot("base", true, false, &[]));
        namespace.register_spec(base);
    }
    let mut child = Spec::new("demo::Child", "demo", "Child");
    child.base = Some("hidden::Base".into());
    child.slots.push(slot("child", true, false, &[]));
    namespace.register_spec(child);
    let mut parent = Spec::new("demo::Parent", "demo", "Parent");
    parent.slots = vec![
        slot("parent", true, false, &[]),
        slot(
            "children",
            false,
            true,
            &[("via", "childRef"), ("of", "Child")],
        ),
    ];
    namespace.register_spec(parent);
    let graph = SharedGraph::new(EntityGraph::with_namespace(namespace));
    let mut child = row("child");
    child.set("child", Kind::Marker);
    child.set("base", Kind::Marker);
    graph.add(child).unwrap();
    let mut parent = row("parent");
    parent.set("parent", Kind::Marker);
    parent.set("childRef", Kind::Ref(HRef::from_val("child")));
    graph.add(parent).unwrap();
    graph
}

#[tokio::test]
async fn absent_base_stops_inheritance_in_pure_and_controlled_direct_and_of_fitting() {
    let graph = inherited_query_graph(false);
    let svc = ReadService::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default()).unwrap();
    let mut comparisons = Vec::new();
    for (filter, expected) in [("demo::Child", "child"), ("demo::Parent", "parent")] {
        let pure = graph.read(|graph| {
            graph
                .read_all(filter, 0)
                .unwrap()
                .iter()
                .map(|row| row.id().unwrap().val.clone())
                .collect::<Vec<_>>()
        });
        assert_eq!(pure, [expected]);
        let controlled = typed(
            svc.read(context(), request(ReadQuery::Filter(filter.into())))
                .await
                .unwrap(),
        );
        comparisons.push((
            filter,
            pure,
            ids(&controlled)
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>(),
        ));
    }
    assert!(
        comparisons
            .iter()
            .all(|(_, pure, controlled)| pure == controlled),
        "{comparisons:?}"
    );
}

#[tokio::test]
async fn present_but_denied_base_is_not_treated_as_an_absent_base() {
    let graph = inherited_query_graph(true);
    let unrestricted =
        ReadService::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default()).unwrap();
    let scoped = service(graph.clone(), ReadLimits::default());
    for (filter, expected) in [("demo::Child", "child"), ("demo::Parent", "parent")] {
        let pure = graph.read(|graph| {
            graph
                .read_all(filter, 0)
                .unwrap()
                .iter()
                .map(|row| row.id().unwrap().val.clone())
                .collect::<Vec<_>>()
        });
        assert_eq!(pure, [expected]);
        let controlled = typed(
            unrestricted
                .read(context(), request(ReadQuery::Filter(filter.into())))
                .await
                .unwrap(),
        );
        assert_eq!(ids(&controlled), [expected]);
        assert!(
            typed(
                scoped
                    .read(context(), request(ReadQuery::Filter(filter.into())))
                    .await
                    .unwrap()
            )
            .is_empty()
        );
    }
}

fn queued_worker_stop(deadline: bool) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let blocker = runtime.spawn_blocking(move || {
        started_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    });
    started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let svc = service(
        make_graph(&["a"]),
        ReadLimits {
            max_concurrent: 1,
            max_queued: 0,
            ..ReadLimits::default()
        },
    );
    runtime.block_on(async {
        let ctx = ReadContext::with_timeout(
            Principal::Anonymous,
            if deadline {
                Duration::from_millis(30)
            } else {
                Duration::from_secs(5)
            },
        );
        let token = ctx.cancellation.clone();
        let mut read = {
            let svc = svc.clone();
            tokio::spawn(async move { svc.read(ctx, req_with_limit(1)).await })
        };
        wait_load(
            &svc,
            ReadLoad {
                admitted: 1,
                waiting: 0,
            },
        )
        .await;
        if !deadline {
            token.cancel();
        }
        let prompt = tokio::time::timeout(Duration::from_millis(500), &mut read).await;
        let held_before_release = svc.load();
        let capacity = svc.read(context(), req_with_limit(1)).await.unwrap_err();
        // Always release/join the unrelated blocker before asserting the timeout,
        // so a red regression cannot strand a blocking runtime during teardown.
        release_tx.send(()).unwrap();
        blocker.await.unwrap();
        let (returned_promptly, outcome) = match prompt {
            Ok(outcome) => (true, outcome.unwrap()),
            Err(_) => (false, read.await.unwrap()),
        };
        wait_load(
            &svc,
            ReadLoad {
                admitted: 0,
                waiting: 0,
            },
        )
        .await;
        assert_eq!(
            held_before_release,
            ReadLoad {
                admitted: 1,
                waiting: 0
            }
        );
        assert_eq!(capacity, ReadError::Capacity);
        assert!(
            returned_promptly,
            "request stop waited for unrelated blocking work"
        );
        assert_eq!(
            outcome.unwrap_err(),
            if deadline {
                ReadError::Deadline
            } else {
                ReadError::Cancelled
            }
        );
        assert_eq!(
            svc.read(context(), req_with_limit(1))
                .await
                .unwrap()
                .row_count,
            1
        );
    });
}
#[test]
fn queued_blocking_worker_deadline_returns_before_unrelated_blocker_releases() {
    queued_worker_stop(true);
}
#[test]
fn queued_blocking_worker_cancellation_returns_before_unrelated_blocker_releases() {
    queued_worker_stop(false);
}

#[tokio::test]
async fn catalog_regex_parser_allocation_is_reserved_before_compilation() {
    for (pattern, expected) in [
        ("^ok$".to_string(), None),
        (
            ".".repeat(32_768),
            Some(ReadError::Budget(BudgetKind::Retained)),
        ),
    ] {
        let mut namespace = DefNamespace::new();
        let mut spec = Spec::new("demo::Pattern", "demo", "Pattern");
        spec.slots
            .push(slot("text", false, false, &[("pattern", &pattern)]));
        namespace.register_spec(spec);
        let graph = SharedGraph::new(EntityGraph::with_namespace(namespace));
        let mut record = row("a");
        record.set("text", Kind::Str("ok".into()));
        graph.add(record).unwrap();
        let svc = service(
            graph,
            ReadLimits {
                max_regex_bytes: 4096,
                max_retained_bytes: 128 * 1024,
                ..ReadLimits::default()
            },
        );
        let result = svc
            .read(
                context(),
                request(ReadQuery::Filter("demo::Pattern".into())),
            )
            .await;
        match expected {
            Some(expected) => assert_eq!(result.unwrap_err(), expected),
            None => assert_eq!(result.unwrap().row_count, 1),
        }
    }
}

#[test]
fn dropping_caller_while_worker_is_queued_keeps_admission_until_actual_release() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let blocker = runtime.spawn_blocking(move || {
        started_tx.send(()).unwrap();
        let _ = release_rx.recv();
    });
    started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let svc = service(
        make_graph(&["a"]),
        ReadLimits {
            max_concurrent: 1,
            max_queued: 0,
            ..ReadLimits::default()
        },
    );
    runtime.block_on(async {
        let read = {
            let svc = svc.clone();
            tokio::spawn(async move { svc.read(context(), req_with_limit(1)).await })
        };
        wait_load(
            &svc,
            ReadLoad {
                admitted: 1,
                waiting: 0,
            },
        )
        .await;
        read.abort();
        let cancelled = read.await.unwrap_err().is_cancelled();
        let before_release = svc.load();
        let capacity = svc.read(context(), req_with_limit(1)).await.unwrap_err();
        release_tx.send(()).unwrap();
        blocker.await.unwrap();
        wait_load(
            &svc,
            ReadLoad {
                admitted: 0,
                waiting: 0,
            },
        )
        .await;
        assert!(cancelled);
        assert_eq!(
            before_release,
            ReadLoad {
                admitted: 1,
                waiting: 0
            }
        );
        assert_eq!(capacity, ReadError::Capacity);
        assert_eq!(
            svc.read(context(), req_with_limit(1))
                .await
                .unwrap()
                .row_count,
            1
        );
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn running_worker_cancellation_returns_before_worker_releases_admission() {
    let (entered, rx) = std::sync::mpsc::channel();
    let release = Arc::new(Barrier::new(2));
    let svc = ReadService::new(
        make_graph(&["a"]),
        Arc::new(PausePolicy {
            entered,
            release: release.clone(),
            calls: AtomicUsize::new(0),
            panic_first: false,
        }),
        ReadLimits {
            max_concurrent: 1,
            max_queued: 0,
            ..ReadLimits::default()
        },
    )
    .unwrap();
    let ctx = context();
    let token = ctx.cancellation.clone();
    let mut task = {
        let svc = svc.clone();
        tokio::spawn(async move { svc.read(ctx, req_with_limit(1)).await })
    };
    rx.recv_timeout(Duration::from_secs(2)).unwrap();
    token.cancel();
    let prompt = tokio::time::timeout(Duration::from_millis(500), &mut task).await;
    let before_release = svc.load();
    release.wait();
    let (timely, result) = match prompt {
        Ok(result) => (true, result.unwrap()),
        Err(_) => (false, task.await.unwrap()),
    };
    wait_load(
        &svc,
        ReadLoad {
            admitted: 0,
            waiting: 0,
        },
    )
    .await;
    assert!(
        timely,
        "cancellation response is separate from worker completion"
    );
    assert_eq!(
        before_release,
        ReadLoad {
            admitted: 1,
            waiting: 0
        }
    );
    assert_eq!(result.unwrap_err(), ReadError::Cancelled);
    assert_eq!(
        svc.read(context(), req_with_limit(1))
            .await
            .unwrap()
            .row_count,
        1
    );
}

#[tokio::test]
async fn regex_source_ceiling_and_unicode_reservations_preserve_bounded_patterns() {
    for (pattern, text, limits, expected) in [
        (
            ".".repeat(500_000),
            "x",
            ReadLimits {
                max_regex_source_bytes: 500_000,
                max_retained_bytes: 1024 * 1024,
                ..ReadLimits::default()
            },
            Some(ReadError::Budget(BudgetKind::Retained)),
        ),
        (
            ".".repeat(500_000),
            "x",
            ReadLimits::default(),
            Some(ReadError::Budget(BudgetKind::Regex)),
        ),
        (r"^\p{Han}+$".into(), "工程", ReadLimits::default(), None),
        (r"(?i)^[a-z]+$".into(), "ABcd", ReadLimits::default(), None),
        (r"^\w+$".into(), "工程", ReadLimits::default(), None),
    ] {
        let mut namespace = DefNamespace::new();
        let mut spec = Spec::new("demo::Pattern", "demo", "Pattern");
        spec.slots
            .push(slot("text", false, false, &[("pattern", &pattern)]));
        namespace.register_spec(spec);
        let graph = SharedGraph::new(EntityGraph::with_namespace(namespace));
        let mut record = row("a");
        record.set("text", Kind::Str(text.into()));
        graph.add(record).unwrap();
        let svc = service(graph, limits);
        let result = svc
            .read(
                context(),
                request(ReadQuery::Filter("demo::Pattern".into())),
            )
            .await;
        match expected {
            Some(expected) => assert_eq!(result.unwrap_err(), expected),
            None => assert_eq!(result.unwrap().row_count, 1),
        }
    }
}

use haystack_app::*;
use haystack_core::{
    data::HDict,
    graph::{EntityGraph, SharedGraph},
    kinds::{Float, HRef, Kind, NominalScalar, Number, Symbol},
    ontology::DefNamespace,
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

const REPOSITORY: &str = "https://github.com/Project-Haystack/xeto";
const REVISION: &str = "873b922451d3ef4c0c9c08ef3daa542f352d69f3";
fn context() -> ReadContext {
    ReadContext::with_timeout(Principal::Anonymous, Duration::from_secs(5))
}
fn request() -> ReadRequest {
    ReadRequest::new(ReadQuery::Filter("*".into()), OutputProfile::Jeto)
}
fn row(id: &str) -> HDict {
    let mut row = HDict::new();
    row.set("id", Kind::Ref(HRef::from_val(id)));
    row
}
fn setup(rows: Vec<HDict>, limits: ReadLimits) -> (ReadService, SharedGraph) {
    let graph = SharedGraph::new(EntityGraph::new());
    for row in rows {
        graph.add(row).unwrap();
    }
    let service = ReadService::new(graph.clone(), Arc::new(AllowAll), limits).unwrap();
    (service, graph)
}
fn wire(page: ReadPage) -> (Value, ReadProvenance) {
    let ReadOutput::Jeto { body, provenance } = page.output else {
        panic!("Jeto output")
    };
    (serde_json::from_slice(&body).unwrap(), provenance)
}

#[tokio::test]
async fn exact_boxed_scalars_and_provenance_share_the_read_cut() {
    let mut record = row("a");
    for (name, value) in [
        ("large", Kind::Int(9_007_199_254_740_993)),
        ("min", Kind::Int(i64::MIN)),
        ("max", Kind::Int(i64::MAX)),
        ("float", Kind::Float(Float::new(-0.0))),
        ("number", Kind::Number(Number::new(42.0, Some("kW".into())))),
        ("buf", Kind::Buf(vec![0, 255, 251])),
        (
            "nominal",
            Kind::Nominal(
                NominalScalar::new("sys::Version", REPOSITORY, REVISION, "1.2.3").unwrap(),
            ),
        ),
        ("none", Kind::None),
        ("na", Kind::NA),
        ("marker", Kind::Marker),
        (
            "list",
            Kind::List(vec![Kind::Null, Kind::None, Kind::NA, Kind::Marker]),
        ),
    ] {
        record.set(name, value);
    }
    let (service, graph) = setup(vec![record], ReadLimits::default());
    let selection = graph.read(|graph| {
        graph
            .activated_catalog()
            .unwrap()
            .selection_identity()
            .to_owned()
    });
    let (document, provenance) = wire(service.read(context(), request()).await.unwrap());
    assert_eq!(provenance.repository, REPOSITORY);
    assert_eq!(provenance.revision, REVISION);
    assert_eq!(provenance.selection, selection);
    assert_eq!(document["spec"], "sys::Grid");
    let row = &document["rows"][0];
    assert_eq!(
        row["large"],
        json!({"spec":"sys::Int","val":"9007199254740993"})
    );
    assert_eq!(row["min"]["val"], "-9223372036854775808");
    assert_eq!(row["max"]["val"], "9223372036854775807");
    assert_eq!(row["float"], json!({"spec":"sys::Float","val":"-0.0"}));
    assert_eq!(row["number"], json!({"spec":"sys::Number","val":"42.0kW"}));
    assert_eq!(row["buf"], json!({"spec":"sys::Buf","val":"AP_7"}));
    assert_eq!(row["nominal"], json!({"spec":"sys::Version","val":"1.2.3"}));
    assert_eq!(row["none"]["spec"], "sys::None");
    assert_eq!(row["na"]["spec"], "sys::NA");
    assert_eq!(row["marker"]["spec"], "sys::Marker");
    assert!(row["list"][0].is_null());
    assert!(!row.as_object().unwrap().contains_key("missing"));
    let mut basic = request();
    basic.profile = OutputProfile::H4(H4Codec::Json);
    assert_eq!(
        service.read(context(), basic).await.unwrap_err(),
        ReadError::Projection
    );
}

#[tokio::test]
async fn exact_output_rejects_loss_unsupported_and_foreign_context_without_cursor_publication() {
    for value in [
        Kind::Null,
        Kind::Remove,
        Kind::Symbol(Symbol::new("demo")),
        Kind::Float(Float::from_bits(0x7ff8_0000_0000_0042)),
        Kind::Nominal(NominalScalar::new("unknown::Scalar", REPOSITORY, REVISION, "x").unwrap()),
        Kind::Nominal(NominalScalar::new("sys::Version", "foreign", REVISION, "1.2.3").unwrap()),
        Kind::Nominal(NominalScalar::new("sys::Version", REPOSITORY, "foreign", "1.2.3").unwrap()),
    ] {
        let mut first = row("a");
        first.set("value", value);
        let (service, _) = setup(
            vec![first, row("b")],
            ReadLimits {
                cursor_capacity: 1,
                ..Default::default()
            },
        );
        let mut read = request();
        read.page_size = 1;
        assert_eq!(
            service.read(context(), read.clone()).await.unwrap_err(),
            ReadError::Projection
        );
        // A failed encoder must not consume the sole cursor entry.
        read.projection = vec!["id".into()];
        let page = service.read(context(), read).await.unwrap();
        assert!(!page.complete && page.cursor.is_some());
    }
}

#[tokio::test]
async fn continuations_bind_profile_and_catalog_and_context_never_falls_back() {
    let (service, graph) = setup(vec![row("a"), row("b")], ReadLimits::default());
    let mut read = request();
    read.page_size = 1;
    read.cursor = service.read(context(), read.clone()).await.unwrap().cursor;
    for profile in [OutputProfile::Typed, OutputProfile::H4(H4Codec::Json)] {
        let mut changed = read.clone();
        changed.profile = profile;
        assert_eq!(
            service.read(context(), changed).await.unwrap_err(),
            ReadError::StaleCursor
        );
    }
    graph.set_namespace(DefNamespace::new());
    assert_eq!(
        service.read(context(), read.clone()).await.unwrap_err(),
        ReadError::StaleCursor
    );
    read.cursor = None;
    // Namespace rebasing retains the admitted selection. A replacement graph
    // without that observation must not silently use a standard codec context.
    assert!(service.read(context(), read.clone()).await.is_ok());
    graph.write(|graph| *graph = EntityGraph::new());
    assert_eq!(
        service.read(context(), read).await.unwrap_err(),
        ReadError::Unavailable
    );
}

#[tokio::test]
async fn rich_read_limits_and_interruption_do_not_become_successful_partial_pages() {
    let mut record = row("a");
    record.set("huge", Kind::Buf(vec![0; 4096]));
    let (service, _) = setup(
        vec![record],
        ReadLimits {
            max_output_bytes: 1024,
            ..Default::default()
        },
    );
    assert_eq!(
        service.read(context(), request()).await.unwrap_err(),
        ReadError::Budget(BudgetKind::Output)
    );
    let mut record = row("a");
    let mut value = Kind::Int(1);
    for _ in 0..12 {
        value = Kind::List(vec![value]);
    }
    record.set("nested", value);
    let (service, _) = setup(
        vec![record],
        ReadLimits {
            max_value_depth: 4,
            ..Default::default()
        },
    );
    assert_eq!(
        service.read(context(), request()).await.unwrap_err(),
        ReadError::Budget(BudgetKind::Depth)
    );
    let cancelled = context();
    cancelled.cancellation.cancel();
    assert_eq!(
        service.read(cancelled, request()).await.unwrap_err(),
        ReadError::Cancelled
    );
}

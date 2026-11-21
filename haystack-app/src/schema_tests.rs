//! Boundary tests for the schema request's original admission and disclosure.
use crate::*;
use haystack_core::{
    graph::{EntityGraph, SharedGraph},
    kinds::NominalScalar,
    xeto::catalog::Catalog,
};
use std::{future::Future, task::Poll};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

fn input() -> TypedInvocationInput {
    TypedInvocationInput {
        operation: "rusty-haystack/openapi.json".into(),
        versions: vec!["5".into()],
        ..Default::default()
    }
}
fn context() -> ReadContext {
    ReadContext::with_timeout(Principal::Anonymous, Duration::from_secs(5))
}
fn trusted() -> ReadContext {
    ReadContext::with_timeout(
        Principal::TrustedEmbedding {
            subject: "schema-owner".into(),
        },
        Duration::from_secs(5),
    )
}
struct Policy(Arc<AtomicUsize>);
struct Snapshot(String);
impl ReadPolicy for Policy {
    fn snapshot(&self, _: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
        Ok(Arc::new(Snapshot(
            self.0.load(Ordering::SeqCst).to_string(),
        )))
    }
}
impl PolicySnapshot for Snapshot {
    fn scope_key(&self) -> &str {
        &self.0
    }
    fn function(&self, identity: &FunctionIdentity) -> bool {
        identity.qname != "sys.api::readAll"
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
    fn catalog(&self, _: CatalogKind, name: &str) -> bool {
        !name.starts_with("secret::") && name != "public::Hidden"
    }
    fn nominal_provenance(&self, _: &NominalScalar) -> bool {
        true
    }
}
async fn export(service: &ReadService) -> TypedInvocationResponse {
    service
        .begin(context())
        .await
        .unwrap()
        .invoke_wire(input())
        .await
        .unwrap()
}

#[tokio::test]
async fn schema_visibility_and_determinism_ignore_hidden_catalog_changes() {
    let epoch = Arc::new(AtomicUsize::new(0));
    let service = ReadService::new(
        SharedGraph::new(EntityGraph::new()),
        Arc::new(Policy(epoch.clone())),
        ReadLimits::default(),
    )
    .unwrap();
    let first = export(&service).await;
    first.disclosure.check().unwrap();
    let bytes = String::from_utf8(first.body.clone()).unwrap();
    assert!(!bytes.contains("sys.api::readAll"));
    for absent in [
        "ph.api::hisRead",
        "ph.api::hisWrite",
        "ph.api::pointWrite",
        "ph.api::watchSub",
    ] {
        assert!(!bytes.contains(absent));
    }
    let hidden = Catalog::load_http_pinned()
        .unwrap()
        .with_project(
            "secret",
            r#"pragma: Lib <version:"1.0.0", depends:{{lib:"sys",versions:"5.0.0"}}>
Hidden: sys::Dict { confidential:Str }
"#,
            &[],
        )
        .unwrap();
    service.activate_catalog(trusted(), hidden).await.unwrap();
    let next = export(&service).await;
    assert_eq!(
        first.body, next.body,
        "hidden provenance must not alter a scoped export"
    );
    assert_eq!(first.disclosure.check(), Err(ApiError::Unavailable));
    next.disclosure.check().unwrap();
    epoch.store(1, Ordering::SeqCst);
    assert_eq!(next.disclosure.check(), Err(ApiError::Permission));
    // Scope keys themselves never become document metadata.
    assert_eq!(export(&service).await.body, first.body);
}

#[tokio::test]
async fn visible_type_with_hidden_dependency_is_excluded_without_names_or_fields() {
    let service = ReadService::new(
        SharedGraph::new(EntityGraph::new()),
        Arc::new(Policy(Arc::new(AtomicUsize::new(0)))),
        ReadLimits::default(),
    )
    .unwrap();
    let catalog = Catalog::load_http_pinned()
        .unwrap()
        .with_project(
            "public",
            r#"pragma: Lib <version:"1.0.0", depends:{{lib:"sys",versions:"5.0.0"}}>
Hidden: sys::Dict { confidential:Str }
Visible: sys::Dict { payload:Hidden }
"#,
            &[],
        )
        .unwrap();
    service.activate_catalog(trusted(), catalog).await.unwrap();
    let bytes = export(&service).await.body;
    let text = String::from_utf8(bytes.clone()).unwrap();
    assert!(!text.contains("Hidden") && !text.contains("confidential"));
    let document: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        document["x-rusty-haystack-profile"]["excluded"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["declaration"] == "public::Visible"
                && entry["reason"] == "dependency is not visible")
    );
}

#[tokio::test]
async fn schema_limits_cancellation_and_revocation_are_not_renewed() {
    for limits in [
        ReadLimits {
            max_work: 128,
            ..ReadLimits::default()
        },
        ReadLimits {
            max_retained_bytes: 1024,
            ..ReadLimits::default()
        },
        ReadLimits {
            max_output_bytes: 128,
            ..ReadLimits::default()
        },
        ReadLimits {
            max_value_depth: 1,
            ..ReadLimits::default()
        },
    ] {
        let service = ReadService::new(
            SharedGraph::new(EntityGraph::new()),
            Arc::new(AllowAll),
            limits,
        )
        .unwrap();
        assert!(matches!(
            service
                .begin(context())
                .await
                .unwrap()
                .invoke_wire(input())
                .await,
            Err(ApiError::InvalidArgs)
        ));
        assert_eq!(service.load().admitted, 0);
    }
    let service = ReadService::new(
        SharedGraph::new(EntityGraph::new()),
        Arc::new(AllowAll),
        ReadLimits::default(),
    )
    .unwrap();
    let cancel = CancellationToken::new();
    let admission = service
        .begin(ReadContext::new(
            Principal::Anonymous,
            Instant::now() + Duration::from_secs(3),
            cancel.clone(),
        ))
        .await
        .unwrap();
    let response = admission.invoke_wire(input()).await.unwrap();
    cancel.cancel();
    assert_eq!(response.disclosure.check(), Err(ApiError::Unavailable));
    let principal = Principal::authenticated("schema-user", vec!["read".into()]);
    let session = SubscriptionSession::authenticated(
        principal.clone(),
        Instant::now() + Duration::from_secs(5),
    )
    .unwrap();
    let mut admission = service.begin(context()).await.unwrap();
    admission
        .bind_wire_session(principal, session.clone())
        .unwrap();
    let response = admission.invoke_wire(input()).await.unwrap();
    session.close();
    assert_eq!(response.disclosure.check(), Err(ApiError::Permission));
    assert_eq!(service.load().admitted, 0);
}

#[tokio::test]
async fn schema_retained_response_does_not_keep_owner_alive_and_seal_blocks_disclosure() {
    let builder = ApplicationBuilder::new(
        SharedGraph::new(EntityGraph::new()),
        Arc::new(AllowAll),
        ReadLimits::default(),
    )
    .unwrap();
    let service = builder.handle().read_service();
    let owner = builder.start(&tokio::runtime::Handle::current()).unwrap();
    owner.ready().await.unwrap();
    let response = export(&service).await;
    assert_eq!(service.load().admitted, 0);
    owner.close().await.unwrap();
    owner.terminated().await;
    assert_eq!(response.disclosure.check(), Err(ApiError::Unavailable));
}

#[test]
fn cancelling_schema_encoding_keeps_the_slot_and_owner_registration_until_exit() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let app = ApplicationBuilder::new(
            SharedGraph::new(EntityGraph::new()),
            Arc::new(AllowAll),
            ReadLimits {
                max_concurrent: 1,
                max_queued: 0,
                ..ReadLimits::default()
            },
        )
        .unwrap()
        .shutdown_policy(ShutdownPolicy {
            drain_timeout: Duration::ZERO,
            stop_timeout: Duration::from_secs(1),
        });
        let reads = app.handle().read_service();
        let owner = app.start(&tokio::runtime::Handle::current()).unwrap();
        owner.ready().await.unwrap();
        let cancel = CancellationToken::new();
        let mut admission = reads
            .begin(ReadContext::new(
                Principal::Anonymous,
                std::time::Instant::now() + Duration::from_secs(3),
                cancel.clone(),
            ))
            .await
            .unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release = std::sync::Mutex::new(release_rx);
        admission.budget_mut().typed_encode_hook = Some(Arc::new(move || {
            entered_tx.send(()).unwrap();
            release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(2))
                .unwrap();
        }));
        let mut result = Box::pin(admission.invoke_wire(input()));
        assert!(
            std::future::poll_fn(|cx| Poll::Ready(result.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        cancel.cancel();
        assert!(matches!(result.await, Err(ApiError::Unavailable)));
        assert_eq!(reads.load().admitted, 1);
        let mut closing = Box::pin(owner.close());
        assert!(
            std::future::poll_fn(|cx| Poll::Ready(closing.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        assert_eq!(reads.load().admitted, 1);
        release_tx.send(()).unwrap();
        tokio::task::spawn_blocking(|| ()).await.unwrap();
        closing.await.unwrap();
        owner.terminated().await;
        assert_eq!(reads.load().admitted, 0);
    });
}

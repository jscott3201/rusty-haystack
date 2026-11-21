//! Real immutable repository -> PR06 client -> bounded admission -> owner activation.
use haystack_app::{
    AllowAll, ApplicationBuilder, ApplicationOwner, CatalogActivationLimits, Principal,
    ReadContext, ReadLimits, ReadService,
};
use haystack_client::repository::{
    Integrity, RepoErrorKind, RepositoryClient, RepositoryConfig, RepositoryError,
};
use haystack_core::{
    graph::{EntityGraph, SharedGraph},
    xeto::repository::*,
};
use haystack_server::HaystackServer;
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
const SAMPLE: &[u8] =
    include_bytes!("../../haystack-core/tests/fixtures/repository/sample-deflate.xetolib");
const DEPENDENT: &[u8] =
    include_bytes!("../../haystack-core/tests/fixtures/repository/dependent-store.xetolib");
fn pin(name: &str, version: &str) -> LibraryPin {
    LibraryPin::new(name, version.parse().unwrap()).unwrap()
}
fn repository() -> LibraryRepository {
    LibraryRepository::new(
        "Local fixture",
        vec![
            LibraryPackage::from_bytes(pin("sample", "1.0.0"), SAMPLE.to_vec()).unwrap(),
            LibraryPackage::from_bytes(pin("dependent", "1.0.0"), DEPENDENT.to_vec()).unwrap(),
            LibraryPackage::from_bytes(
                pin("sample", "1.9.0"),
                include_bytes!(
                    "../../haystack-core/tests/fixtures/repository/sample-1.9.0-store.xetolib"
                )
                .to_vec(),
            )
            .unwrap(),
            LibraryPackage::from_bytes(
                pin("sample", "1.10.0"),
                include_bytes!(
                    "../../haystack-core/tests/fixtures/repository/sample-1.10.0-store.xetolib"
                )
                .to_vec(),
            )
            .unwrap(),
        ],
    )
    .unwrap()
}
fn trusted() -> ReadContext {
    ReadContext::with_timeout(
        Principal::TrustedEmbedding {
            subject: "owner".into(),
        },
        Duration::from_secs(5),
    )
}
struct Fixture {
    owner: ApplicationOwner,
    reads: ReadService,
    graph: SharedGraph,
    base: String,
    client: RepositoryClient,
    http: reqwest::Client,
}
impl Fixture {
    async fn start(enabled: bool) -> Self {
        let graph = SharedGraph::new(EntityGraph::new());
        let app = ApplicationBuilder::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default())
            .unwrap();
        let reads = app.handle().read_service();
        let mut server = HaystackServer::new(graph.clone())
            .with_scoped_reads(app.handle())
            .port(0);
        if enabled {
            server = server.with_public_repository(repository());
        }
        let owner = app
            .owned_resource(server.into_listener())
            .start(&tokio::runtime::Handle::current())
            .unwrap();
        let address = owner.ready().await.unwrap().listeners[0].address;
        let base = format!("http://{address}/repo");
        let client = RepositoryClient::with_config(
            &base,
            RepositoryConfig {
                allow_loopback_http: true,
                ..Default::default()
            },
        )
        .unwrap();
        let http = haystack_client::ClientConfig::default()
            .build_reqwest_client()
            .unwrap();
        Self {
            owner,
            reads,
            graph,
            base,
            client,
            http,
        }
    }
}
#[tokio::test]
async fn real_repository_metadata_bytes_dependency_admission_and_atomic_activation() {
    let f = Fixture::start(true).await;
    let cancel = CancellationToken::new();
    assert_eq!(f.client.ping(&cancel).await.unwrap().dis, "Local fixture");
    let all = f.client.search("*", Some(1), &cancel).await.unwrap();
    assert_eq!(all.total, Some(2));
    assert!(all.more);
    assert_eq!(all.libs[0].lib, "dependent");
    let search = f.client.search("sample", None, &cancel).await.unwrap();
    assert_eq!(search.libs[0].latest_version.to_string(), "1.10.0");
    assert_eq!(search.libs[0].latest_maturity, Maturity::Beta);
    assert_eq!(search.libs[0].latest_stable.unwrap().to_string(), "1.9.0");
    assert!(!search.more);
    let no_match = f.client.search("SAMPLE", None, &cancel).await.unwrap();
    assert_eq!(no_match.total, Some(0));
    assert!(no_match.libs.is_empty());
    let versions = f
        .client
        .versions("sample", Some(&"1.x.x".parse().unwrap()), None, &cancel)
        .await
        .unwrap();
    assert_eq!(
        versions
            .iter()
            .map(|v| v.pin.version().to_string())
            .collect::<Vec<_>>(),
        ["1.10.0", "1.9.0", "1.0.0"]
    );
    let limited = f
        .client
        .versions(
            "sample",
            Some(&"1.9.0-1.10.0".parse().unwrap()),
            Some(1),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(limited[0].pin.version().to_string(), "1.10.0");
    assert!(
        f.client
            .versions("unknown", None, None, &cancel)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        f.client
            .versions("sample", Some(&"2.x.x".parse().unwrap()), None, &cancel)
            .await
            .unwrap()
            .is_empty()
    );
    for (name, version, kind) in [
        ("missing", "1.0.0", RepoErrorKind::UnknownLibrary),
        ("sample", "2.0.0", RepoErrorKind::UnknownVersion),
    ] {
        assert!(
            matches!(f.client.fetch(&pin(name,version),None,None,&cancel).await,Err(RepositoryError::Api{status:404,kind:actual,..}) if actual==kind)
        );
    }
    let observation = f.graph.read(|g| g.activated_catalog().cloned().unwrap());
    let state = f.graph.state();
    let advertised = versions
        .iter()
        .find(|v| v.pin.version().to_string() == "1.0.0")
        .unwrap();
    let downloaded = f
        .client
        .fetch(
            &pin("sample", "1.0.0"),
            Some(advertised.into()),
            None,
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(downloaded.integrity, Integrity::Advertised);
    assert_eq!(downloaded.bytes, SAMPLE);
    use sha2::{Digest, Sha256};
    let independent = Sha256::digest(&downloaded.bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let manifest: serde_json::Value = serde_json::from_str(include_str!(
        "../../haystack-core/tests/fixtures/repository/manifest.json"
    ))
    .unwrap();
    assert_eq!(
        independent,
        manifest["sample-deflate.xetolib"]["sha256"]
            .as_str()
            .unwrap()
    );
    let dependent = f
        .client
        .versions("dependent", None, None, &cancel)
        .await
        .unwrap();
    let dependent = f
        .client
        .fetch(
            &pin("dependent", "1.0.0"),
            Some((&dependent[0]).into()),
            None,
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(state, f.graph.state());
    assert!(
        f.graph
            .read(|g| Arc::ptr_eq(g.activated_catalog().unwrap(), &observation))
    );
    let selection = PackageSelection::new(
        vec![dependent.pin.clone()],
        vec![
            LibraryPackage::from_bytes(dependent.pin, dependent.bytes).unwrap(),
            LibraryPackage::from_bytes(downloaded.pin, downloaded.bytes).unwrap(),
        ],
        vec![],
    )
    .unwrap();
    let prepared = f
        .reads
        .prepare_packages(trusted(), selection)
        .await
        .unwrap();
    assert_eq!(state, f.graph.state());
    let mut wakes = f.graph.subscribe_wakes();
    let activation = f
        .reads
        .activate_prepared_packages(trusted(), prepared, CatalogActivationLimits::default())
        .await
        .unwrap();
    assert_eq!(activation.catalog_generation, state.catalog_generation + 1);
    assert!(wakes.try_recv().is_ok());
    assert!(wakes.try_recv().is_err());
    assert!(f.graph.read(|g| {
        g.activated_catalog()
            .unwrap()
            .catalog()
            .declaration("dependent::Child")
            .is_some()
    }));
    assert!(
        observation
            .catalog()
            .declaration("dependent::Child")
            .is_none()
    );
    f.owner.close().await.unwrap();
}
#[tokio::test]
async fn repository_controls_reject_unsupported_semantics_with_typed_errors() {
    let f = Fixture::start(true).await;
    for (path, status) in [
        ("sys.repo::repoPing?ignored=1", 400),
        ("sys.repo::repoSearch?query=*&limit=0", 400),
        ("sys.repo::repoSearch?query=*&limit=65", 400),
        ("sys.repo::repoSearch?query=*&query=x", 400),
        ("sys.repo::repoVersions?lib=sample&versions=latest", 400),
        ("sys.repo::repoFetch?lib=../sample&version=1.0.0", 400),
        ("sys.repo::repoFetch?lib=sample&version=1.0.0&limit=1", 400),
        ("sys.repo::repoPing?xeto-version=4", 400),
        ("sys.repo::repoPing?xeto-filetype=zinc", 406),
        ("sys.repo::repoPing?xeto-unknown=1", 400),
        ("sys.repo::repoSearch?query=%FF", 400),
        ("sys.repo::missing", 404),
        ("repoPing", 404),
        ("sys.repo::repoPublish", 501),
    ] {
        let response = f
            .http
            .get(format!("{}/{path}", f.base))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), status, "{path}");
        assert_eq!(response.headers()["xeto-version"], "5");
        assert_eq!(response.headers()["content-type"], "application/json");
        let body: serde_json::Value =
            serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        assert_eq!(body["status"], status);
        assert!(body["spec"].as_str().unwrap().starts_with("sys.api::"));
    }
    for (header, value, status) in [
        ("Accept", "application/json;box=always", 406),
        ("If-None-Match", "*", 400),
        ("Range", "bytes=0-1", 400),
        ("Xeto-Version", "garbage", 400),
    ] {
        let response = f
            .http
            .get(format!("{}/sys.repo::repoPing", f.base))
            .header(header, value)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), status);
    }
    let response = f
        .http
        .post(format!("{}/sys.repo::repoPing", f.base))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 405);
    assert_eq!(response.headers()["allow"], "GET");
    assert_eq!(
        f.http
            .post(format!("{}/sys.repo::repoPublish", f.base))
            .body("untrusted upload")
            .send()
            .await
            .unwrap()
            .status(),
        501
    );
    let fetched = f
        .http
        .get(format!(
            "{}/sys.repo::repoFetch?lib=sample&version=1.0.0",
            f.base
        ))
        .header("Accept", "text/plain")
        .send()
        .await
        .unwrap();
    assert_eq!(fetched.status(), 200);
    assert_eq!(fetched.bytes().await.unwrap(), SAMPLE);
    let override_format = f
        .http
        .get(format!("{}/sys.repo::repoPing?xeto-filetype=json", f.base))
        .header("Accept", "text/plain")
        .send()
        .await
        .unwrap();
    assert_eq!(override_format.status(), 200);
    f.owner.close().await.unwrap();
}
#[tokio::test]
async fn repository_is_absent_by_default_and_obeys_application_close() {
    let f = Fixture::start(false).await;
    assert_eq!(
        f.http
            .get(format!("{}/sys.repo::repoPing", f.base))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    f.owner.close().await.unwrap();
    let graph = SharedGraph::new(EntityGraph::new());
    let app =
        ApplicationBuilder::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default()).unwrap();
    let handle = app.handle();
    let router = HaystackServer::new(graph)
        .with_scoped_reads(handle)
        .with_public_repository(repository())
        .into_external_router()
        .unwrap();
    let owner = app.start(&tokio::runtime::Handle::current()).unwrap();
    owner.ready().await.unwrap();
    owner.close().await.unwrap();
    let response = router
        .oneshot(
            axum::http::Request::builder()
                .uri("/repo/sys.repo::repoPing")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 503);
    assert_eq!(response.headers()["xeto-version"], "5");
    assert_eq!(response.headers()["content-type"], "application/json");
}

//! Download remains inert until a separately authorized caller admits source and
//! activates the resulting catalog. The ZIP reader below accepts only this one
//! project-owned, stored fixture; it is not a production archive import path.
use haystack_app::{AllowAll, Principal, ReadContext, ReadLimits, ReadService};
use haystack_client::repository::{
    Integrity, LibraryPin, RepositoryClient, RepositoryConfig, RepositoryError,
};
use haystack_core::{
    graph::{EntityGraph, SharedGraph},
    xeto::catalog::Catalog,
};
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;
const ZIP: &[u8] =
    include_bytes!("../../haystack-client/tests/fixtures/repository/downloaded-1.0.0.xetolib");
const DIGEST: &str = "sha256:Qm3aICrF2MHDjamk30dmuQCif1quynjfBEPGq_zMP80";

struct Fixture {
    base: String,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Fixture {
    fn start() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/repo", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let (stop, mut shutdown) = tokio::sync::oneshot::channel();
        let thread = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async move {
                let listener=tokio::net::TcpListener::from_std(listener).unwrap();
                let serve=async {
                    let (mut socket,_)=listener.accept().await.unwrap(); let mut request=Vec::new(); let mut buf=[0;1024];
                    while !request.windows(4).any(|w|w==b"\r\n\r\n") { let n=socket.read(&mut buf).await.unwrap(); assert!(n>0); request.extend_from_slice(&buf[..n]); assert!(request.len()<=8192); }
                    assert!(request.starts_with(b"GET /repo/sys.repo::repoFetch?lib=downloaded&version=1.0.0 HTTP/1.1\r\n"));
                    socket.write_all(format!("HTTP/1.1 200 OK\r\nConnection: close\r\nXeto-Version: 5\r\nContent-Type: application/xetolib\r\nContent-Length: {}\r\n\r\n",ZIP.len()).as_bytes()).await.unwrap();
                    socket.write_all(ZIP).await.unwrap();
                };
                tokio::select! { _=&mut shutdown=>{},_=serve=>{} }
            });
        });
        Self {
            base,
            stop: Some(stop),
            thread: Some(thread),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.thread.take().unwrap().join().unwrap();
    }
}

// Independent test-only extraction of the first known stored ZIP entry. Pin the
// entire archive bytes, entry path, flags, method and both lengths before slicing.
// No writes, decompression, arbitrary entry names or archive traversal are possible.
fn reviewed_fixture_source(bytes: &[u8]) -> &str {
    assert_eq!(bytes, ZIP);
    assert!(bytes.len() < 4096);
    assert_eq!(&bytes[..4], b"PK\x03\x04");
    let short = |at| u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap()) as usize;
    let long = |at| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    assert_eq!(short(6), 0);
    assert_eq!(short(8), 0); // no flags, stored
    assert_eq!(long(18), long(22));
    assert!(long(18) < 1024);
    let name = short(26);
    let extra = short(28);
    assert_eq!(extra, 0);
    assert_eq!(&bytes[30..30 + name], b"lib.xeto");
    std::str::from_utf8(&bytes[30 + name..30 + name + long(18)]).unwrap()
}

#[tokio::test]
async fn fetched_library_requires_separate_admission_and_trusted_activation() {
    let graph = SharedGraph::new(EntityGraph::new());
    let reads = ReadService::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default()).unwrap();
    let before = graph.state();
    let observation = graph.read(|g| g.activated_catalog().cloned().unwrap());
    assert!(
        observation
            .catalog()
            .declaration("downloaded::Downloaded")
            .is_none()
    );
    let fixture = Fixture::start();
    let client = RepositoryClient::with_config(
        &fixture.base,
        RepositoryConfig {
            allow_loopback_http: true,
            ..Default::default()
        },
    )
    .unwrap();
    let pin = LibraryPin::new("downloaded", "1.0.0".parse().unwrap()).unwrap();
    let downloaded = client
        .fetch(
            &pin,
            None,
            Some(DIGEST.parse().unwrap()),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(downloaded.bytes, ZIP);
    assert_eq!(downloaded.integrity, Integrity::Caller);
    assert_eq!(graph.state(), before);
    assert!(graph.read(|g| Arc::ptr_eq(g.activated_catalog().unwrap(), &observation)));
    assert!(
        observation
            .catalog()
            .declaration("downloaded::Downloaded")
            .is_none()
    );
    let failed_fixture = Fixture::start();
    let failed_client = RepositoryClient::with_config(
        &failed_fixture.base,
        RepositoryConfig {
            allow_loopback_http: true,
            ..Default::default()
        },
    )
    .unwrap();
    let wrong = "sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        .parse()
        .unwrap();
    assert_eq!(
        failed_client
            .fetch(&pin, None, Some(wrong), &CancellationToken::new())
            .await
            .unwrap_err(),
        RepositoryError::DigestMismatch
    );
    assert_eq!(graph.state(), before);
    assert!(graph.read(|g| Arc::ptr_eq(g.activated_catalog().unwrap(), &observation)));
    let source = reviewed_fixture_source(&downloaded.bytes);
    assert!(
        Catalog::load_http_pinned()
            .unwrap()
            .with_project("downloaded", "not Xeto source", &[])
            .is_err()
    );
    assert_eq!(graph.state(), before);
    let candidate = Catalog::load_http_pinned()
        .unwrap()
        .with_project("downloaded", source, &[])
        .unwrap();
    assert!(candidate.declaration("downloaded::Downloaded").is_some());
    assert_eq!(graph.state(), before); // Creating the admitted candidate is also inert.
    let published = reads
        .activate_catalog(
            ReadContext::with_timeout(
                Principal::TrustedEmbedding {
                    subject: "fixture owner".into(),
                },
                Duration::from_secs(5),
            ),
            candidate,
        )
        .await
        .unwrap();
    assert_eq!(published.catalog_generation, before.catalog_generation + 1);
    assert_eq!(
        graph.state().catalog_generation,
        published.catalog_generation
    );
    assert!(graph.read(|g| {
        g.activated_catalog()
            .unwrap()
            .catalog()
            .declaration("downloaded::Downloaded")
            .is_some()
    }));
    assert!(graph.read(|g| g.all().is_empty()));
}

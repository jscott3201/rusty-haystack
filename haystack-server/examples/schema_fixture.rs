//! Loopback-only synthetic server for the locked external schema/client check.
//! The fixed test token is public fixture data, never a production credential.
use haystack_app::{
    AllowAll, ApplicationBuilder, ApplicationOwner, Principal, ReadContext, ReadLimits,
};
use haystack_core::{
    data::HDict,
    graph::{EntityGraph, SharedGraph},
    kinds::{HRef, Kind},
    xeto::catalog::Catalog,
};
use haystack_server::HaystackServer;
use std::{sync::Arc, time::Duration};

const ASSET: &str = include_str!("schema-library.xeto");
struct Fixture {
    owner: ApplicationOwner,
    url: String,
}
impl Fixture {
    async fn start() -> Self {
        let graph = SharedGraph::new(EntityGraph::new());
        let builder =
            ApplicationBuilder::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default())
                .unwrap();
        let reads = builder.handle().read_service();
        let auth = haystack_server::auth::AuthManager::from_toml_str(r#"[users.schema]
password_hash = "W22ZaJ0SNY7soEsUEjb6gQ==:4096:WG5d8oPm3OtcPnkdi4Uo7BkeZkBFzpcXkuLmtbsT4qY=:wfPLwcE6nTWhTAmQ7tl2KeoiWGPlZqQxSrmfPwDl2dU="
permissions = ["read"]
"#).unwrap();
        auth.inject_token(
            "schema-fixture".into(),
            haystack_server::auth::AuthUser {
                username: "schema".into(),
                permissions: vec!["read".into()],
            },
        );
        let listener = HaystackServer::new(graph.clone())
            .with_auth(auth)
            .with_scoped_reads(builder.handle())
            .port(0)
            .into_listener();
        let owner = builder
            .owned_resource(listener)
            .start(&tokio::runtime::Handle::current())
            .unwrap();
        let address = owner.ready().await.unwrap().listeners[0].address;
        let catalog = Catalog::load_http_pinned()
            .unwrap()
            .with_project("fixture", ASSET, &[])
            .unwrap();
        reads
            .activate_catalog(
                ReadContext::with_timeout(
                    Principal::TrustedEmbedding {
                        subject: "fixture-owner".into(),
                    },
                    Duration::from_secs(5),
                ),
                catalog.clone(),
            )
            .await
            .unwrap();
        for (id, typed) in [("asset", true), ("dynamic", false)] {
            let mut row = HDict::new();
            row.set("id", Kind::Ref(HRef::from_val(id)));
            row.set("dis", Kind::Str("Alpha".into()));
            if typed {
                row.set("spec", Kind::Ref(HRef::from_val("fixture::Asset")));
            }
            graph.add(row).unwrap();
        }
        let mut item = HDict::new();
        item.set("spec", Kind::Ref(HRef::from_val("fixture::RefinedCode")));
        item.set("code", Kind::Str("ABC".into()));
        let mut container = HDict::new();
        container.set("id", Kind::Ref(HRef::from_val("codes")));
        container.set("spec", Kind::Ref(HRef::from_val("fixture::CodeContainer")));
        container.set("item", Kind::Dict(Box::new(item.clone())));
        container.set("items", Kind::List(vec![Kind::Dict(Box::new(item))]));
        assert!(catalog.fits_selected("fixture::CodeContainer", &container, None));
        for code in [
            "BCD",
            "Abc",
            "ABC\n",
            "ABC\r",
            "ABC\u{2028}",
            "ABC\u{2029}",
            "",
        ] {
            let mut invalid = HDict::new();
            invalid.set("spec", Kind::Ref(HRef::from_val("fixture::RefinedCode")));
            invalid.set("code", Kind::Str(code.into()));
            assert!(!catalog.fits_selected("fixture::RefinedCode", &invalid, None));
        }
        graph.add(container).unwrap();
        Self {
            owner,
            url: format!("http://{address}/api"),
        }
    }
    async fn close(self) {
        self.owner.close().await.unwrap();
        self.owner.terminated().await;
    }
}
#[tokio::main]
async fn main() {
    let fixture = Fixture::start().await;
    println!("{}", serde_json::json!({"url":fixture.url}));
    tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).unwrap();
    })
    .await
    .unwrap();
    fixture.close().await;
}

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

use futures_util::future::join_all;
use haystack_core::data::HDict;
use haystack_core::graph::{EntityGraph, SharedGraph};
use haystack_core::kinds::{HRef, Kind, Number};
#[path = "fixtures/items.rs"]
mod history_items;
#[path = "fixtures/server.rs"]
mod server_fixture;
use server_fixture::TestServer;

/// Build a test graph with 10 sites and `n` points distributed evenly.
fn build_test_graph(n: usize) -> SharedGraph {
    let mut graph = EntityGraph::new();

    // Create 10 sites
    for i in 0..10 {
        let mut d = HDict::new();
        d.set("id", Kind::Ref(HRef::from_val(format!("site-{i}"))));
        d.set("site", Kind::Marker);
        d.set("dis", Kind::Str(format!("Site {i}")));
        d.set(
            "area",
            Kind::Number(Number::new(10000.0, Some("ft\u{00b2}".into()))),
        );
        graph.add(d).unwrap();
    }

    // Create n points, distributed across the 10 sites
    for i in 0..n {
        let site_idx = i % 10;
        let mut d = HDict::new();
        d.set("id", Kind::Ref(HRef::from_val(format!("p-{i}"))));
        d.set("point", Kind::Marker);
        d.set("his", Kind::Marker);
        d.set("sensor", Kind::Marker);
        d.set("temp", Kind::Marker);
        d.set("dis", Kind::Str(format!("Point {i}")));
        d.set(
            "siteRef",
            Kind::Ref(HRef::from_val(format!("site-{site_idx}"))),
        );
        d.set(
            "curVal",
            Kind::Number(Number::new(
                70.0 + (i as f64) * 0.01,
                Some("\u{00b0}F".into()),
            )),
        );
        d.set("kind", Kind::Str("Number".into()));
        d.set("tz", Kind::Str("UTC".into()));
        d.set("unit", Kind::Str("°F".into()));
        graph.add(d).unwrap();
    }

    SharedGraph::new(graph)
}

// ---------------------------------------------------------------------------
// HTTP operation benchmarks
// ---------------------------------------------------------------------------

fn http_benchmarks(c: &mut Criterion) {
    let server = TestServer::start(build_test_graph(1000));
    let rt = server.runtime();
    let client = server.connect_http();

    c.bench_function("http_about", |b| {
        b.iter(|| {
            rt.block_on(async {
                black_box(client.about().await.unwrap());
            });
        });
    });

    c.bench_function("http_read_by_id", |b| {
        b.iter(|| {
            rt.block_on(async {
                black_box(client.read_by_ids(&["p-500"]).await.unwrap());
            });
        });
    });

    c.bench_function("http_read_filter", |b| {
        b.iter(|| {
            rt.block_on(async {
                black_box(
                    client
                        .read("point and siteRef==@site-0", None)
                        .await
                        .unwrap(),
                );
            });
        });
    });

    c.bench_function("http_read_filter_large", |b| {
        b.iter(|| {
            rt.block_on(async {
                black_box(client.read("point", None).await.unwrap());
            });
        });
    });

    c.bench_function("http_nav", |b| {
        b.iter(|| {
            rt.block_on(async {
                black_box(client.nav(None).await.unwrap());
            });
        });
    });
}

// ---------------------------------------------------------------------------
// History benchmarks
// ---------------------------------------------------------------------------

/// Create a vector of history items (dicts with "ts" and "val").
fn make_his_items(count: usize, base_hour: u32) -> Vec<HDict> {
    history_items::history_dicts(count, i64::from(base_hour) * 60)
}

fn his_benchmarks(c: &mut Criterion) {
    let server = TestServer::start(build_test_graph(100));
    let rt = server.runtime();
    let client = server.connect_http();

    // Pre-load 1000 history items for p-0
    let preload_items = make_his_items(1000, 0);
    rt.block_on(async {
        client.his_write("p-0", preload_items).await.unwrap();
    });

    c.bench_function("http_his_read_1000", |b| {
        b.iter(|| {
            rt.block_on(async {
                black_box(client.his_read("p-0", "2024-06-01").await.unwrap());
            });
        });
    });

    c.bench_function("http_his_write_100", |b| {
        b.iter(|| {
            let items = make_his_items(100, 12);
            rt.block_on(async {
                black_box(client.his_write("p-1", items).await.unwrap());
            });
        });
    });
}

// ---------------------------------------------------------------------------
// Watch benchmarks (over HTTP)
// ---------------------------------------------------------------------------

fn watch_benchmarks(c: &mut Criterion) {
    let server = TestServer::start(build_test_graph(100));
    let rt = server.runtime();

    // watch_sub: subscribe to 10 entities via HTTP
    let watch_ids: Vec<String> = (0..10).map(|i| format!("p-{i}")).collect();
    c.bench_function("http_watch_sub", |b| {
        let client = server.connect_http();
        b.iter(|| {
            let id_refs: Vec<&str> = watch_ids.iter().map(|s| s.as_str()).collect();
            rt.block_on(async {
                let grid = client.watch_sub(&id_refs, None).await.unwrap();
                // Close the watch (empty IDs = full unsubscribe) to avoid hitting watch limit
                let Some(Kind::Str(wid)) = grid.meta.get("watchId") else {
                    panic!("watchSub did not return a watchId");
                };
                client.watch_unsub(wid, &[]).await.unwrap();
                black_box(grid);
            });
        });
    });

    // watch_poll_no_changes: subscribe once, poll repeatedly (no changes expected)
    let poll_client = server.connect_http();
    let poll_ids: Vec<&str> = watch_ids.iter().map(|s| s.as_str()).collect();
    let sub_grid = rt.block_on(async { poll_client.watch_sub(&poll_ids, None).await.unwrap() });
    let watch_id = match sub_grid.meta.get("watchId") {
        Some(Kind::Str(s)) => s.clone(),
        _ => panic!("watchSub did not return a watchId in grid meta"),
    };

    c.bench_function("http_watch_poll_no_changes", |b| {
        b.iter(|| {
            rt.block_on(async {
                black_box(poll_client.watch_poll(&watch_id).await.unwrap());
            });
        });
    });
}

// ---------------------------------------------------------------------------
// Concurrent load benchmarks
// ---------------------------------------------------------------------------

fn concurrent_benchmarks(c: &mut Criterion) {
    let server = TestServer::start(build_test_graph(1000));
    let rt = server.runtime();

    // Pre-create clients to reuse connections across iterations (avoids ephemeral port exhaustion)
    let clients_10: Vec<_> = (0..10).map(|_| server.connect_http()).collect();

    c.bench_function("http_concurrent_reads_10", |b| {
        b.iter(|| {
            rt.block_on(async {
                let mut handles = Vec::new();
                for (i, client) in clients_10.iter().enumerate() {
                    let id = format!("p-{}", i * 100);
                    handles.push(async move { client.read_by_ids(&[id.as_str()]).await.unwrap() });
                }
                for result in join_all(handles).await {
                    black_box(result);
                }
            });
        });
    });

    let clients_50: Vec<_> = (0..50).map(|_| server.connect_http()).collect();

    c.bench_function("http_concurrent_reads_50", |b| {
        b.iter(|| {
            rt.block_on(async {
                let mut handles = Vec::new();
                for (i, client) in clients_50.iter().enumerate() {
                    let id = format!("p-{}", i % 1000);
                    handles.push(async move { client.read_by_ids(&[id.as_str()]).await.unwrap() });
                }
                for result in join_all(handles).await {
                    black_box(result);
                }
            });
        });
    });
}

criterion_group!(
    benches,
    http_benchmarks,
    his_benchmarks,
    watch_benchmarks,
    concurrent_benchmarks
);
criterion_main!(benches);

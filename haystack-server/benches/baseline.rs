mod fixtures;

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use fixtures::history::{FIRST_DAY, HistoryFixture, POINT_ID, SCALES};
use std::hint::black_box;
use std::time::Duration;

fn baseline(c: &mut Criterion) {
    let mut group = c.benchmark_group("history_baseline");
    for count in SCALES {
        let fixture = HistoryFixture::new(count);
        fixture.validate();
        eprintln!(
            "dataset=history_v1 items={} step_seconds=60 first_day_matches={} sha256={}",
            count,
            fixture.day_count(),
            fixture.digest()
        );
        let store = fixture.store();
        let (start, end) = fixture.day_bounds();
        group.bench_function(BenchmarkId::new("direct_full", count), |b| {
            b.iter_batched_ref(
                || (),
                |()| store.read(black_box(POINT_ID), None, None),
                BatchSize::PerIteration,
            );
        });
        group.bench_function(BenchmarkId::new("direct_first_day", count), |b| {
            b.iter_batched_ref(
                || (),
                |()| store.read(black_box(POINT_ID), Some(start), Some(end)),
                BatchSize::PerIteration,
            );
        });

        let server = fixture.server();
        let client = server.connect_http();
        group.bench_function(BenchmarkId::new("http_first_day", count), |b| {
            b.iter_batched_ref(
                || (),
                |()| {
                    server
                        .runtime()
                        .block_on(client.his_read(black_box(POINT_ID), black_box(FIRST_DAY)))
                        .expect("validated HTTP history read")
                },
                BatchSize::PerIteration,
            );
        });
    }
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(30).warm_up_time(Duration::from_millis(500)).measurement_time(Duration::from_secs(2)).nresamples(10_000);
    targets = baseline
}
criterion_main!(benches);

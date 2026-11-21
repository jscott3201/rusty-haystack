mod fixtures;

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use fixtures::{CacheState, CampusFixture, Query, SCALES, move_site_zero};
use std::hint::black_box;
use std::time::Duration;

fn baseline(c: &mut Criterion) {
    let mut group = c.benchmark_group("core_baseline");
    for campuses in SCALES {
        let fixture = CampusFixture::new(campuses);
        fixture.validate();
        eprintln!(
            "dataset=campus_v1 entities={} campuses={} sha256={}",
            fixture.len(),
            campuses,
            fixture.digest()
        );
        for query in Query::ALL {
            eprintln!(
                "query={} expression={:?} matches={}",
                query.id(),
                query.expression(),
                fixture.expected_ids(query, false).len()
            );
            for state in CacheState::ALL {
                group.bench_function(
                    BenchmarkId::new(format!("{}/{}", query.id(), state.id()), fixture.len()),
                    |b| {
                        b.iter_batched_ref(
                            || fixture.graph(query, state),
                            |graph| {
                                graph
                                    .read(black_box(query.expression()), 0)
                                    .expect("validated query")
                            },
                            BatchSize::PerIteration,
                        )
                    },
                );
            }
        }
        eprintln!(
            "mutation=move_site_zero query=nested_reference matches_after={}",
            fixture.expected_ids(Query::NestedReference, true).len()
        );
        group.bench_function(
            BenchmarkId::new("mutation_then_nested_query", fixture.len()),
            |b| {
                b.iter_batched_ref(
                    || fixture.graph(Query::NestedReference, CacheState::ResultWarm),
                    |graph| {
                        move_site_zero(graph);
                        graph
                            .read(black_box(Query::NestedReference.expression()), 0)
                            .expect("query after mutation")
                    },
                    BatchSize::PerIteration,
                );
            },
        );
    }
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(30).warm_up_time(Duration::from_millis(500)).measurement_time(Duration::from_secs(2)).nresamples(10_000);
    targets = baseline
}
criterion_main!(benches);

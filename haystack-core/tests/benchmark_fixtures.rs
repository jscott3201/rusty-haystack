#[path = "../benches/fixtures/mod.rs"]
mod fixtures;

use fixtures::{CacheState, CampusFixture, ENTITIES_PER_CAMPUS, Query, SCALES};

#[test]
fn benchmark_campus_counts_and_query_identities_are_exact() {
    for (campuses, entities, markers, nested) in [(12, 972, 96, 90), (125, 10_125, 1000, 945)] {
        assert!(SCALES.contains(&campuses));
        let fixture = CampusFixture::new(campuses);
        assert_eq!(fixture.len(), entities);
        assert_eq!(entities, campuses * ENTITIES_PER_CAMPUS);
        assert_eq!(fixture.expected_ids(Query::Marker, false).len(), markers);
        assert_eq!(fixture.expected_ids(Query::Reference, false).len(), 16);
        assert_eq!(
            fixture.expected_ids(Query::NestedReference, false).len(),
            nested
        );
        assert_eq!(
            fixture.expected_ids(Query::NestedReference, true).len(),
            nested - 15
        );
        fixture.validate();
        assert_eq!(fixture.digest(), CampusFixture::new(campuses).digest());
    }
}

#[test]
fn benchmark_nested_query_selects_exact_named_rows_and_mutation_removes_them() {
    let fixture = CampusFixture::new(1);
    let expected = [
        "pt-0-0-alarm-8",
        "pt-0-0-flow-2",
        "pt-0-0-occ-3",
        "pt-0-0-pressure-1",
        "pt-0-0-temp-0",
        "pt-0-1-alarm-8",
        "pt-0-1-flow-2",
        "pt-0-1-occ-3",
        "pt-0-1-pressure-1",
        "pt-0-1-temp-0",
        "pt-0-2-alarm-8",
        "pt-0-2-flow-2",
        "pt-0-2-occ-3",
        "pt-0-2-pressure-1",
        "pt-0-2-temp-0",
    ];
    assert_eq!(
        fixture.expected_ids(Query::NestedReference, false),
        expected
    );
    fixture.validate();
    assert!(
        fixture
            .expected_ids(Query::NestedReference, true)
            .is_empty()
    );
    assert_eq!(
        Query::ALL.map(Query::id),
        ["marker", "reference", "nested_reference"]
    );
    assert_eq!(
        CacheState::ALL.map(CacheState::id),
        ["cold", "ast_warm_result_cold", "result_warm"]
    );
}

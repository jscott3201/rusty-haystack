#[path = "../benches/fixtures/mod.rs"]
mod fixtures;

use fixtures::history::{HistoryFixture, SCALES};
use fixtures::items::history_items;
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

#[test]
fn benchmark_history_is_unique_across_days_and_direct_and_http_results_match() {
    for count in SCALES {
        let fixture = HistoryFixture::new(count);
        fixture.validate();
        assert_eq!(fixture.digest(), HistoryFixture::new(count).digest());
    }
    let rows = history_items(10_000, 0);
    assert_eq!(rows[1440].ts.timestamp() - rows[0].ts.timestamp(), 86_400);
    assert_eq!(rows[9999].ts.timestamp() - rows[0].ts.timestamp(), 599_940);
}

#[test]
fn benchmark_server_drop_joins_task_and_releases_listener() {
    let server = HistoryFixture::new(1000).server();
    let address: SocketAddr = server
        .api_url()
        .strip_prefix("http://")
        .unwrap()
        .strip_suffix("/api")
        .unwrap()
        .parse()
        .unwrap();
    drop(server);
    assert!(TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_err());
}

#[test]
#[should_panic(expected = "assertion `left == right` failed")]
fn benchmark_history_oracle_detects_a_wrong_timestamp_even_with_correct_cardinality() {
    let fixture = HistoryFixture::new(1000);
    let mut rows = history_items(1000, 0);
    rows[10].ts += chrono::Duration::seconds(1);
    fixture.assert_items(&rows, 1000);
}

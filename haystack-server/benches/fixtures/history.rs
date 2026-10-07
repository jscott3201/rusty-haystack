use super::items::{history_dicts, history_items};
use super::server::TestServer;
use chrono::{DateTime, FixedOffset, TimeZone};
use haystack_core::data::{HDict, HGrid};
use haystack_core::graph::{EntityGraph, SharedGraph};
use haystack_core::kinds::{HRef, Kind, Number};
use haystack_server::his_store::{HisItem, HisStore};
use sha2::{Digest, Sha256};

pub const SCALES: [usize; 2] = [1000, 10_000];
pub const POINT_ID: &str = "history-point";
pub const FIRST_DAY: &str = "2024-06-01";

pub struct HistoryFixture {
    pub count: usize,
    items: Vec<HisItem>,
}

impl HistoryFixture {
    pub fn new(count: usize) -> Self {
        assert!(SCALES.contains(&count));
        Self {
            count,
            items: history_items(count, 0),
        }
    }

    pub fn store(&self) -> HisStore {
        let store = HisStore::new();
        store.write(POINT_ID, self.items.clone());
        assert_eq!(store.len(POINT_ID), self.count);
        store
    }

    pub fn server(&self) -> TestServer {
        let mut graph = EntityGraph::new();
        let mut point = HDict::new();
        point.set("id", Kind::Ref(HRef::from_val(POINT_ID)));
        point.set("point", Kind::Marker);
        point.set("his", Kind::Marker);
        point.set("kind", Kind::Str("Number".into()));
        graph.add(point).expect("history fixture point");
        let server = TestServer::start(SharedGraph::new(graph));
        let client = server.connect_http();
        server
            .runtime()
            .block_on(client.his_write(POINT_ID, history_dicts(self.count, 0)))
            .expect("history fixture preload");
        server
    }

    pub fn day_bounds(&self) -> (DateTime<FixedOffset>, DateTime<FixedOffset>) {
        let utc = FixedOffset::east_opt(0).unwrap();
        (
            utc.with_ymd_and_hms(2024, 6, 1, 0, 0, 0).unwrap(),
            utc.with_ymd_and_hms(2024, 6, 1, 23, 59, 59).unwrap(),
        )
    }

    pub fn day_count(&self) -> usize {
        self.count.min(1440)
    }

    pub fn assert_items(&self, rows: &[HisItem], count: usize) {
        assert_eq!(rows.len(), count);
        // Independent expected epoch/step/value formula, not a comparison back
        // to the input vector that a broken timestamp generator could share.
        for (index, item) in rows.iter().enumerate() {
            assert_eq!(item.ts.timestamp(), 1_717_200_000 + index as i64 * 60);
            assert_eq!(item.ts.offset().local_minus_utc(), 0);
            assert_eq!(
                item.val,
                Kind::Number(Number::unitless(70.0 + index as f64 / 4.0))
            );
        }
        assert!(rows.windows(2).all(|pair| pair[0].ts < pair[1].ts));
    }

    pub fn assert_grid(&self, grid: &HGrid) {
        assert_eq!(
            grid.meta.get("id"),
            Some(&Kind::Ref(HRef::from_val(POINT_ID)))
        );
        let items: Vec<_> = grid
            .rows
            .iter()
            .map(|row| {
                let Some(Kind::DateTime(ts)) = row.get("ts") else {
                    panic!("missing history timestamp")
                };
                assert_eq!(ts.tz_name, "UTC");
                HisItem {
                    ts: ts.dt,
                    val: row.get("val").expect("history value").clone(),
                }
            })
            .collect();
        self.assert_items(&items, self.day_count());
    }

    pub fn digest(&self) -> String {
        let mut hash = Sha256::new();
        for item in &self.items {
            hash.update(format!("{}\t{}\n", item.ts.to_rfc3339(), item.val).as_bytes());
        }
        hash.finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// Separate validation instances never warm the measured store/server.
    pub fn validate(&self) {
        self.assert_items(&self.items, self.count);
        let store = self.store();
        self.assert_items(&store.read(POINT_ID, None, None), self.count);
        let (start, end) = self.day_bounds();
        self.assert_items(
            &store.read(POINT_ID, Some(start), Some(end)),
            self.day_count(),
        );
        let server = self.server();
        let client = server.connect_http();
        let grid = server
            .runtime()
            .block_on(client.his_read(POINT_ID, FIRST_DAY))
            .expect("history validation read");
        self.assert_grid(&grid);
    }
}

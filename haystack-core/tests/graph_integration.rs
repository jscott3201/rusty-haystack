// Integration tests for the EntityGraph layer.

use haystack_core::data::{HCol, HDict, HGrid};
use haystack_core::graph::{DiffOp, EntityGraph, SharedGraph};
use haystack_core::kinds::{HRef, Kind, Number};
use haystack_core::ontology::DefNamespace;

// ── Helpers ──

fn make_site(id: &str, city: &str) -> HDict {
    let mut d = HDict::new();
    d.set("id", Kind::Ref(HRef::from_val(id)));
    d.set("site", Kind::Marker);
    d.set("dis", Kind::Str(format!("Site {id}")));
    d.set(
        "area",
        Kind::Number(Number::new(4500.0, Some("ft\u{00b2}".into()))),
    );
    d.set("geoCity", Kind::Str(city.into()));
    d
}

fn make_equip(id: &str, site_ref: &str, dis: &str) -> HDict {
    let mut d = HDict::new();
    d.set("id", Kind::Ref(HRef::from_val(id)));
    d.set("equip", Kind::Marker);
    d.set("dis", Kind::Str(dis.into()));
    d.set("siteRef", Kind::Ref(HRef::from_val(site_ref)));
    d
}

fn make_ahu(id: &str, site_ref: &str, dis: &str) -> HDict {
    let mut d = make_equip(id, site_ref, dis);
    d.set("ahu", Kind::Marker);
    d
}

fn make_point(id: &str, equip_ref: &str, dis: &str) -> HDict {
    let mut d = HDict::new();
    d.set("id", Kind::Ref(HRef::from_val(id)));
    d.set("point", Kind::Marker);
    d.set("sensor", Kind::Marker);
    d.set("temp", Kind::Marker);
    d.set("dis", Kind::Str(dis.into()));
    d.set("equipRef", Kind::Ref(HRef::from_val(equip_ref)));
    d.set(
        "curVal",
        Kind::Number(Number::new(72.5, Some("\u{00b0}F".into()))),
    );
    d
}

fn build_sample_graph() -> EntityGraph {
    let mut g = EntityGraph::new();

    g.add(make_site("site-1", "Richmond")).unwrap();
    g.add(make_site("site-2", "Norfolk")).unwrap();

    g.add(make_ahu("ahu-1", "site-1", "AHU-1")).unwrap();
    g.add(make_equip("boiler-1", "site-1", "Boiler-1")).unwrap();
    g.add(make_equip("pump-1", "site-2", "Pump-1")).unwrap();

    g.add(make_point("temp-1", "ahu-1", "Zone Temp 1")).unwrap();
    g.add(make_point("temp-2", "ahu-1", "Zone Temp 2")).unwrap();
    g.add(make_point("temp-3", "boiler-1", "Supply Temp"))
        .unwrap();

    g
}

// ── Entity loading and filter queries ──

#[test]
fn load_entities_and_filter() {
    let g = build_sample_graph();
    assert_eq!(g.len(), 8);

    // Find all sites.
    let sites = g.read_all("site", 0).unwrap();
    assert_eq!(sites.len(), 2);

    // Find all equipment.
    let equips = g.read_all("equip", 0).unwrap();
    assert_eq!(equips.len(), 3); // ahu-1, boiler-1, pump-1

    // Find all points.
    let points = g.read_all("point", 0).unwrap();
    assert_eq!(points.len(), 3);

    // Combined filter.
    let sensors = g.read_all("point and sensor and temp", 0).unwrap();
    assert_eq!(sensors.len(), 3);
}

#[test]
fn filter_with_comparison() {
    let g = build_sample_graph();

    let results = g.read_all("geoCity == \"Richmond\"", 0).unwrap();
    assert_eq!(results.len(), 1);
    let entity = results[0];
    assert_eq!(entity.id().unwrap().val, "site-1");
}

#[test]
fn filter_with_or() {
    let g = build_sample_graph();

    let results = g.read_all("site or point", 0).unwrap();
    assert_eq!(results.len(), 5); // 2 sites + 3 points
}

#[test]
fn filter_with_missing() {
    let g = build_sample_graph();

    let results = g.read_all("equip and not ahu", 0).unwrap();
    assert_eq!(results.len(), 2); // boiler-1, pump-1
}

#[test]
fn filter_with_limit() {
    let g = build_sample_graph();

    let results = g.read_all("point", 1).unwrap();
    assert_eq!(results.len(), 1);
}

// ── Ref traversal ──

#[test]
fn ref_traversal_forward() {
    let g = build_sample_graph();

    // ahu-1 -> site-1 via siteRef
    let targets = g.refs_from("ahu-1", Some("siteRef"));
    assert_eq!(targets, vec!["site-1".to_string()]);

    // temp-1 -> ahu-1 via equipRef
    let targets = g.refs_from("temp-1", Some("equipRef"));
    assert_eq!(targets, vec!["ahu-1".to_string()]);
}

#[test]
fn ref_traversal_reverse() {
    let g = build_sample_graph();

    // Who points to site-1?
    let mut sources = g.refs_to("site-1", Some("siteRef"));
    sources.sort();
    assert_eq!(sources.len(), 2); // ahu-1, boiler-1

    // Who points to ahu-1?
    let mut sources = g.refs_to("ahu-1", Some("equipRef"));
    sources.sort();
    assert_eq!(sources.len(), 2); // temp-1, temp-2
}

#[test]
fn ref_traversal_all_types() {
    let g = build_sample_graph();

    // All refs from ahu-1 (should be siteRef only, since id is excluded).
    let targets = g.refs_from("ahu-1", None);
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0], "site-1");
}

// ── from_grid / to_grid round-trip ──

#[test]
fn grid_round_trip() {
    let g = build_sample_graph();

    // Export all sites to a grid.
    let grid = g.to_grid("site").unwrap();
    assert_eq!(grid.len(), 2);
    assert!(grid.col("id").is_some());
    assert!(grid.col("site").is_some());

    // Re-import into a new graph.
    let g2 = EntityGraph::from_grid(&grid, None).unwrap();
    assert_eq!(g2.len(), 2);
    assert!(g2.contains("site-1"));
    assert!(g2.contains("site-2"));

    // Verify filter works on re-imported graph.
    let results = g2.read_all("site", 0).unwrap();
    assert_eq!(results.len(), 2);
}

#[test]
fn from_grid_with_all_entities() {
    // Build a grid manually.
    let cols = vec![HCol::new("id"), HCol::new("site"), HCol::new("dis")];
    let mut row1 = HDict::new();
    row1.set("id", Kind::Ref(HRef::from_val("s1")));
    row1.set("site", Kind::Marker);
    row1.set("dis", Kind::Str("Site One".into()));

    let mut row2 = HDict::new();
    row2.set("id", Kind::Ref(HRef::from_val("s2")));
    row2.set("site", Kind::Marker);
    row2.set("dis", Kind::Str("Site Two".into()));

    let grid = HGrid::from_parts(HDict::new(), cols, vec![row1, row2]);
    let g = EntityGraph::from_grid(&grid, None).unwrap();

    assert_eq!(g.len(), 2);
    assert_eq!(
        g.get("s1").unwrap().get("dis"),
        Some(&Kind::Str("Site One".into()))
    );
}

// ── Change tracking ──

#[test]
fn changelog_integration() {
    let mut g = EntityGraph::new();
    let v0 = g.version();

    g.add(make_site("site-1", "Richmond")).unwrap();
    let v1 = g.version();

    g.add(make_equip("equip-1", "site-1", "AHU-1")).unwrap();
    let v2 = g.version();

    let mut changes = HDict::new();
    changes.set("dis", Kind::Str("Updated AHU".into()));
    g.update("equip-1", changes).unwrap();
    let v3 = g.version();

    g.remove("equip-1").unwrap();
    let v4 = g.version();

    assert_eq!(v0, 0);
    assert_eq!(v1, 1);
    assert_eq!(v2, 2);
    assert_eq!(v3, 3);
    assert_eq!(v4, 4);

    // Changes since v0 should be all 4.
    let all_changes = g.changes_since(v0).unwrap();
    assert_eq!(all_changes.len(), 4);

    // Changes since v2 should be update + remove.
    let recent = g.changes_since(v2).unwrap();
    assert_eq!(recent.len(), 2);
    assert_eq!(recent[0].op, DiffOp::Update);
    assert_eq!(recent[1].op, DiffOp::Remove);

    // Changes since latest should be empty.
    let none = g.changes_since(v4).unwrap();
    assert!(none.is_empty());
}

// ── Namespace-aware operations ──

#[test]
fn spec_fitting_with_namespace() {
    // Build a minimal namespace.
    let trio = "\
def:^marker
doc:\"Marker type\"
is:[^marker]
lib:^lib:ph
---
def:^entity
doc:\"Top-level entity\"
is:[^marker]
lib:^lib:ph
---
def:^equip
doc:\"Equipment\"
is:[^entity]
lib:^lib:phIoT
mandatory
---
def:^ahu
doc:\"Air Handling Unit\"
is:[^equip]
lib:^lib:phIoT
mandatory
---
def:^site
doc:\"A site\"
is:[^entity]
lib:^lib:ph
---
def:^lib:ph
doc:\"Project Haystack core\"
is:[^lib]
lib:^lib:ph
version:\"4.0.0\"
---
def:^lib:phIoT
doc:\"Project Haystack IoT\"
is:[^lib]
lib:^lib:phIoT
version:\"4.0.0\"
depends:[^lib:ph]
";

    let mut ns = DefNamespace::new();
    ns.load_trio_str(trio).unwrap();

    let mut g = EntityGraph::with_namespace(ns);

    // Valid AHU: has both ahu and equip markers.
    let mut ahu = HDict::new();
    ahu.set("id", Kind::Ref(HRef::from_val("ahu-1")));
    ahu.set("ahu", Kind::Marker);
    ahu.set("equip", Kind::Marker);
    ahu.set("dis", Kind::Str("AHU-1".into()));
    g.add(ahu).unwrap();

    // Invalid AHU: missing equip marker.
    let mut bad_ahu = HDict::new();
    bad_ahu.set("id", Kind::Ref(HRef::from_val("ahu-2")));
    bad_ahu.set("ahu", Kind::Marker);
    // No equip marker!
    bad_ahu.set("dis", Kind::Str("Bad AHU".into()));
    g.add(bad_ahu).unwrap();

    // Plain site.
    let mut site = HDict::new();
    site.set("id", Kind::Ref(HRef::from_val("site-1")));
    site.set("site", Kind::Marker);
    site.set("dis", Kind::Str("Main Site".into()));
    g.add(site).unwrap();

    // entities_fitting: only ahu-1 should fit "ahu" spec.
    let fitting = g.entities_fitting("ahu");
    assert_eq!(fitting.len(), 1);
    assert_eq!(fitting[0].id().unwrap().val, "ahu-1");

    // validate: should find issue with ahu-2.
    let issues = g.validate();
    assert!(!issues.is_empty());
    let has_ahu2_issue = issues
        .iter()
        .any(|i| i.entity.as_deref() == Some("ahu-2") && i.detail.contains("equip"));
    assert!(has_ahu2_issue);
}

// ── SharedGraph integration ──

#[test]
fn shared_graph_concurrent_access() {
    use std::thread;

    let g = build_sample_graph();
    let sg = SharedGraph::new(g);

    // Spawn readers.
    let mut handles = Vec::new();
    for _ in 0..5 {
        let sg_clone = sg.clone();
        handles.push(thread::spawn(move || {
            let entity = sg_clone.get("site-1");
            assert!(entity.is_some());
            let grid = sg_clone.read_filter("site", 0).unwrap();
            // Writer may or may not have added site-3 yet.
            assert!(grid.len() >= 2 && grid.len() <= 3);
        }));
    }

    // Spawn a writer.
    let sg_clone = sg.clone();
    handles.push(thread::spawn(move || {
        let mut new_site = HDict::new();
        new_site.set("id", Kind::Ref(HRef::from_val("site-3")));
        new_site.set("site", Kind::Marker);
        new_site.set("dis", Kind::Str("Site Three".into()));
        sg_clone.add(new_site).unwrap();
    }));

    for h in handles {
        h.join().unwrap();
    }

    // After all threads, site-3 should exist.
    assert!(sg.get("site-3").is_some());
}

#[test]
fn shared_graph_update_and_version() {
    let sg = SharedGraph::new(EntityGraph::new());

    let mut site = HDict::new();
    site.set("id", Kind::Ref(HRef::from_val("site-1")));
    site.set("site", Kind::Marker);
    site.set("dis", Kind::Str("Original".into()));
    sg.add(site).unwrap();

    assert_eq!(sg.version(), 1);

    let mut changes = HDict::new();
    changes.set("dis", Kind::Str("Updated".into()));
    sg.update("site-1", changes).unwrap();

    assert_eq!(sg.version(), 2);
    let entity = sg.get("site-1").unwrap();
    assert_eq!(entity.get("dis"), Some(&Kind::Str("Updated".into())));
}

// ── Edge cases ──

#[test]
fn empty_graph_queries() {
    let g = EntityGraph::new();

    let results = g.read_all("site", 0).unwrap();
    assert!(results.is_empty());

    let grid = g.to_grid("site").unwrap();
    assert!(grid.is_empty());

    assert!(g.refs_from("anything", None).is_empty());
    assert!(g.refs_to("anything", None).is_empty());
}

#[test]
fn update_preserves_refs_in_query() {
    let mut g = EntityGraph::new();
    g.add(make_site("site-1", "Richmond")).unwrap();
    g.add(make_equip("equip-1", "site-1", "AHU")).unwrap();

    // Update equip with new tag.
    let mut changes = HDict::new();
    changes.set("power", Kind::Number(Number::new(10.0, Some("kW".into()))));
    g.update("equip-1", changes).unwrap();

    // Should still find equip-1 via filter.
    let results = g.read_all("equip", 0).unwrap();
    assert_eq!(results.len(), 1);

    // Ref traversal should still work.
    let targets = g.refs_from("equip-1", Some("siteRef"));
    assert_eq!(targets, vec!["site-1".to_string()]);

    // New tag should be present.
    let entity = g.get("equip-1").unwrap();
    assert!(entity.has("power"));
}

#[test]
fn remove_updates_ref_indices() {
    let mut g = EntityGraph::new();
    g.add(make_site("site-1", "Richmond")).unwrap();
    g.add(make_equip("equip-1", "site-1", "AHU")).unwrap();

    g.remove("equip-1").unwrap();

    // No more refs to site-1.
    assert!(g.refs_to("site-1", None).is_empty());
}

// ── Bulk operations ──

#[test]
fn bulk_import_100_entities() {
    let mut g = EntityGraph::new();

    // Add 5 sites
    for i in 0..5 {
        g.add(make_site(&format!("site-{i}"), &format!("City-{i}")))
            .unwrap();
    }

    // Add 20 equips per site
    for s in 0..5 {
        for e in 0..20 {
            let id = format!("equip-{s}-{e}");
            let site_ref = format!("site-{s}");
            g.add(make_equip(&id, &site_ref, &format!("Equip {e}")))
                .unwrap();
        }
    }

    assert_eq!(g.len(), 105); // 5 sites + 100 equips

    // Bitmap queries should handle scale
    let sites = g.read_all("site", 0).unwrap();
    assert_eq!(sites.len(), 5);

    let equips = g.read_all("equip", 0).unwrap();
    assert_eq!(equips.len(), 100);

    // Combined filter
    let site_or_equip = g.read_all("site or equip", 0).unwrap();
    assert_eq!(site_or_equip.len(), 105);

    // Verify ref traversal at scale
    for s in 0..5 {
        let sources = g.refs_to(&format!("site-{s}"), Some("siteRef"));
        assert_eq!(sources.len(), 20);
    }
}

#[test]
fn filter_with_path_traversal() {
    let g = build_sample_graph();

    // equipRef->siteRef should follow: point has equipRef -> equip has siteRef
    // This is a Has check on the two-segment path.
    let results = g.read_all("siteRef->geoCity", 0).unwrap();
    // ahu-1, boiler-1, pump-1 all have siteRef pointing to sites with geoCity
    assert_eq!(results.len(), 3);

    // Comparison through path traversal
    let results = g.read_all("siteRef->geoCity == \"Richmond\"", 0).unwrap();
    // ahu-1 and boiler-1 point to site-1 (Richmond)
    assert_eq!(results.len(), 2);
}

#[test]
fn shared_graph_remove_and_verify() {
    let sg = SharedGraph::new(EntityGraph::new());

    let mut site = HDict::new();
    site.set("id", Kind::Ref(HRef::from_val("site-1")));
    site.set("site", Kind::Marker);
    site.set("dis", Kind::Str("Test Site".into()));
    sg.add(site).unwrap();

    assert!(sg.contains("site-1"));

    let removed = sg.remove("site-1").unwrap();
    assert!(removed.has("site"));
    assert!(!sg.contains("site-1"));
    assert!(sg.is_empty());
}

#[test]
fn shared_graph_bulk_concurrent_writes() {
    use std::thread;

    let sg = SharedGraph::new(EntityGraph::new());
    let mut handles = Vec::new();

    // 4 threads each adding 25 entities
    for t in 0..4 {
        let sg_clone = sg.clone();
        handles.push(thread::spawn(move || {
            for i in 0..25 {
                let mut d = HDict::new();
                d.set("id", Kind::Ref(HRef::from_val(format!("e-{t}-{i}"))));
                d.set("site", Kind::Marker);
                sg_clone.add(d).unwrap();
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    assert_eq!(sg.len(), 100);
    assert_eq!(sg.version(), 100);
}

#[test]
fn multiple_updates_same_entity() {
    let mut g = EntityGraph::new();
    g.add(make_site("site-1", "Richmond")).unwrap();

    // Apply multiple updates
    for i in 0..10 {
        let mut changes = HDict::new();
        changes.set("dis", Kind::Str(format!("Site v{i}")));
        g.update("site-1", changes).unwrap();
    }

    assert_eq!(g.version(), 11); // 1 add + 10 updates
    let entity = g.get("site-1").unwrap();
    assert_eq!(entity.get("dis"), Some(&Kind::Str("Site v9".into())));

    // Changelog should have all 11 entries
    let changes = g.changes_since(0).unwrap();
    assert_eq!(changes.len(), 11);
}

#[test]
fn add_remove_readd() {
    let mut g = EntityGraph::new();
    g.add(make_site("site-1", "Richmond")).unwrap();
    g.remove("site-1").unwrap();

    // Re-add with different data
    let mut site2 = HDict::new();
    site2.set("id", Kind::Ref(HRef::from_val("site-1")));
    site2.set("site", Kind::Marker);
    site2.set("dis", Kind::Str("Re-added Site".into()));
    g.add(site2).unwrap();

    let entity = g.get("site-1").unwrap();
    assert_eq!(entity.get("dis"), Some(&Kind::Str("Re-added Site".into())));
    assert_eq!(g.len(), 1);
    assert_eq!(g.version(), 3); // add + remove + add
}

#[test]
fn contains_after_operations() {
    let mut g = EntityGraph::new();
    assert!(!g.contains("site-1"));

    g.add(make_site("site-1", "Richmond")).unwrap();
    assert!(g.contains("site-1"));

    g.remove("site-1").unwrap();
    assert!(!g.contains("site-1"));
}

#[test]
fn error_on_invalid_filter() {
    let g = build_sample_graph();
    let result = g.read_all("!!!", 0);
    assert!(result.is_err());
}

#[test]
fn read_returns_correct_grid_columns() {
    let g = build_sample_graph();
    let grid = g.read("site", 0).unwrap();

    // Grid should have columns for all tags in the matched entities.
    assert!(grid.col("id").is_some());
    assert!(grid.col("site").is_some());
    assert!(grid.col("dis").is_some());
    assert!(grid.col("area").is_some());
    assert!(grid.col("geoCity").is_some());
}

#[test]
fn shared_graph_read_closure() {
    let sg = SharedGraph::new(EntityGraph::new());

    let mut site = HDict::new();
    site.set("id", Kind::Ref(HRef::from_val("site-1")));
    site.set("site", Kind::Marker);
    site.set("dis", Kind::Str("Test".into()));
    sg.add(site).unwrap();

    // Use the read closure API
    let dis = sg.read(|g| g.get("site-1").and_then(|e| e.get("dis")).cloned());
    assert_eq!(dis, Some(Kind::Str("Test".into())));
}

#[test]
fn shared_graph_write_closure() {
    let sg = SharedGraph::new(EntityGraph::new());

    // Use the write closure API to add multiple entities atomically
    sg.write(|g| {
        g.add(make_site("site-1", "Richmond")).unwrap();
        g.add(make_site("site-2", "Norfolk")).unwrap();
    });

    assert_eq!(sg.len(), 2);
    assert_eq!(sg.version(), 2);
}

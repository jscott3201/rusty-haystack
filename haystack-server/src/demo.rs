// Demo dataset — a realistic small building automation system for testing.

use haystack_core::data::HDict;
use haystack_core::kinds::{HRef, Kind, Number};

/// Generate a complete demo dataset representing a small building automation system.
///
/// Returns 36 entities:
/// - 1 site
/// - 3 floors
/// - 2 AHUs (on Floor 1)
/// - 6 VAVs (3 per AHU, spread across floors)
/// - 24 points (4 per VAV)
pub fn demo_entities() -> Vec<HDict> {
    let mut entities = Vec::new();

    // ── Site ──
    let site_id = "demo-site";
    entities.push(make_site(site_id));

    // ── Floors ──
    let floor_ids: Vec<String> = (1..=3).map(|n| format!("demo-floor-{n}")).collect();
    for (i, floor_id) in floor_ids.iter().enumerate() {
        entities.push(make_floor(floor_id, i + 1, site_id));
    }

    // ── AHUs (both on Floor 1) ──
    let ahu_ids: Vec<String> = (1..=2).map(|n| format!("demo-ahu-{n}")).collect();
    for (i, ahu_id) in ahu_ids.iter().enumerate() {
        entities.push(make_ahu(ahu_id, i + 1, site_id, &floor_ids[0]));
    }

    // ── VAVs (3 per AHU, spread across floors) ──
    // AHU-1: VAV-1-01 (Floor 1), VAV-1-02 (Floor 2), VAV-1-03 (Floor 3)
    // AHU-2: VAV-2-01 (Floor 1), VAV-2-02 (Floor 2), VAV-2-03 (Floor 3)
    for (ahu_idx, ahu_id) in ahu_ids.iter().enumerate() {
        let ahu_num = ahu_idx + 1;
        for vav_num in 1..=3 {
            let vav_id = format!("demo-vav-{ahu_num}-{vav_num:02}");
            let floor_id = &floor_ids[vav_num - 1];
            entities.push(make_vav(
                &vav_id, ahu_num, vav_num, site_id, floor_id, ahu_id,
            ));

            // ── Points (4 per VAV) ──
            entities.push(make_zone_air_temp_sensor(
                &vav_id, site_id, floor_id, ahu_id,
            ));
            entities.push(make_zone_air_temp_sp(&vav_id, site_id, floor_id, ahu_id));
            entities.push(make_damper_cmd(&vav_id, site_id, floor_id, ahu_id));
            entities.push(make_occ_sensor(&vav_id, site_id, floor_id, ahu_id));
        }
    }

    entities
}

fn make_site(id: &str) -> HDict {
    let mut d = HDict::new();
    d.set("id", Kind::Ref(HRef::new(id, Some("Demo Building".into()))));
    d.set("site", Kind::Marker);
    d.set("dis", Kind::Str("Demo Building".into()));
    d.set(
        "area",
        Kind::Number(Number::new(50000.0, Some("ft\u{00b2}".into()))),
    );
    d.set("geoCity", Kind::Str("Richmond".into()));
    d.set("geoState", Kind::Str("VA".into()));
    d.set("tz", Kind::Str("New_York".into()));
    d
}

fn make_floor(id: &str, num: usize, site_id: &str) -> HDict {
    let mut d = HDict::new();
    let dis = format!("Floor {num}");
    d.set("id", Kind::Ref(HRef::new(id, Some(dis.clone()))));
    d.set("floor", Kind::Marker);
    d.set("dis", Kind::Str(dis));
    d.set("siteRef", Kind::Ref(HRef::from_val(site_id)));
    d
}

fn make_ahu(id: &str, num: usize, site_id: &str, floor_id: &str) -> HDict {
    let mut d = HDict::new();
    let dis = format!("AHU-{num}");
    d.set("id", Kind::Ref(HRef::new(id, Some(dis.clone()))));
    d.set("ahu", Kind::Marker);
    d.set("equip", Kind::Marker);
    d.set("dis", Kind::Str(dis));
    d.set("siteRef", Kind::Ref(HRef::from_val(site_id)));
    d.set("floorRef", Kind::Ref(HRef::from_val(floor_id)));
    d
}

fn make_vav(
    id: &str,
    ahu_num: usize,
    vav_num: usize,
    site_id: &str,
    floor_id: &str,
    ahu_id: &str,
) -> HDict {
    let mut d = HDict::new();
    let dis = format!("VAV-{ahu_num}-{vav_num:02}");
    d.set("id", Kind::Ref(HRef::new(id, Some(dis.clone()))));
    d.set("vav", Kind::Marker);
    d.set("equip", Kind::Marker);
    d.set("dis", Kind::Str(dis));
    d.set("siteRef", Kind::Ref(HRef::from_val(site_id)));
    d.set("floorRef", Kind::Ref(HRef::from_val(floor_id)));
    d.set("equipRef", Kind::Ref(HRef::from_val(ahu_id)));
    d
}

fn make_zone_air_temp_sensor(vav_id: &str, site_id: &str, floor_id: &str, _ahu_id: &str) -> HDict {
    let mut d = HDict::new();
    let pt_id = format!("{vav_id}-zat");
    let dis = format!("{} Zone Air Temp", vav_dis(vav_id));
    d.set("id", Kind::Ref(HRef::new(&pt_id, Some(dis.clone()))));
    d.set("point", Kind::Marker);
    d.set("sensor", Kind::Marker);
    d.set("zone", Kind::Marker);
    d.set("air", Kind::Marker);
    d.set("temp", Kind::Marker);
    d.set("his", Kind::Marker);
    d.set("kind", Kind::Str("Number".into()));
    d.set("unit", Kind::Str("\u{00b0}F".into()));
    d.set(
        "curVal",
        Kind::Number(Number::new(72.0, Some("\u{00b0}F".into()))),
    );
    d.set("dis", Kind::Str(dis));
    d.set("siteRef", Kind::Ref(HRef::from_val(site_id)));
    d.set("floorRef", Kind::Ref(HRef::from_val(floor_id)));
    d.set("equipRef", Kind::Ref(HRef::from_val(vav_id)));
    d
}

fn make_zone_air_temp_sp(vav_id: &str, site_id: &str, floor_id: &str, _ahu_id: &str) -> HDict {
    let mut d = HDict::new();
    let pt_id = format!("{vav_id}-zatsp");
    let dis = format!("{} Zone Air Temp SP", vav_dis(vav_id));
    d.set("id", Kind::Ref(HRef::new(&pt_id, Some(dis.clone()))));
    d.set("point", Kind::Marker);
    d.set("sp", Kind::Marker);
    d.set("zone", Kind::Marker);
    d.set("air", Kind::Marker);
    d.set("temp", Kind::Marker);
    d.set("kind", Kind::Str("Number".into()));
    d.set("unit", Kind::Str("\u{00b0}F".into()));
    d.set(
        "curVal",
        Kind::Number(Number::new(72.0, Some("\u{00b0}F".into()))),
    );
    d.set("dis", Kind::Str(dis));
    d.set("siteRef", Kind::Ref(HRef::from_val(site_id)));
    d.set("floorRef", Kind::Ref(HRef::from_val(floor_id)));
    d.set("equipRef", Kind::Ref(HRef::from_val(vav_id)));
    d
}

fn make_damper_cmd(vav_id: &str, site_id: &str, floor_id: &str, _ahu_id: &str) -> HDict {
    let mut d = HDict::new();
    let pt_id = format!("{vav_id}-dmpr");
    let dis = format!("{} Damper Cmd", vav_dis(vav_id));
    d.set("id", Kind::Ref(HRef::new(&pt_id, Some(dis.clone()))));
    d.set("point", Kind::Marker);
    d.set("cmd", Kind::Marker);
    d.set("damper", Kind::Marker);
    d.set("writable", Kind::Marker);
    d.set("kind", Kind::Str("Number".into()));
    d.set("unit", Kind::Str("%".into()));
    d.set("curVal", Kind::Number(Number::new(85.0, Some("%".into()))));
    d.set("dis", Kind::Str(dis));
    d.set("siteRef", Kind::Ref(HRef::from_val(site_id)));
    d.set("floorRef", Kind::Ref(HRef::from_val(floor_id)));
    d.set("equipRef", Kind::Ref(HRef::from_val(vav_id)));
    d
}

fn make_occ_sensor(vav_id: &str, site_id: &str, floor_id: &str, _ahu_id: &str) -> HDict {
    let mut d = HDict::new();
    let pt_id = format!("{vav_id}-occ");
    let dis = format!("{} Occ", vav_dis(vav_id));
    d.set("id", Kind::Ref(HRef::new(&pt_id, Some(dis.clone()))));
    d.set("point", Kind::Marker);
    d.set("sensor", Kind::Marker);
    d.set("occ", Kind::Marker);
    d.set("kind", Kind::Str("Bool".into()));
    d.set("curVal", Kind::Bool(true));
    d.set("dis", Kind::Str(dis));
    d.set("siteRef", Kind::Ref(HRef::from_val(site_id)));
    d.set("floorRef", Kind::Ref(HRef::from_val(floor_id)));
    d.set("equipRef", Kind::Ref(HRef::from_val(vav_id)));
    d
}

/// Extract a human-readable display name from a VAV entity ID.
fn vav_dis(vav_id: &str) -> String {
    // "demo-vav-1-01" -> "VAV-1-01"
    vav_id
        .strip_prefix("demo-")
        .unwrap_or(vav_id)
        .to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn demo_has_expected_count() {
        let entities = demo_entities();
        // 1 site + 3 floors + 2 AHUs + 6 VAVs + 24 points = 36
        assert_eq!(entities.len(), 36);
    }

    #[test]
    fn demo_site_has_required_tags() {
        let entities = demo_entities();
        let site = entities
            .iter()
            .find(|e| e.has("site"))
            .expect("should have a site entity");
        assert!(site.has("site"));
        assert!(site.has("dis"));
        assert!(site.has("area"));
        assert_eq!(site.get("dis"), Some(&Kind::Str("Demo Building".into())));
        assert_eq!(
            site.get("area"),
            Some(&Kind::Number(Number::new(
                50000.0,
                Some("ft\u{00b2}".into())
            )))
        );
    }

    #[test]
    fn demo_refs_are_valid() {
        let entities = demo_entities();

        // Collect all entity IDs.
        let ids: HashSet<String> = entities
            .iter()
            .filter_map(|e| e.id().map(|r| r.val.clone()))
            .collect();

        // Check every siteRef, floorRef, equipRef points to a valid ID.
        for entity in &entities {
            for ref_tag in &["siteRef", "floorRef", "equipRef"] {
                if let Some(Kind::Ref(r)) = entity.get(ref_tag) {
                    assert!(
                        ids.contains(&r.val),
                        "entity {:?} has {} = @{} which is not a valid entity ID",
                        entity.id().map(|r| &r.val),
                        ref_tag,
                        r.val
                    );
                }
            }
        }
    }

    #[test]
    fn demo_points_have_kind() {
        let entities = demo_entities();
        let points: Vec<&HDict> = entities.iter().filter(|e| e.has("point")).collect();

        // Should have 24 points (4 per VAV * 6 VAVs)
        assert_eq!(points.len(), 24);

        for pt in &points {
            assert!(
                pt.has("kind"),
                "point {:?} is missing 'kind' tag",
                pt.id().map(|r| &r.val)
            );
            match pt.get("kind") {
                Some(Kind::Str(s)) => {
                    assert!(s == "Number" || s == "Bool", "unexpected kind value: {s}");
                }
                other => panic!("expected kind to be a Str, got {:?}", other),
            }
        }
    }
}

use criterion::{Criterion, criterion_group, criterion_main};
use haystack_core::codecs::codec_for;
use haystack_core::data::{HCol, HDict, HGrid};
use haystack_core::filter;
use haystack_core::graph::{EntityGraph, SharedGraph};
use haystack_core::kinds::{self, HDateTime, HRef, Kind, Number, Uri};
use haystack_core::ontology::DefNamespace;
use haystack_core::xeto;
use std::hint::black_box;

fn make_sample_entity(i: usize) -> HDict {
    let mut d = HDict::new();
    d.set("id", Kind::Ref(HRef::from_val(format!("p-{i}"))));
    d.set("dis", Kind::Str(format!("Point {i}")));
    d.set("site", Kind::Marker);
    d.set("equip", Kind::Marker);
    d.set("point", Kind::Marker);
    d.set(
        "temp",
        Kind::Number(Number::new(72.5, Some("\u{00b0}F".into()))),
    );
    d.set("siteRef", Kind::Ref(HRef::from_val("site-1")));
    d
}

fn make_sample_grid(n: usize) -> HGrid {
    let rows: Vec<HDict> = (0..n).map(make_sample_entity).collect();

    // Collect unique column names.
    let mut col_names: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for row in &rows {
        for name in row.tag_names() {
            if seen.insert(name.to_string()) {
                col_names.push(name.to_string());
            }
        }
    }
    col_names.sort();
    let cols: Vec<HCol> = col_names.iter().map(|n| HCol::new(n.as_str())).collect();

    HGrid::from_parts(HDict::new(), cols, rows)
}

fn make_mixed_type_grid() -> HGrid {
    use chrono::{FixedOffset, NaiveDate, NaiveTime, TimeZone};

    let offset = FixedOffset::west_opt(5 * 3600).unwrap();
    let dt = offset.with_ymd_and_hms(2024, 1, 15, 10, 30, 0).unwrap();
    let hdt = HDateTime::new(dt, "New_York");

    let rows: Vec<HDict> = (0..100)
        .map(|i| {
            let mut d = HDict::new();
            d.set("id", Kind::Ref(HRef::from_val(format!("mixed-{i}"))));
            d.set("dis", Kind::Str(format!("Mixed row {i}")));
            d.set(
                "temp",
                Kind::Number(Number::new(
                    68.0 + (i as f64) * 0.1,
                    Some("\u{00b0}F".into()),
                )),
            );
            d.set("active", Kind::Bool(i % 2 == 0));
            d.set("site", Kind::Marker);
            d.set(
                "startTime",
                Kind::Time(NaiveTime::from_hms_opt(8, 0, 0).unwrap()),
            );
            d.set(
                "startDate",
                Kind::Date(NaiveDate::from_ymd_opt(2024, 1, 15).unwrap()),
            );
            d.set("lastUpdate", Kind::DateTime(hdt.clone()));
            d.set(
                "uri",
                Kind::Uri(Uri::new(format!("http://example.com/entity/{i}"))),
            );
            d
        })
        .collect();

    let mut col_names: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for row in &rows {
        for name in row.tag_names() {
            if seen.insert(name.to_string()) {
                col_names.push(name.to_string());
            }
        }
    }
    col_names.sort();
    let cols: Vec<HCol> = col_names.iter().map(|n| HCol::new(n.as_str())).collect();

    HGrid::from_parts(HDict::new(), cols, rows)
}

fn codec_benchmarks(c: &mut Criterion) {
    let grid = make_sample_grid(100);

    // Zinc encode
    let zinc = codec_for("text/zinc").unwrap();
    c.bench_function("zinc_encode_100_rows", |b| {
        b.iter(|| zinc.encode_grid(black_box(&grid)))
    });

    // Zinc decode
    let zinc_data = zinc.encode_grid(&grid).unwrap();
    c.bench_function("zinc_decode_100_rows", |b| {
        b.iter(|| zinc.decode_grid(black_box(&zinc_data)))
    });

    // JSON v4 encode/decode
    let json = codec_for("application/json").unwrap();
    c.bench_function("json4_encode_100_rows", |b| {
        b.iter(|| json.encode_grid(black_box(&grid)))
    });
    let json_data = json.encode_grid(&grid).unwrap();
    c.bench_function("json4_decode_100_rows", |b| {
        b.iter(|| json.decode_grid(black_box(&json_data)))
    });

    // Scalar encode/decode
    let number = Kind::Number(Number::new(72.5, Some("\u{00b0}F".into())));
    c.bench_function("zinc_encode_scalar", |b| {
        b.iter(|| zinc.encode_scalar(black_box(&number)))
    });

    let number_str = zinc.encode_scalar(&number).unwrap();
    c.bench_function("zinc_decode_scalar", |b| {
        b.iter(|| zinc.decode_scalar(black_box(&number_str)))
    });

    // Large grid (1000 rows)
    let grid_1000 = make_sample_grid(1000);

    c.bench_function("zinc_encode_1000_rows", |b| {
        b.iter(|| zinc.encode_grid(black_box(&grid_1000)))
    });

    let zinc_data_1000 = zinc.encode_grid(&grid_1000).unwrap();
    c.bench_function("zinc_decode_1000_rows", |b| {
        b.iter(|| zinc.decode_grid(black_box(&zinc_data_1000)))
    });

    c.bench_function("json4_encode_1000_rows", |b| {
        b.iter(|| json.encode_grid(black_box(&grid_1000)))
    });

    let json_data_1000 = json.encode_grid(&grid_1000).unwrap();
    c.bench_function("json4_decode_1000_rows", |b| {
        b.iter(|| json.decode_grid(black_box(&json_data_1000)))
    });

    // CSV encode (encode-only codec)
    let csv = codec_for("text/csv").unwrap();
    c.bench_function("csv_encode_1000_rows", |b| {
        b.iter(|| csv.encode_grid(black_box(&grid_1000)))
    });

    // Mixed-type codec roundtrip
    let mixed_grid = make_mixed_type_grid();
    c.bench_function("codec_roundtrip_mixed_types", |b| {
        b.iter(|| {
            let encoded = zinc.encode_grid(black_box(&mixed_grid)).unwrap();
            zinc.decode_grid(black_box(&encoded))
        })
    });
}

fn filter_benchmarks(c: &mut Criterion) {
    let entity = make_sample_entity(0);

    c.bench_function("filter_parse_simple", |b| {
        b.iter(|| filter::parse_filter(black_box("site")))
    });

    c.bench_function("filter_parse_complex", |b| {
        b.iter(|| {
            filter::parse_filter(black_box("site and equip and point and temp > 70\u{00b0}F"))
        })
    });

    let simple = filter::parse_filter("site").unwrap();
    c.bench_function("filter_eval_simple", |b| {
        b.iter(|| filter::matches(black_box(&simple), black_box(&entity), None))
    });

    let complex = filter::parse_filter("site and equip and point and temp > 70\u{00b0}F").unwrap();
    c.bench_function("filter_eval_complex", |b| {
        b.iter(|| filter::matches(black_box(&complex), black_box(&entity), None))
    });
}

fn graph_benchmarks(c: &mut Criterion) {
    // Build a graph with 1000 entities.
    let mut graph = EntityGraph::new();
    let mut site = HDict::new();
    site.set("id", Kind::Ref(HRef::from_val("site-1")));
    site.set("site", Kind::Marker);
    site.set("dis", Kind::Str("Main Site".into()));
    graph.add(site).unwrap();

    for i in 0..1000 {
        graph.add(make_sample_entity(i)).unwrap();
    }

    c.bench_function("graph_get_entity", |b| {
        b.iter(|| graph.get(black_box("p-500")))
    });

    c.bench_function("graph_filter_1000_entities", |b| {
        b.iter(|| graph.read(black_box("point and temp > 70\u{00b0}F"), 0))
    });

    c.bench_function("graph_add_entity", |b| {
        let mut g = EntityGraph::new();
        let mut s = HDict::new();
        s.set("id", Kind::Ref(HRef::from_val("site-1")));
        s.set("site", Kind::Marker);
        g.add(s).unwrap();
        let mut counter = 0usize;
        b.iter_with_setup(
            || {
                counter += 1;
                let mut d = HDict::new();
                d.set("id", Kind::Ref(HRef::from_val(format!("bench-{counter}"))));
                d.set("point", Kind::Marker);
                d.set("siteRef", Kind::Ref(HRef::from_val("site-1")));
                d
            },
            |entity| {
                let _ = g.add(entity);
            },
        );
    });

    // Bulk insert 1000 entities into a fresh graph
    c.bench_function("graph_add_1000_entities", |b| {
        b.iter(|| {
            let mut g = EntityGraph::new();
            let mut s = HDict::new();
            s.set("id", Kind::Ref(HRef::from_val("site-1")));
            s.set("site", Kind::Marker);
            g.add(s).unwrap();
            for i in 0..1000 {
                g.add(make_sample_entity(i)).unwrap();
            }
            g
        });
    });

    // Update an entity in the 1000-entity graph
    c.bench_function("graph_update_entity", |b| {
        b.iter(|| {
            let mut changes = HDict::new();
            changes.set("dis", Kind::Str("Updated Point 500".into()));
            changes.set(
                "temp",
                Kind::Number(Number::new(75.0, Some("\u{00b0}F".into()))),
            );
            graph.update(black_box("p-500"), changes)
        });
    });

    // Remove + re-add cycle on a 100-entity graph
    c.bench_function("graph_remove_entity", |b| {
        let mut g100 = EntityGraph::new();
        let mut s = HDict::new();
        s.set("id", Kind::Ref(HRef::from_val("site-1")));
        s.set("site", Kind::Marker);
        g100.add(s).unwrap();
        for i in 0..100 {
            g100.add(make_sample_entity(i)).unwrap();
        }
        b.iter(|| {
            let removed = g100.remove(black_box("p-50")).unwrap();
            g100.add(removed).unwrap();
        });
    });

    // Filter on a 10k entity graph
    c.bench_function("graph_filter_10000_entities", |b| {
        let mut g10k = EntityGraph::new();
        let mut s = HDict::new();
        s.set("id", Kind::Ref(HRef::from_val("site-1")));
        s.set("site", Kind::Marker);
        g10k.add(s).unwrap();
        for i in 0..10_000 {
            g10k.add(make_sample_entity(i)).unwrap();
        }
        b.iter(|| g10k.read(black_box("point and temp > 70\u{00b0}F"), 0));
    });

    // Query changelog at midpoint version
    c.bench_function("graph_changes_since", |b| {
        let midpoint = graph.version() / 2;
        b.iter(|| graph.changes_since(black_box(midpoint)));
    });

    // SharedGraph concurrent read/write benchmark
    c.bench_function("shared_graph_concurrent_rw", |b| {
        b.iter(|| {
            use std::thread;

            let sg = SharedGraph::new(EntityGraph::new());
            let mut s = HDict::new();
            s.set("id", Kind::Ref(HRef::from_val("site-1")));
            s.set("site", Kind::Marker);
            sg.add(s).unwrap();
            // Pre-populate with some entities
            for i in 0..100 {
                sg.add(make_sample_entity(i)).unwrap();
            }

            let mut handles = Vec::new();

            // 4 reader threads, each doing 10 reads
            for _ in 0..4 {
                let reader = sg.clone();
                handles.push(thread::spawn(move || {
                    for j in 0..10 {
                        let key = format!("p-{}", j * 10);
                        black_box(reader.get(&key));
                    }
                }));
            }

            // 1 writer thread, doing 10 writes
            let writer = sg.clone();
            handles.push(thread::spawn(move || {
                for j in 0..10 {
                    let mut d = HDict::new();
                    d.set("id", Kind::Ref(HRef::from_val(format!("w-{j}"))));
                    d.set("point", Kind::Marker);
                    d.set("siteRef", Kind::Ref(HRef::from_val("site-1")));
                    let _ = writer.add(d);
                }
            }));

            for h in handles {
                h.join().unwrap();
            }
        });
    });
}

fn ontology_benchmarks(c: &mut Criterion) {
    let ns = DefNamespace::load_standard().unwrap();

    c.bench_function("ontology_load_standard", |b| {
        b.iter(DefNamespace::load_standard)
    });

    let mut entity = HDict::new();
    entity.set("id", Kind::Ref(HRef::from_val("ahu-1")));
    entity.set("ahu", Kind::Marker);
    entity.set("equip", Kind::Marker);

    c.bench_function("ontology_fits_check", |b| {
        b.iter(|| ns.fits(black_box(&entity), black_box("ahu")))
    });

    c.bench_function("ontology_is_subtype", |b| {
        b.iter(|| ns.is_a(black_box("ahu"), black_box("equip")))
    });

    c.bench_function("ontology_mandatory_tags", |b| {
        b.iter(|| ns.mandatory_tags(black_box("ahu")))
    });

    c.bench_function("ontology_validate_entity", |b| {
        b.iter(|| ns.validate_entity(black_box(&entity)))
    });
}

fn xeto_benchmarks(c: &mut Criterion) {
    let mut ns = DefNamespace::load_standard().unwrap();

    // Entity with ahu/equip markers for fitting tests
    let mut ahu_entity = HDict::new();
    ahu_entity.set("id", Kind::Ref(HRef::from_val("ahu-1")));
    ahu_entity.set("ahu", Kind::Marker);
    ahu_entity.set("equip", Kind::Marker);
    ahu_entity.set("dis", Kind::Str("Main AHU".into()));

    // Benchmark xeto::fitting::fits against "ahu" (resolved via DefNamespace)
    c.bench_function("xeto_fits_ahu", |b| {
        b.iter(|| xeto::fitting::fits(black_box(&ahu_entity), black_box("ahu"), &mut ns, None))
    });

    // Benchmark fits with a missing marker (should fail fast)
    let mut incomplete_entity = HDict::new();
    incomplete_entity.set("id", Kind::Ref(HRef::from_val("ahu-2")));
    incomplete_entity.set("ahu", Kind::Marker);
    // Missing "equip" marker

    c.bench_function("xeto_fits_missing_marker", |b| {
        b.iter(|| {
            xeto::fitting::fits(
                black_box(&incomplete_entity),
                black_box("ahu"),
                &mut ns,
                None,
            )
        })
    });

    // Benchmark fits_explain for detailed issue reporting
    c.bench_function("xeto_fits_explain", |b| {
        b.iter(|| {
            xeto::fitting::fits_explain(
                black_box(&incomplete_entity),
                black_box("ahu"),
                &mut ns,
                None,
            )
        })
    });

    // Simple entity fitting against "site" (fewer mandatory markers)
    let mut site_entity = HDict::new();
    site_entity.set("id", Kind::Ref(HRef::from_val("site-1")));
    site_entity.set("site", Kind::Marker);
    site_entity.set("dis", Kind::Str("Main Site".into()));

    c.bench_function("xeto_fits_site", |b| {
        b.iter(|| xeto::fitting::fits(black_box(&site_entity), black_box("site"), &mut ns, None))
    });

    // Benchmark effective_slots on specs from the loaded namespace
    let specs_map = ns.specs_map().clone();
    // Find a spec with slots for effective_slots benchmarking
    let spec_for_slots = specs_map.values().find(|s| !s.slots.is_empty()).cloned();

    if let Some(spec) = spec_for_slots {
        let spec_name = spec.qname.clone();
        c.bench_function("xeto_effective_slots", |b| {
            b.iter(|| spec.effective_slots(black_box(&specs_map)))
        });

        // Also benchmark effective_slots on a spec with a base chain
        let spec_with_base = specs_map
            .values()
            .find(|s| s.base.is_some() && !s.slots.is_empty())
            .cloned();

        if let Some(derived_spec) = spec_with_base {
            c.bench_function("xeto_effective_slots_inherited", |b| {
                b.iter(|| derived_spec.effective_slots(black_box(&specs_map)))
            });
        }

        // Log what spec we are benchmarking for traceability
        eprintln!("xeto_effective_slots: using spec '{}'", spec_name);
    }
}

fn build_hierarchy_graph() -> EntityGraph {
    let mut graph = EntityGraph::new();

    // 2 sites
    for i in 0..2 {
        let mut s = HDict::new();
        s.set("id", Kind::Ref(HRef::from_val(format!("site-{i}"))));
        s.set("site", Kind::Marker);
        s.set("dis", Kind::Str(format!("Site {i}")));
        graph.add(s).unwrap();
    }

    // 5 equips per site
    for si in 0..2 {
        for ei in 0..5 {
            let mut e = HDict::new();
            e.set("id", Kind::Ref(HRef::from_val(format!("equip-{si}-{ei}"))));
            e.set("equip", Kind::Marker);
            e.set("ahu", Kind::Marker);
            e.set("siteRef", Kind::Ref(HRef::from_val(format!("site-{si}"))));
            e.set("dis", Kind::Str(format!("AHU {si}-{ei}")));
            graph.add(e).unwrap();
        }
    }

    // 10 points per equip (100 total)
    for si in 0..2 {
        for ei in 0..5 {
            for pi in 0..10 {
                let mut p = HDict::new();
                p.set(
                    "id",
                    Kind::Ref(HRef::from_val(format!("point-{si}-{ei}-{pi}"))),
                );
                p.set("point", Kind::Marker);
                p.set("sensor", Kind::Marker);
                p.set("temp", Kind::Marker);
                p.set(
                    "equipRef",
                    Kind::Ref(HRef::from_val(format!("equip-{si}-{ei}"))),
                );
                p.set("siteRef", Kind::Ref(HRef::from_val(format!("site-{si}"))));
                p.set(
                    "curVal",
                    Kind::Number(Number::new(70.0 + pi as f64, Some("\u{00b0}F".into()))),
                );
                graph.add(p).unwrap();
            }
        }
    }

    graph
}

fn unit_benchmarks(c: &mut Criterion) {
    c.bench_function("unit_convert_temperature", |b| {
        b.iter(|| {
            kinds::convert(
                black_box(72.0),
                black_box("\u{00b0}F"),
                black_box("\u{00b0}C"),
            )
        })
    });

    c.bench_function("unit_compatible_check", |b| {
        b.iter(|| kinds::compatible(black_box("\u{00b0}F"), black_box("\u{00b0}C")))
    });

    c.bench_function("unit_quantity_lookup", |b| {
        b.iter(|| kinds::quantity(black_box("\u{00b0}F")))
    });
}

fn traversal_benchmarks(c: &mut Criterion) {
    let graph = build_hierarchy_graph();

    c.bench_function("graph_hierarchy_tree", |b| {
        b.iter(|| graph.hierarchy_tree(black_box("site-0"), black_box(3)))
    });

    c.bench_function("graph_classify", |b| {
        b.iter(|| graph.classify(black_box("equip-0-0")))
    });

    c.bench_function("graph_ref_chain", |b| {
        b.iter(|| {
            graph.ref_chain(
                black_box("point-0-0-0"),
                black_box(&["equipRef", "siteRef"]),
            )
        })
    });

    c.bench_function("graph_children", |b| {
        b.iter(|| graph.children(black_box("site-0")))
    });

    c.bench_function("graph_site_for", |b| {
        b.iter(|| graph.site_for(black_box("point-0-0-0")))
    });

    c.bench_function("graph_equip_points", |b| {
        b.iter(|| graph.equip_points(black_box("equip-0-0"), None).unwrap())
    });
}

fn validation_benchmarks(c: &mut Criterion) {
    use haystack_core::ontology::validate_graph;

    let ns = DefNamespace::load_standard().unwrap();

    let mut graph = EntityGraph::new();
    let mut site = HDict::new();
    site.set("id", Kind::Ref(HRef::from_val("site-1")));
    site.set("site", Kind::Marker);
    site.set("dis", Kind::Str("Main Site".into()));
    graph.add(site).unwrap();
    for i in 0..1000 {
        graph.add(make_sample_entity(i)).unwrap();
    }

    c.bench_function("validate_graph_1000", |b| {
        b.iter(|| validate_graph(black_box(&graph), black_box(&ns)))
    });
}

/// Build a realistic Haystack graph with diverse entity types.
///
/// Structure per "campus" (repeats to reach target count):
///   1 site, 3 AHUs, 2 VAVs, 1 boiler, 1 meter, 1 weather station,
///   then ~10 points per equip with varying kinds (temp, pressure, flow, occ, cmd).
/// Total per campus ≈ 80 entities (8 parents + ~72 points).
fn build_realistic_graph(target: usize) -> EntityGraph {
    let mut graph = EntityGraph::new();
    let campuses = (target / 80).max(1);

    for c in 0..campuses {
        // Site
        let site_id = format!("site-{c}");
        let mut s = HDict::new();
        s.set("id", Kind::Ref(HRef::from_val(&site_id)));
        s.set("site", Kind::Marker);
        s.set("dis", Kind::Str(format!("Campus {c}")));
        s.set("geoCity", Kind::Str("Portland".into()));
        s.set(
            "area",
            Kind::Number(Number::new(
                50_000.0 + c as f64 * 1000.0,
                Some("ft²".into()),
            )),
        );
        graph.add(s).unwrap();

        // Equips: 3 AHU, 2 VAV, 1 boiler, 1 meter, 1 weather
        let equip_defs: Vec<(&str, &[&str])> = vec![
            ("ahu", &["equip", "ahu", "hvac"][..]),
            ("ahu", &["equip", "ahu", "hvac"]),
            ("ahu", &["equip", "ahu", "hvac"]),
            ("vav", &["equip", "vav", "hvac"]),
            ("vav", &["equip", "vav", "hvac"]),
            ("boiler", &["equip", "boiler", "hotWaterHeating"]),
            ("meter", &["equip", "meter", "elecMeter"]),
            ("weather", &["equip", "weatherStation"]),
        ];

        let mut equip_ids = Vec::new();
        for (ei, (prefix, tags)) in equip_defs.iter().enumerate() {
            let eid = format!("{prefix}-{c}-{ei}");
            let mut e = HDict::new();
            e.set("id", Kind::Ref(HRef::from_val(&eid)));
            for tag in *tags {
                e.set(*tag, Kind::Marker);
            }
            e.set("siteRef", Kind::Ref(HRef::from_val(&site_id)));
            e.set("dis", Kind::Str(format!("{prefix} {c}-{ei}")));
            graph.add(e).unwrap();
            equip_ids.push(eid);
        }

        // Points: ~9 per equip with varying kinds
        let point_kinds: &[(&str, &[&str], &str, f64)] = &[
            ("temp", &["sensor", "temp", "air"], "°F", 72.0),
            ("pressure", &["sensor", "pressure", "air"], "inH₂O", 1.2),
            ("flow", &["sensor", "flow", "air"], "cfm", 1500.0),
            ("occ", &["sensor", "occ"], "%", 85.0),
            ("damper", &["cmd", "damper"], "%", 50.0),
            ("speed", &["cmd", "speed", "fan"], "%", 75.0),
            ("sp", &["sp", "temp", "air"], "°F", 72.0),
            ("enable", &["cmd", "enable"], "", 1.0),
            ("alarm", &["sensor", "alarm"], "", 0.0),
        ];

        for (ei, eq_id) in equip_ids.iter().enumerate() {
            for (pi, (kind, tags, unit, base_val)) in point_kinds.iter().enumerate() {
                let pid = format!("pt-{c}-{ei}-{kind}-{pi}");
                let mut p = HDict::new();
                p.set("id", Kind::Ref(HRef::from_val(&pid)));
                p.set("point", Kind::Marker);
                for tag in *tags {
                    p.set(*tag, Kind::Marker);
                }
                p.set("kind", Kind::Str("Number".into()));
                p.set("equipRef", Kind::Ref(HRef::from_val(eq_id)));
                p.set("siteRef", Kind::Ref(HRef::from_val(&site_id)));
                let val = base_val + (pi as f64 * 0.5) + (c as f64 * 0.1);
                if unit.is_empty() {
                    p.set("curVal", Kind::Number(Number::unitless(val)));
                } else {
                    p.set(
                        "curVal",
                        Kind::Number(Number::new(val, Some((*unit).into()))),
                    );
                }
                graph.add(p).unwrap();
            }
        }
    }
    graph
}

fn graph_scale_benchmarks(c: &mut Criterion) {
    let mut g10k = build_realistic_graph(10_000);

    c.bench_function("graph_update_delta_10000", |b| {
        b.iter(|| {
            let mut changes = HDict::new();
            changes.set("dis", Kind::Str("Updated".into()));
            changes.set("curVal", Kind::Number(Number::new(99.0, Some("°F".into()))));
            let _ = g10k.update(black_box("pt-60-2-temp-0"), changes);
        });
    });

    c.bench_function("graph_filter_realistic_10000", |b| {
        b.iter(|| g10k.read(black_box("point and sensor and temp"), 0));
    });

    c.bench_function("graph_filter_compound_10000", |b| {
        b.iter(|| {
            g10k.read(
                black_box("point and sensor and (temp or pressure) and siteRef == @site-0"),
                0,
            )
        });
    });

    c.bench_function("graph_filter_range_10000", |b| {
        b.iter(|| g10k.read(black_box("point and curVal > 73°F"), 0));
    });

    c.bench_function("graph_freelist_recycle_1000", |b| {
        let mut g = EntityGraph::new();
        for i in 0..2000 {
            let mut d = HDict::new();
            d.set("id", Kind::Ref(HRef::from_val(format!("r-{i}"))));
            d.set("point", Kind::Marker);
            g.add(d).unwrap();
        }
        for i in 0..1000 {
            g.remove(&format!("r-{i}")).unwrap();
        }
        b.iter(|| {
            for i in 0..1000 {
                let mut d = HDict::new();
                d.set("id", Kind::Ref(HRef::from_val(format!("r-{i}"))));
                d.set("point", Kind::Marker);
                g.add(d).unwrap();
            }
            for i in 0..1000 {
                g.remove(&format!("r-{i}")).unwrap();
            }
        });
    });
}

fn trio_json3_benchmarks(c: &mut Criterion) {
    let grid = make_sample_grid(100);
    let grid_1000 = make_sample_grid(1000);

    // Trio encode/decode
    let trio = codec_for("text/trio").unwrap();
    c.bench_function("trio_encode_100_rows", |b| {
        b.iter(|| trio.encode_grid(black_box(&grid)))
    });
    let trio_data = trio.encode_grid(&grid).unwrap();
    c.bench_function("trio_decode_100_rows", |b| {
        b.iter(|| trio.decode_grid(black_box(&trio_data)))
    });

    // JSON v3 encode/decode
    let json3 = codec_for("application/json;v=3").unwrap();
    c.bench_function("json3_encode_100_rows", |b| {
        b.iter(|| json3.encode_grid(black_box(&grid)))
    });
    let json3_data = json3.encode_grid(&grid).unwrap();
    c.bench_function("json3_decode_100_rows", |b| {
        b.iter(|| json3.decode_grid(black_box(&json3_data)))
    });

    c.bench_function("json3_encode_1000_rows", |b| {
        b.iter(|| json3.encode_grid(black_box(&grid_1000)))
    });
    let json3_data_1000 = json3.encode_grid(&grid_1000).unwrap();
    c.bench_function("json3_decode_1000_rows", |b| {
        b.iter(|| json3.decode_grid(black_box(&json3_data_1000)))
    });
}

fn dict_grid_benchmarks(c: &mut Criterion) {
    // Dict creation and operations
    c.bench_function("dict_create_7_tags", |b| {
        b.iter(|| make_sample_entity(black_box(42)))
    });

    let dict = make_sample_entity(0);
    c.bench_function("dict_get_tag", |b| b.iter(|| dict.get(black_box("temp"))));

    c.bench_function("dict_has_tag", |b| b.iter(|| dict.has(black_box("point"))));

    c.bench_function("dict_sorted_tags", |b| b.iter(|| dict.sorted_tags()));

    let mut d1 = HDict::new();
    d1.set("a", Kind::Str("hello".into()));
    d1.set("b", Kind::Number(Number::unitless(42.0)));
    let mut d2 = HDict::new();
    d2.set("b", Kind::Str("world".into()));
    d2.set("c", Kind::Marker);
    c.bench_function("dict_merge", |b| {
        b.iter(|| {
            let mut target = d1.clone();
            target.merge(black_box(&d2));
            target
        })
    });

    // Grid construction from entities
    let graph = build_hierarchy_graph();
    c.bench_function("graph_to_grid", |b| b.iter(|| graph.to_grid(black_box(""))));

    c.bench_function("graph_to_grid_filtered", |b| {
        b.iter(|| graph.to_grid(black_box("point and sensor")))
    });

    // from_grid: loading entities from a grid
    let grid = graph.to_grid("").unwrap();
    c.bench_function("graph_from_grid_112", |b| {
        b.iter(|| EntityGraph::from_grid(black_box(&grid), None))
    });
}

fn auth_benchmarks(c: &mut Criterion) {
    use haystack_core::auth;

    c.bench_function("auth_derive_credentials", |b| {
        let salt = b"test_salt_value_";
        b.iter(|| {
            auth::derive_credentials(
                black_box("password123"),
                black_box(salt),
                black_box(1000), // reduced iterations for benchmarking
            )
        })
    });

    c.bench_function("auth_generate_nonce", |b| b.iter(auth::generate_nonce));

    c.bench_function("auth_client_first_message", |b| {
        b.iter(|| auth::client_first_message(black_box("admin")))
    });

    // Parse auth headers
    c.bench_function("auth_parse_bearer", |b| {
        b.iter(|| auth::parse_auth_header(black_box("BEARER authToken=abc123def456")))
    });

    c.bench_function("auth_parse_hello", |b| {
        b.iter(|| auth::parse_auth_header(black_box("HELLO username=YWRtaW4")))
    });
}

fn shared_graph_benchmarks(c: &mut Criterion) {
    let sg = SharedGraph::new(build_hierarchy_graph());

    c.bench_function("shared_graph_get", |b| {
        b.iter(|| sg.get(black_box("point-0-0-0")))
    });

    c.bench_function("shared_graph_read_filter", |b| {
        b.iter(|| sg.read_filter(black_box("point and sensor and temp"), 0))
    });

    c.bench_function("shared_graph_len", |b| b.iter(|| sg.len()));

    c.bench_function("shared_graph_changes_since", |b| {
        b.iter(|| sg.changes_since(black_box(0)))
    });
}

criterion_group!(
    benches,
    codec_benchmarks,
    trio_json3_benchmarks,
    filter_benchmarks,
    graph_benchmarks,
    dict_grid_benchmarks,
    shared_graph_benchmarks,
    ontology_benchmarks,
    xeto_benchmarks,
    auth_benchmarks,
    unit_benchmarks,
    traversal_benchmarks,
    validation_benchmarks,
    graph_scale_benchmarks,
);

criterion_main!(benches);

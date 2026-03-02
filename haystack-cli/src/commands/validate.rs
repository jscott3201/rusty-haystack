use std::fs;
use std::path::PathBuf;

use haystack_core::codecs::codec_for;
use haystack_core::graph::EntityGraph;
use haystack_core::ontology::{DefNamespace, validate_graph};
use haystack_core::xeto::load_xeto_with_deps;

pub fn run(file: &str, format: Option<&str>, xeto_dirs: &[String], report: bool) {
    let mime = format
        .map(|f| {
            match f {
                "zinc" => "text/zinc",
                "trio" => "text/trio",
                "json" | "json4" => "application/json",
                "json3" => "application/json;v=3",
                other => other,
            }
            .to_string()
        })
        .unwrap_or_else(|| detect_format(file));

    let codec = match codec_for(&mime) {
        Some(c) => c,
        None => {
            eprintln!("Error: unsupported format '{}'", mime);
            std::process::exit(1);
        }
    };

    let content = match fs::read_to_string(file) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error reading '{}': {}", file, e);
            std::process::exit(1);
        }
    };

    let grid = match codec.decode_grid(&content) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("Error decoding '{}': {}", file, e);
            std::process::exit(1);
        }
    };

    let mut ns = match DefNamespace::load_standard() {
        Ok(n) => n,
        Err(e) => {
            eprintln!("Error loading standard namespace: {}", e);
            std::process::exit(1);
        }
    };

    // Load custom Xeto libraries if specified
    if !xeto_dirs.is_empty() {
        let dirs: Vec<PathBuf> = xeto_dirs
            .iter()
            .map(|d| {
                std::fs::canonicalize(d).unwrap_or_else(|e| {
                    eprintln!("Error: invalid xeto directory '{}': {e}", d);
                    std::process::exit(1);
                })
            })
            .collect();
        match load_xeto_with_deps(&dirs, &mut ns) {
            Ok(loaded) => {
                for lib_name in &loaded {
                    eprintln!("Loaded custom library: {}", lib_name);
                }
            }
            Err(e) => {
                eprintln!("Error loading custom Xeto libraries: {}", e);
                std::process::exit(1);
            }
        }
    }

    let graph = match EntityGraph::from_grid(&grid, None) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("Error building graph: {}", e);
            std::process::exit(1);
        }
    };

    let vr = validate_graph(&graph, &ns);
    let s = &vr.summary;

    if report {
        // Full structured report
        println!("=== Validation Report ===");
        println!(
            "Validated {} entities: {} valid, {} warnings, {} errors, {} untyped ({:.1}% spec coverage)",
            s.total_entities,
            s.valid,
            s.with_warnings,
            s.with_errors,
            s.untyped,
            vr.spec_coverage * 100.0,
        );

        if !vr.entity_issues.is_empty() {
            println!("\n--- Entity Issues ---");
            let mut ids: Vec<&String> = vr.entity_issues.keys().collect();
            ids.sort();
            for id in ids {
                let issues = &vr.entity_issues[id];
                println!("  {}:", id);
                for issue in issues {
                    println!("    - {}", issue);
                }
            }
        }

        if !vr.dangling_refs.is_empty() {
            println!("\n--- Dangling References ---");
            for (entity_id, tag, missing) in &vr.dangling_refs {
                println!("  {} -> {}.{} (not found)", missing, entity_id, tag);
            }
        }

        if vr.entity_issues.is_empty() && vr.dangling_refs.is_empty() {
            println!("\nNo issues found.");
        }
    } else {
        // Summary line
        println!(
            "Validated {} entities: {} valid, {} warnings, {} errors, {} untyped ({:.1}% spec coverage)",
            s.total_entities,
            s.valid,
            s.with_warnings,
            s.with_errors,
            s.untyped,
            vr.spec_coverage * 100.0,
        );
    }

    if s.with_errors > 0 || !vr.dangling_refs.is_empty() {
        std::process::exit(1);
    }
}

fn detect_format(file: &str) -> String {
    if file.ends_with(".zinc") {
        "text/zinc".to_string()
    } else if file.ends_with(".trio") {
        "text/trio".to_string()
    } else if file.ends_with(".json") {
        "application/json".to_string()
    } else {
        "text/zinc".to_string()
    }
}

use std::fs;

use haystack_core::codecs::codec_for;
use haystack_core::graph::EntityGraph;
use haystack_core::ontology::DefNamespace;

pub fn run(file: &str, format: Option<&str>) {
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

    let ns = DefNamespace::load_standard().unwrap();
    let graph = match EntityGraph::from_grid(&grid, Some(ns)) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("Error building graph: {}", e);
            std::process::exit(1);
        }
    };

    let issues = graph.validate();

    if issues.is_empty() {
        println!(
            "Validation passed: {} entities, 0 issues",
            graph.len()
        );
    } else {
        println!(
            "Validation found {} issues in {} entities:",
            issues.len(),
            graph.len()
        );
        for issue in &issues {
            println!(
                "  {} [{}] {}",
                issue.entity.as_deref().unwrap_or("?"),
                issue.issue_type,
                issue.detail
            );
        }
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

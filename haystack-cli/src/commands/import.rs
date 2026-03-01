use std::fs;

use haystack_core::codecs::codec_for;
use haystack_core::graph::EntityGraph;
use haystack_core::ontology::DefNamespace;

pub fn run(file: &str, format: Option<&str>) {
    let mime = format
        .map(format_to_mime)
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

    // Load into graph with standard namespace
    let ns = DefNamespace::load_standard().unwrap();
    let graph = match EntityGraph::from_grid(&grid, Some(ns)) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("Error building graph: {}", e);
            std::process::exit(1);
        }
    };

    println!("Imported {} entities from '{}'", graph.len(), file);
    println!("Format: {}", mime);

    // Print tag summary
    // Count marker tags across entities
    let all_grid = graph.to_grid("").unwrap();
    let mut tag_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for row in &all_grid.rows {
        for name in row.tag_names() {
            *tag_counts.entry(name.to_string()).or_default() += 1;
        }
    }

    // Print top tags
    let mut tags: Vec<_> = tag_counts.into_iter().collect();
    tags.sort_by(|a, b| b.1.cmp(&a.1));
    println!("\nTag distribution:");
    for (tag, count) in tags.iter().take(15) {
        println!("  {:<20} {}", tag, count);
    }
}

fn format_to_mime(format: &str) -> String {
    match format {
        "zinc" => "text/zinc".to_string(),
        "trio" => "text/trio".to_string(),
        "json" | "json4" => "application/json".to_string(),
        "json3" => "application/json;v=3".to_string(),
        other => other.to_string(),
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
        "text/zinc".to_string() // default
    }
}

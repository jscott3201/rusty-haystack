use std::path::PathBuf;

use haystack_core::codecs::codec_for;
use haystack_core::graph::{EntityGraph, SharedGraph, SnapshotReader, SnapshotWriter};

pub fn run_snapshot(dir: &str, input: Option<&str>, format: Option<&str>) {
    let graph = match input {
        Some(file) => load_graph_from_file(file, format),
        None => {
            eprintln!("Error: --input is required for snapshot");
            std::process::exit(1);
        }
    };

    let dir = std::fs::canonicalize(dir).unwrap_or_else(|e| {
        eprintln!("Error: invalid snapshot directory: {e}");
        std::process::exit(1);
    });
    let writer = SnapshotWriter::new(dir, 10);
    let snap_path = match writer.write(&graph) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Error writing snapshot: {}", e);
            std::process::exit(1);
        }
    };

    let entity_count = graph.len();
    let version = graph.version();
    println!("Snapshot written: {}", snap_path.display());
    println!("  Entities: {}", entity_count);
    println!("  Graph version: {}", version);
}

pub fn run_restore(snapshot: &str, output: Option<&str>, format: Option<&str>) {
    let snap_path = PathBuf::from(snapshot);
    if !snap_path.exists() {
        eprintln!("Error: snapshot file not found: {}", snapshot);
        std::process::exit(1);
    }

    let graph = SharedGraph::new(EntityGraph::new());
    let meta = match SnapshotReader::load(&snap_path, &graph) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Error loading snapshot: {}", e);
            std::process::exit(1);
        }
    };

    println!("Restored snapshot: {}", snap_path.display());
    println!("  Format version: {}", meta.format_version);
    println!("  Entities: {}", meta.entity_count);
    println!("  Graph version: {}", meta.graph_version);
    println!("  Timestamp: {}", format_timestamp_nanos(meta.timestamp));

    if let Some(out_path) = output {
        let fmt = format.unwrap_or("zinc");
        let mime = format_to_mime(fmt);
        let codec = match codec_for(&mime) {
            Some(c) => c,
            None => {
                eprintln!("Error: unsupported format '{}'", fmt);
                std::process::exit(1);
            }
        };

        let grid = graph.read(|g| {
            g.to_grid("").unwrap_or_else(|e| {
                eprintln!("Error building grid: {}", e);
                std::process::exit(1);
            })
        });

        let encoded = match codec.encode_grid(&grid) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Error encoding to {}: {}", fmt, e);
                std::process::exit(1);
            }
        };

        if let Err(e) = std::fs::write(out_path, &encoded) {
            eprintln!("Error writing '{}': {}", out_path, e);
            std::process::exit(1);
        }
        println!("  Exported {} rows to '{}'", meta.entity_count, out_path);
    }
}

fn load_graph_from_file(file: &str, format: Option<&str>) -> SharedGraph {
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

    let content = match std::fs::read_to_string(file) {
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

    let eg = match EntityGraph::from_grid(&grid, None) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("Error building graph: {}", e);
            std::process::exit(1);
        }
    };

    SharedGraph::new(eg)
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
        "text/zinc".to_string()
    }
}

fn format_timestamp_nanos(nanos: i64) -> String {
    let secs = nanos / 1_000_000_000;
    let nsec = (nanos % 1_000_000_000) as u32;
    match std::time::UNIX_EPOCH.checked_add(std::time::Duration::new(secs as u64, nsec)) {
        Some(t) => {
            let elapsed = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
            format!("{}s since epoch", elapsed.as_secs())
        }
        None => format!("{} ns", nanos),
    }
}

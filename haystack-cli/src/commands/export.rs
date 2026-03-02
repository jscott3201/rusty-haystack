use std::fs;
use std::io::{self, Read};

use haystack_core::codecs::codec_for;

pub fn run(format: &str, output: Option<&str>, filter: Option<&str>) {
    let mime = format_to_mime(format);

    // Read from stdin
    let mut input = String::new();
    if let Err(e) = io::stdin().read_to_string(&mut input) {
        eprintln!("Error reading stdin: {}", e);
        std::process::exit(1);
    }

    // Try to decode as Zinc (default input format)
    let input_codec = codec_for("text/zinc").unwrap_or_else(|| {
        eprintln!("Error: zinc codec not available");
        std::process::exit(1);
    });
    let grid = match input_codec.decode_grid(&input) {
        Ok(g) => g,
        Err(_) => {
            // Try JSON
            let json_codec = codec_for("application/json").unwrap_or_else(|| {
                eprintln!("Error: json codec not available");
                std::process::exit(1);
            });
            match json_codec.decode_grid(&input) {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("Error: could not decode input as Zinc or JSON: {}", e);
                    std::process::exit(1);
                }
            }
        }
    };

    // If filter is specified, apply it
    let output_grid = if let Some(filter_expr) = filter {
        use haystack_core::graph::EntityGraph;
        let graph = EntityGraph::from_grid(&grid, None).unwrap_or_else(|e| {
            eprintln!("Error building graph: {}", e);
            std::process::exit(1);
        });
        graph.read(filter_expr, 0).unwrap_or_else(|e| {
            eprintln!("Error evaluating filter: {}", e);
            std::process::exit(1);
        })
    } else {
        grid
    };

    // HBF binary output
    if mime == "application/x-haystack-binary" {
        let bytes = haystack_core::codecs::encode_grid_binary(&output_grid).unwrap_or_else(|e| {
            eprintln!("Error encoding to HBF: {}", e);
            std::process::exit(1);
        });
        match output {
            Some(path) => {
                if let Err(e) = fs::write(path, &bytes) {
                    eprintln!("Error writing '{}': {}", path, e);
                    std::process::exit(1);
                }
                eprintln!(
                    "Exported {} rows to '{}' (HBF binary)",
                    output_grid.rows.len(),
                    path
                );
            }
            None => {
                use std::io::Write;
                if let Err(e) = io::stdout().write_all(&bytes) {
                    eprintln!("Error writing to stdout: {}", e);
                    std::process::exit(1);
                }
            }
        }
        return;
    }

    let codec = match codec_for(&mime) {
        Some(c) => c,
        None => {
            eprintln!("Error: unsupported format '{}'", format);
            std::process::exit(1);
        }
    };

    let encoded = match codec.encode_grid(&output_grid) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error encoding to {}: {}", format, e);
            std::process::exit(1);
        }
    };

    // Write output
    match output {
        Some(path) => {
            if let Err(e) = fs::write(path, &encoded) {
                eprintln!("Error writing '{}': {}", path, e);
                std::process::exit(1);
            }
            eprintln!("Exported {} rows to '{}'", output_grid.rows.len(), path);
        }
        None => {
            print!("{}", encoded);
        }
    }
}

fn format_to_mime(format: &str) -> String {
    match format {
        "zinc" => "text/zinc".to_string(),
        "trio" => "text/trio".to_string(),
        "json" | "json4" => "application/json".to_string(),
        "json3" => "application/json;v=3".to_string(),
        "hbf" | "binary" => "application/x-haystack-binary".to_string(),
        other => other.to_string(),
    }
}

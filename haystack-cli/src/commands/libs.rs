//! The `libs` command — list loaded libraries.

use haystack_core::ontology::DefNamespace;

/// Run the libs command: list all loaded libraries.
pub fn run() {
    let ns = match DefNamespace::load_standard() {
        Ok(ns) => ns,
        Err(e) => {
            eprintln!("Error loading namespace: {e}");
            std::process::exit(1);
        }
    };

    println!("{:<20} {:<10} {}", "Name", "Version", "Defs");
    println!("{}", "-".repeat(50));
    let mut libs: Vec<_> = ns.libs().values().collect();
    libs.sort_by_key(|l| &l.name);
    for lib in libs {
        println!("{:<20} {:<10} {}", lib.name, lib.version, lib.defs.len());
    }
}

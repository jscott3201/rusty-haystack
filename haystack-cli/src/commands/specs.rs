//! The `specs` command — list loaded Xeto specs.

use haystack_core::ontology::DefNamespace;

/// Run the specs command: list all specs, optionally filtered by library.
pub fn run(lib: Option<&str>) {
    let ns = match DefNamespace::load_standard() {
        Ok(ns) => ns,
        Err(e) => {
            eprintln!("Error loading namespace: {e}");
            std::process::exit(1);
        }
    };

    let specs = ns.specs(lib);
    if specs.is_empty() {
        println!("No specs loaded.");
        return;
    }

    println!("{:<30} {:<15} {:<30} Slots", "QName", "Lib", "Base");
    println!("{}", "-".repeat(80));
    let mut sorted: Vec<_> = specs;
    sorted.sort_by_key(|s| &s.qname);
    for spec in sorted {
        let base = spec.base.as_deref().unwrap_or("-");
        let slot_count = spec.slots.len();
        println!(
            "{:<30} {:<15} {:<30} {}",
            spec.qname, spec.lib, base, slot_count
        );
    }
}

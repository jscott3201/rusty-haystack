use haystack_core::ontology::DefNamespace;

pub fn run(def_name: Option<&str>) {
    let ns = DefNamespace::load_standard().unwrap();

    match def_name {
        Some(name) => {
            // Show info about a specific def
            if ns.contains(name) {
                println!("Def: {}", name);

                let supers = ns.supertypes(name);
                if !supers.is_empty() {
                    println!("  Supertypes: {}", supers.join(", "));
                }

                let subs = ns.subtypes(name);
                if !subs.is_empty() {
                    println!("  Direct subtypes: {}", subs.join(", "));
                }

                let mandatory = ns.mandatory_tags(name);
                if !mandatory.is_empty() {
                    let mut tags: Vec<&str> = mandatory.iter().map(|s| s.as_str()).collect();
                    tags.sort();
                    println!("  Mandatory tags: {}", tags.join(", "));
                }

                let tags_for = ns.tags_for(name);
                if !tags_for.is_empty() {
                    let mut tags: Vec<&str> = tags_for.iter().map(|s| s.as_str()).collect();
                    tags.sort();
                    println!("  All tags: {}", tags.join(", "));
                }
            } else {
                eprintln!("Def '{}' not found in standard library", name);
                std::process::exit(1);
            }
        }
        None => {
            // Show general info
            println!("Haystack Standard Library");
            println!("  Defs loaded: {}", ns.len());

            // Count by type
            let entity_types: Vec<String> = ns.subtypes("entity");
            println!("  Entity types: {}", entity_types.len());

            let equip_types: Vec<String> = ns.subtypes("equip");
            println!("  Equip subtypes: {}", equip_types.len());

            let point_types: Vec<String> = ns.subtypes("point");
            println!("  Point subtypes: {}", point_types.len());
        }
    }
}

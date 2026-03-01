use haystack_server::auth::users::hash_password;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Deserialize, Clone)]
struct UserEntry {
    password_hash: String,
    permissions: Vec<String>,
}

#[derive(Deserialize)]
struct UserConfig {
    #[serde(default)]
    users: HashMap<String, UserEntry>,
}

pub fn run_add(file: &str, username: &str, password: &str, permissions: &[String]) {
    let mut config = load_config(file);

    if config.users.contains_key(username) {
        eprintln!("User '{}' already exists", username);
        std::process::exit(1);
    }

    let hash = hash_password(password);
    config.users.insert(
        username.to_string(),
        UserEntry {
            password_hash: hash,
            permissions: permissions.to_vec(),
        },
    );

    save_config(file, &config);
    eprintln!("Added user '{}'", username);
}

pub fn run_delete(file: &str, username: &str) {
    let mut config = load_config(file);

    if config.users.remove(username).is_none() {
        eprintln!("User '{}' not found", username);
        std::process::exit(1);
    }

    save_config(file, &config);
    eprintln!("Deleted user '{}'", username);
}

pub fn run_list(file: &str) {
    let config = load_config(file);

    if config.users.is_empty() {
        println!("No users configured.");
        return;
    }

    println!("{:<20} PERMISSIONS", "USERNAME");
    println!("{:<20} -----------", "--------");
    let mut names: Vec<_> = config.users.keys().collect();
    names.sort();
    for name in names {
        let entry = &config.users[name];
        println!("{:<20} {}", name, entry.permissions.join(", "));
    }
}

pub fn run_update_password(file: &str, username: &str, password: &str) {
    let mut config = load_config(file);

    let entry = match config.users.get_mut(username) {
        Some(e) => e,
        None => {
            eprintln!("User '{}' not found", username);
            std::process::exit(1);
        }
    };

    entry.password_hash = hash_password(password);
    save_config(file, &config);
    eprintln!("Updated password for '{}'", username);
}

fn load_config(path: &str) -> UserConfig {
    match std::fs::read_to_string(path) {
        Ok(content) => toml::from_str(&content).unwrap_or_else(|e| {
            eprintln!("Error parsing '{}': {}", path, e);
            std::process::exit(1);
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => UserConfig {
            users: HashMap::new(),
        },
        Err(e) => {
            eprintln!("Error reading '{}': {}", path, e);
            std::process::exit(1);
        }
    }
}

fn save_config(path: &str, config: &UserConfig) {
    let mut out = String::new();
    let mut names: Vec<_> = config.users.keys().collect();
    names.sort();
    for name in names {
        let entry = &config.users[name];
        out.push_str(&format!("[users.{}]\n", name));
        out.push_str(&format!("password_hash = \"{}\"\n", entry.password_hash));
        out.push_str(&format!(
            "permissions = [{}]\n\n",
            entry
                .permissions
                .iter()
                .map(|p| format!("\"{}\"", p))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    std::fs::write(path, out).unwrap_or_else(|e| {
        eprintln!("Error writing '{}': {}", path, e);
        std::process::exit(1);
    });
}

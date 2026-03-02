//! TOML-based user store for SCRAM authentication.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::Deserialize;
use std::collections::HashMap;

use haystack_core::auth::{DEFAULT_ITERATIONS, ScramCredentials, derive_credentials};

/// A named role with a fixed set of permissions.
#[derive(Debug, Clone)]
pub struct Role {
    pub name: String,
    pub permissions: Vec<String>,
}

/// Return the built-in `admin` role (read, write, admin).
pub fn admin_role() -> Role {
    Role {
        name: "admin".to_string(),
        permissions: vec!["read".to_string(), "write".to_string(), "admin".to_string()],
    }
}

/// Return the built-in `operator` role (read, write).
pub fn operator_role() -> Role {
    Role {
        name: "operator".to_string(),
        permissions: vec!["read".to_string(), "write".to_string()],
    }
}

/// Return the built-in `viewer` role (read).
pub fn viewer_role() -> Role {
    Role {
        name: "viewer".to_string(),
        permissions: vec!["read".to_string()],
    }
}

/// Look up a built-in role by name.
///
/// Returns `None` if the name does not match any built-in role.
pub fn builtin_role(name: &str) -> Option<Role> {
    match name {
        "admin" => Some(admin_role()),
        "operator" => Some(operator_role()),
        "viewer" => Some(viewer_role()),
        _ => None,
    }
}

/// Top-level TOML user configuration.
#[derive(Deserialize)]
pub struct UserConfig {
    pub users: HashMap<String, UserEntry>,
}

/// A single user entry in the TOML config.
///
/// Supports two modes:
/// - **Role-based:** set `role` to a built-in role name (`"admin"`,
///   `"operator"`, `"viewer"`).
/// - **Direct permissions:** set `permissions` to an explicit list.
///
/// If both `role` and `permissions` are provided, `permissions` takes
/// precedence.
#[derive(Deserialize, Debug)]
pub struct UserEntry {
    /// Password hash in the format: `"base64(salt):iterations:base64(stored_key):base64(server_key)"`.
    pub password_hash: String,
    /// Optional role name that maps to a built-in role's permissions.
    pub role: Option<String>,
    /// Explicit list of permissions: `"read"`, `"write"`, `"admin"`.
    /// Takes precedence over `role` when both are present.
    pub permissions: Option<Vec<String>>,
}

/// Resolve the effective permissions for a user entry.
///
/// Resolution order:
/// 1. If `permissions` is set, use it directly.
/// 2. If `role` is set, look up the built-in role.
/// 3. Otherwise return an empty list.
pub fn resolve_permissions(entry: &UserEntry) -> Vec<String> {
    if let Some(ref perms) = entry.permissions {
        return perms.clone();
    }
    if let Some(ref role_name) = entry.role
        && let Some(role) = builtin_role(role_name)
    {
        return role.permissions;
    }
    Vec::new()
}

/// Parsed user record ready for authentication.
pub struct UserRecord {
    pub credentials: ScramCredentials,
    pub permissions: Vec<String>,
}

/// Parse a password hash string into SCRAM credentials.
///
/// Format: `"base64(salt):iterations:base64(stored_key):base64(server_key)"`.
pub fn parse_password_hash(hash: &str) -> Result<ScramCredentials, String> {
    let parts: Vec<&str> = hash.split(':').collect();
    if parts.len() != 4 {
        return Err(format!(
            "expected 4 colon-separated fields, got {}",
            parts.len()
        ));
    }

    let salt = BASE64
        .decode(parts[0])
        .map_err(|e| format!("invalid base64 salt: {e}"))?;
    let iterations: u32 = parts[1]
        .parse()
        .map_err(|e| format!("invalid iterations: {e}"))?;
    let stored_key = BASE64
        .decode(parts[2])
        .map_err(|e| format!("invalid base64 stored_key: {e}"))?;
    let server_key = BASE64
        .decode(parts[3])
        .map_err(|e| format!("invalid base64 server_key: {e}"))?;

    Ok(ScramCredentials {
        salt,
        iterations,
        stored_key,
        server_key,
    })
}

/// Create a password hash string from a plaintext password.
///
/// Generates a random 16-byte salt and uses `DEFAULT_ITERATIONS`.
/// Returns the hash in the format accepted by [`parse_password_hash`].
pub fn hash_password(password: &str) -> String {
    let mut salt = [0u8; 16];
    use rand::RngExt;
    rand::rng().fill(&mut salt);

    let creds = derive_credentials(password, &salt, DEFAULT_ITERATIONS);

    format!(
        "{}:{}:{}:{}",
        BASE64.encode(&creds.salt),
        creds.iterations,
        BASE64.encode(&creds.stored_key),
        BASE64.encode(&creds.server_key),
    )
}

/// Load user records from a TOML configuration file path.
pub fn load_users_from_toml(path: &str) -> Result<HashMap<String, UserRecord>, String> {
    let content =
        std::fs::read_to_string(path).map_err(|e| format!("failed to read {path}: {e}"))?;
    load_users_from_str(&content)
}

/// Load user records from TOML content string.
pub fn load_users_from_str(content: &str) -> Result<HashMap<String, UserRecord>, String> {
    let config: UserConfig =
        toml::from_str(content).map_err(|e| format!("TOML parse error: {e}"))?;

    let mut records = HashMap::new();
    for (username, entry) in config.users {
        let credentials = parse_password_hash(&entry.password_hash)?;
        let permissions = resolve_permissions(&entry);
        records.insert(
            username,
            UserRecord {
                credentials,
                permissions,
            },
        );
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_and_parse_roundtrip() {
        let hash_str = hash_password("testpassword");
        let creds = parse_password_hash(&hash_str).unwrap();
        assert_eq!(creds.iterations, DEFAULT_ITERATIONS);
        assert_eq!(creds.stored_key.len(), 32);
        assert_eq!(creds.server_key.len(), 32);
    }

    #[test]
    fn parse_invalid_format() {
        assert!(parse_password_hash("not:enough:parts").is_err());
        assert!(parse_password_hash("").is_err());
    }

    #[test]
    fn load_users_direct_permissions() {
        let hash = hash_password("s3cret");
        let toml_str = format!(
            r#"
[users.admin]
password_hash = "{hash}"
permissions = ["read", "write", "admin"]

[users.viewer]
password_hash = "{hash}"
permissions = ["read"]
"#
        );

        let records = load_users_from_str(&toml_str).unwrap();
        assert_eq!(records.len(), 2);
        assert!(records.contains_key("admin"));
        assert!(records.contains_key("viewer"));
        assert_eq!(records["admin"].permissions, vec!["read", "write", "admin"]);
        assert_eq!(records["viewer"].permissions, vec!["read"]);
    }

    #[test]
    fn load_users_role_based() {
        let hash = hash_password("s3cret");
        let toml_str = format!(
            r#"
[users.alice]
password_hash = "{hash}"
role = "admin"

[users.bob]
password_hash = "{hash}"
role = "operator"

[users.carol]
password_hash = "{hash}"
role = "viewer"
"#
        );

        let records = load_users_from_str(&toml_str).unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(records["alice"].permissions, vec!["read", "write", "admin"]);
        assert_eq!(records["bob"].permissions, vec!["read", "write"]);
        assert_eq!(records["carol"].permissions, vec!["read"]);
    }

    #[test]
    fn load_users_mixed_role_and_permissions() {
        let hash = hash_password("s3cret");
        let toml_str = format!(
            r#"
[users.role_user]
password_hash = "{hash}"
role = "viewer"

[users.perm_user]
password_hash = "{hash}"
permissions = ["read", "write"]
"#
        );

        let records = load_users_from_str(&toml_str).unwrap();
        assert_eq!(records["role_user"].permissions, vec!["read"]);
        assert_eq!(records["perm_user"].permissions, vec!["read", "write"]);
    }

    #[test]
    fn permissions_override_role() {
        let hash = hash_password("s3cret");
        let toml_str = format!(
            r#"
[users.override_user]
password_hash = "{hash}"
role = "viewer"
permissions = ["read", "write", "admin"]
"#
        );

        let records = load_users_from_str(&toml_str).unwrap();
        // permissions takes precedence over role
        assert_eq!(
            records["override_user"].permissions,
            vec!["read", "write", "admin"]
        );
    }

    #[test]
    fn unknown_role_gives_empty_permissions() {
        let hash = hash_password("s3cret");
        let toml_str = format!(
            r#"
[users.mystery]
password_hash = "{hash}"
role = "superuser"
"#
        );

        let records = load_users_from_str(&toml_str).unwrap();
        assert!(records["mystery"].permissions.is_empty());
    }

    #[test]
    fn no_role_no_permissions_gives_empty() {
        let hash = hash_password("s3cret");
        let toml_str = format!(
            r#"
[users.bare]
password_hash = "{hash}"
"#
        );

        let records = load_users_from_str(&toml_str).unwrap();
        assert!(records["bare"].permissions.is_empty());
    }

    #[test]
    fn resolve_permissions_direct() {
        let entry = UserEntry {
            password_hash: String::new(),
            role: None,
            permissions: Some(vec!["read".to_string(), "write".to_string()]),
        };
        assert_eq!(resolve_permissions(&entry), vec!["read", "write"]);
    }

    #[test]
    fn resolve_permissions_role() {
        let entry = UserEntry {
            password_hash: String::new(),
            role: Some("operator".to_string()),
            permissions: None,
        };
        assert_eq!(resolve_permissions(&entry), vec!["read", "write"]);
    }

    #[test]
    fn resolve_permissions_both_prefers_permissions() {
        let entry = UserEntry {
            password_hash: String::new(),
            role: Some("admin".to_string()),
            permissions: Some(vec!["read".to_string()]),
        };
        // explicit permissions win
        assert_eq!(resolve_permissions(&entry), vec!["read"]);
    }

    #[test]
    fn builtin_roles_exist() {
        assert!(builtin_role("admin").is_some());
        assert!(builtin_role("operator").is_some());
        assert!(builtin_role("viewer").is_some());
        assert!(builtin_role("nonexistent").is_none());
    }
}

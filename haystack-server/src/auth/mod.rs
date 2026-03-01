//! Server-side authentication manager using SCRAM SHA-256.
//!
//! Manages user records, in-flight SCRAM handshakes, and active bearer
//! tokens.

pub mod users;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use hmac::{Hmac, Mac};
use parking_lot::RwLock;
use sha2::Sha256;
use uuid::Uuid;

use haystack_core::auth::{
    DEFAULT_ITERATIONS, ScramCredentials, ScramHandshake, derive_credentials, extract_client_nonce,
    format_auth_info, format_www_authenticate, generate_nonce, server_first_message,
    server_verify_final,
};

use users::{UserRecord, load_users_from_str, load_users_from_toml};

/// An authenticated user with associated permissions.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub username: String,
    pub permissions: Vec<String>,
}

/// Time-to-live for in-flight SCRAM handshakes.
const HANDSHAKE_TTL: Duration = Duration::from_secs(60);

/// Server-side authentication manager.
///
/// Holds user credentials, in-flight SCRAM handshakes, and active
/// bearer tokens.
pub struct AuthManager {
    /// Username -> pre-computed SCRAM credentials + permissions.
    users: HashMap<String, UserRecord>,
    /// In-flight SCRAM handshakes: handshake_token -> (ScramHandshake, created_at).
    handshakes: RwLock<HashMap<String, (ScramHandshake, Instant)>>,
    /// Active bearer tokens: auth_token -> (AuthUser, created_at).
    tokens: RwLock<HashMap<String, (AuthUser, Instant)>>,
    /// Time-to-live for bearer tokens.
    token_ttl: Duration,
    /// Secret used to derive fake SCRAM challenges for unknown users,
    /// preventing username enumeration attacks.
    server_secret: [u8; 32],
}

impl AuthManager {
    /// Create a new AuthManager with the given user records and token TTL.
    pub fn new(users: HashMap<String, UserRecord>, token_ttl: Duration) -> Self {
        let mut server_secret = [0u8; 32];
        rand::Rng::fill(&mut rand::rng(), &mut server_secret);
        Self {
            users,
            handshakes: RwLock::new(HashMap::new()),
            tokens: RwLock::new(HashMap::new()),
            token_ttl,
            server_secret,
        }
    }

    /// Create an AuthManager with no users (auth effectively disabled).
    pub fn empty() -> Self {
        Self::new(HashMap::new(), Duration::from_secs(3600))
    }

    /// Builder method to configure the token TTL.
    pub fn with_token_ttl(mut self, duration: Duration) -> Self {
        self.token_ttl = duration;
        self
    }

    /// Create an AuthManager from a TOML file.
    pub fn from_toml(path: &str) -> Result<Self, String> {
        let users = load_users_from_toml(path)?;
        Ok(Self::new(users, Duration::from_secs(3600)))
    }

    /// Create an AuthManager from TOML content string.
    pub fn from_toml_str(content: &str) -> Result<Self, String> {
        let users = load_users_from_str(content)?;
        Ok(Self::new(users, Duration::from_secs(3600)))
    }

    /// Returns true if authentication is enabled (there are registered users).
    pub fn is_enabled(&self) -> bool {
        !self.users.is_empty()
    }

    /// Derive deterministic fake SCRAM credentials for an unknown username.
    ///
    /// Uses HMAC(server_secret, username) so the same unknown username always
    /// produces the same salt, making the response indistinguishable from a
    /// real user's challenge to an outside observer.
    fn fake_credentials(&self, username: &str) -> ScramCredentials {
        let mut mac = <Hmac<Sha256>>::new_from_slice(&self.server_secret)
            .expect("HMAC accepts keys of any size");
        mac.update(username.as_bytes());
        let fake_salt = mac.finalize().into_bytes();

        // Derive credentials using a throwaway password; the handshake will
        // always fail at the `handle_scram` step because the attacker does
        // not know a valid password, but the challenge itself looks normal.
        derive_credentials("", &fake_salt, DEFAULT_ITERATIONS)
    }

    /// Handle a HELLO request: look up user, create SCRAM handshake.
    ///
    /// `client_first_b64` is the optional base64-encoded client-first-message
    /// containing the client nonce. If absent, the server generates a nonce
    /// (but the handshake will fail if the client expects its own nonce).
    ///
    /// Returns the `WWW-Authenticate` header value for the 401 response.
    /// Unknown users receive a fake but plausible challenge to prevent
    /// username enumeration.
    pub fn handle_hello(
        &self,
        username: &str,
        client_first_b64: Option<&str>,
    ) -> Result<String, String> {
        let credentials = match self.users.get(username) {
            Some(user_record) => user_record.credentials.clone(),
            None => self.fake_credentials(username),
        };

        // Extract client nonce from client-first-message, or generate one
        let client_nonce = match client_first_b64 {
            Some(data) => {
                extract_client_nonce(data).map_err(|e| format!("invalid client-first data: {e}"))?
            }
            None => generate_nonce(),
        };

        // Create server-first-message
        let (handshake, server_first_b64) =
            server_first_message(username, &client_nonce, &credentials);

        // Lazy cleanup: remove expired handshakes before inserting.
        {
            let now = Instant::now();
            self.handshakes
                .write()
                .retain(|_, (_, created)| now.duration_since(*created) < HANDSHAKE_TTL);
        }

        // Store handshake with a unique token and timestamp.
        let handshake_token = Uuid::new_v4().to_string();
        self.handshakes
            .write()
            .insert(handshake_token.clone(), (handshake, Instant::now()));

        // Format the WWW-Authenticate header
        let www_auth = format_www_authenticate(&handshake_token, "SHA-256", &server_first_b64);
        Ok(www_auth)
    }

    /// Handle a SCRAM request: verify client proof, issue auth token.
    ///
    /// Returns `(auth_token, authentication_info_header_value)`.
    pub fn handle_scram(
        &self,
        handshake_token: &str,
        data: &str,
    ) -> Result<(String, String), String> {
        // Remove the handshake (one-time use) and check expiry.
        let (handshake, created_at) = self
            .handshakes
            .write()
            .remove(handshake_token)
            .ok_or_else(|| "invalid or expired handshake token".to_string())?;
        if created_at.elapsed() > HANDSHAKE_TTL {
            return Err("handshake token expired".to_string());
        }

        let username = handshake.username.clone();

        // Verify client proof
        let server_sig = server_verify_final(&handshake, data)
            .map_err(|e| format!("SCRAM verification failed: {e}"))?;

        // Issue auth token
        let auth_token = Uuid::new_v4().to_string();

        // Look up permissions
        let permissions = self
            .users
            .get(&username)
            .map(|r| r.permissions.clone())
            .unwrap_or_default();

        // Store token -> (user, created_at) mapping
        self.tokens.write().insert(
            auth_token.clone(),
            (
                AuthUser {
                    username,
                    permissions,
                },
                Instant::now(),
            ),
        );

        // Format the server-final data (v=<server_signature>)
        let server_final_msg = format!("v={}", BASE64.encode(&server_sig));
        let server_final_b64 = BASE64.encode(server_final_msg.as_bytes());
        let auth_info = format_auth_info(&auth_token, &server_final_b64);

        Ok((auth_token, auth_info))
    }

    /// Validate a bearer token and return the associated user.
    ///
    /// Returns `None` if the token is unknown or has expired. Expired
    /// tokens are automatically removed.
    pub fn validate_token(&self, token: &str) -> Option<AuthUser> {
        // First, check with a read lock.
        {
            let tokens = self.tokens.read();
            match tokens.get(token) {
                Some((user, created_at)) => {
                    if created_at.elapsed() <= self.token_ttl {
                        return Some(user.clone());
                    }
                    // Token expired -- fall through to remove it.
                }
                None => return None,
            }
        }
        // Expired: remove under a write lock.
        self.tokens.write().remove(token);
        None
    }

    /// Remove a bearer token (logout / close).
    pub fn revoke_token(&self, token: &str) -> bool {
        self.tokens.write().remove(token).is_some()
    }

    /// Inject a token directly (for testing). The token is stamped with the
    /// current instant so it will not be considered expired.
    #[doc(hidden)]
    pub fn inject_token(&self, token: String, user: AuthUser) {
        self.tokens.write().insert(token, (user, Instant::now()));
    }

    /// Check whether a user has a required permission.
    pub fn check_permission(user: &AuthUser, required: &str) -> bool {
        // Admin has all permissions
        if user.permissions.contains(&"admin".to_string()) {
            return true;
        }
        user.permissions.contains(&required.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::users::hash_password;

    fn make_test_manager() -> AuthManager {
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
        AuthManager::from_toml_str(&toml_str).unwrap()
    }

    #[test]
    fn empty_manager_is_disabled() {
        let mgr = AuthManager::empty();
        assert!(!mgr.is_enabled());
    }

    #[test]
    fn manager_with_users_is_enabled() {
        let mgr = make_test_manager();
        assert!(mgr.is_enabled());
    }

    #[test]
    fn hello_unknown_user_returns_fake_challenge() {
        let mgr = make_test_manager();
        // Unknown users now get a plausible SCRAM challenge instead of an
        // error, preventing username enumeration.
        let result = mgr.handle_hello("nonexistent", None);
        assert!(result.is_ok());
        let www_auth = result.unwrap();
        assert!(www_auth.contains("SCRAM"));
        assert!(www_auth.contains("SHA-256"));
    }

    #[test]
    fn hello_known_user_succeeds() {
        let mgr = make_test_manager();
        let result = mgr.handle_hello("admin", None);
        assert!(result.is_ok());
        let www_auth = result.unwrap();
        assert!(www_auth.contains("SCRAM"));
        assert!(www_auth.contains("SHA-256"));
    }

    #[test]
    fn hello_known_and_unknown_users_look_similar() {
        let mgr = make_test_manager();
        let known = mgr.handle_hello("admin", None).unwrap();
        let unknown = mgr.handle_hello("nonexistent", None).unwrap();

        // Both responses must have the same structural format so that an
        // attacker cannot distinguish real from fake users.
        assert!(known.starts_with("SCRAM handshakeToken="));
        assert!(unknown.starts_with("SCRAM handshakeToken="));
        assert!(known.contains("hash=SHA-256"));
        assert!(unknown.contains("hash=SHA-256"));
        assert!(known.contains("data="));
        assert!(unknown.contains("data="));
    }

    #[test]
    fn fake_challenge_is_deterministic_per_username() {
        let mgr = make_test_manager();
        // The fake salt must be deterministic so that repeated HELLO requests
        // for the same unknown username produce consistent parameters.
        let creds1 = mgr.fake_credentials("ghost");
        let creds2 = mgr.fake_credentials("ghost");
        assert_eq!(creds1.salt, creds2.salt);
        assert_eq!(creds1.stored_key, creds2.stored_key);
        assert_eq!(creds1.server_key, creds2.server_key);

        // Different usernames produce different fake salts.
        let creds3 = mgr.fake_credentials("phantom");
        assert_ne!(creds1.salt, creds3.salt);
    }

    #[test]
    fn validate_token_returns_none_for_unknown() {
        let mgr = make_test_manager();
        assert!(mgr.validate_token("nonexistent-token").is_none());
    }

    #[test]
    fn check_permission_admin_has_all() {
        let user = AuthUser {
            username: "admin".to_string(),
            permissions: vec!["admin".to_string()],
        };
        assert!(AuthManager::check_permission(&user, "read"));
        assert!(AuthManager::check_permission(&user, "write"));
        assert!(AuthManager::check_permission(&user, "admin"));
    }

    #[test]
    fn check_permission_viewer_limited() {
        let user = AuthUser {
            username: "viewer".to_string(),
            permissions: vec!["read".to_string()],
        };
        assert!(AuthManager::check_permission(&user, "read"));
        assert!(!AuthManager::check_permission(&user, "write"));
        assert!(!AuthManager::check_permission(&user, "admin"));
    }

    #[test]
    fn revoke_token_returns_false_for_unknown() {
        let mgr = make_test_manager();
        assert!(!mgr.revoke_token("nonexistent-token"));
    }

    #[test]
    fn validate_token_succeeds_before_expiry() {
        let mgr = make_test_manager();
        // Manually insert a token with Instant::now() (fresh, not expired).
        let user = AuthUser {
            username: "admin".to_string(),
            permissions: vec!["admin".to_string()],
        };
        mgr.tokens
            .write()
            .insert("good-token".to_string(), (user, Instant::now()));

        assert!(mgr.validate_token("good-token").is_some());
    }

    #[test]
    fn validate_token_fails_after_expiry() {
        // Use a very short TTL so the token is already expired.
        let mgr = make_test_manager().with_token_ttl(Duration::from_secs(0));

        let user = AuthUser {
            username: "admin".to_string(),
            permissions: vec!["admin".to_string()],
        };
        // Insert a token that was created "now" -- with a 0s TTL it is
        // immediately expired.
        mgr.tokens
            .write()
            .insert("expired-token".to_string(), (user, Instant::now()));

        // Even though the token exists, it should be reported as expired.
        assert!(mgr.validate_token("expired-token").is_none());

        // The expired token should have been removed from the map.
        assert!(mgr.tokens.read().get("expired-token").is_none());
    }

    #[test]
    fn with_token_ttl_sets_custom_duration() {
        let mgr = AuthManager::empty().with_token_ttl(Duration::from_secs(120));
        assert_eq!(mgr.token_ttl, Duration::from_secs(120));
    }
}

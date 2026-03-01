//! SCRAM SHA-256 authentication primitives for the Haystack auth protocol.
//!
//! This module implements the cryptographic operations needed for SCRAM
//! (Salted Challenge Response Authentication Mechanism) with SHA-256 as
//! specified by the [Project Haystack auth spec](https://project-haystack.org/doc/docHaystack/Auth).
//!
//! It provides functions shared by both server and client implementations
//! for the three-phase handshake: HELLO, SCRAM challenge/response, and
//! BEARER token issuance.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use hmac::{Hmac, Mac};
use pbkdf2::pbkdf2_hmac;
use rand::Rng;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

/// Default PBKDF2 iteration count for SCRAM SHA-256.
pub const DEFAULT_ITERATIONS: u32 = 100_000;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that can occur during SCRAM authentication.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error("invalid auth header: {0}")]
    InvalidHeader(String),
    #[error("handshake failed: {0}")]
    HandshakeFailed(String),
    #[error("base64 decode error: {0}")]
    Base64Error(String),
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Pre-computed SCRAM credentials for a user (stored server-side).
#[derive(Debug, Clone)]
pub struct ScramCredentials {
    pub salt: Vec<u8>,
    pub iterations: u32,
    pub stored_key: Vec<u8>,
    pub server_key: Vec<u8>,
}

/// In-flight SCRAM handshake state held by the server between the
/// server-first-message and client-final-message exchanges.
#[derive(Debug, Clone)]
pub struct ScramHandshake {
    pub username: String,
    pub client_nonce: String,
    pub server_nonce: String,
    pub salt: Vec<u8>,
    pub iterations: u32,
    pub auth_message: String,
    pub server_signature: Vec<u8>,
    /// Stored key from credentials, needed to verify the client proof.
    stored_key: Vec<u8>,
}

/// Parsed Haystack `Authorization` header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthHeader {
    Hello {
        username: String,
    },
    Scram {
        handshake_token: String,
        data: String,
    },
    Bearer {
        auth_token: String,
    },
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Compute HMAC-SHA-256(key, msg).
fn hmac_sha256(key: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut mac =
        HmacSha256::new_from_slice(key).expect("HMAC accepts keys of any size");
    mac.update(msg);
    mac.finalize().into_bytes().to_vec()
}

/// Compute SHA-256(data).
fn sha256(data: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().to_vec()
}

/// XOR two equal-length byte slices.
fn xor_bytes(a: &[u8], b: &[u8]) -> Vec<u8> {
    assert_eq!(a.len(), b.len(), "XOR operands must be the same length");
    a.iter().zip(b.iter()).map(|(x, y)| x ^ y).collect()
}

/// PBKDF2-HMAC-SHA-256 key derivation, producing a 32-byte salted password.
fn pbkdf2_sha256(password: &[u8], salt: &[u8], iterations: u32) -> Vec<u8> {
    let mut salted_password = vec![0u8; 32];
    pbkdf2_hmac::<Sha256>(password, salt, iterations, &mut salted_password);
    salted_password
}

/// Derive (ClientKey, StoredKey, ServerKey) from a salted password.
fn derive_keys(salted_password: &[u8]) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let client_key = hmac_sha256(salted_password, b"Client Key");
    let stored_key = sha256(&client_key);
    let server_key = hmac_sha256(salted_password, b"Server Key");
    (client_key, stored_key, server_key)
}

/// Parse a `key=value` parameter from a SCRAM message segment.
fn parse_scram_param<'a>(segment: &'a str, prefix: &str) -> Result<&'a str, AuthError> {
    let trimmed = segment.trim();
    trimmed.strip_prefix(prefix).ok_or_else(|| {
        AuthError::HandshakeFailed(format!(
            "expected prefix '{}' but got '{}'",
            prefix, trimmed
        ))
    })
}

/// Build the client-first-message-bare: `n=<username>,r=<client_nonce>`.
fn make_client_first_bare(username: &str, client_nonce: &str) -> String {
    format!("n={},r={}", username, client_nonce)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Derive SCRAM credentials from a password (for user creation/storage).
///
/// Uses PBKDF2-HMAC-SHA-256 with the given salt and iteration count.
pub fn derive_credentials(
    password: &str,
    salt: &[u8],
    iterations: u32,
) -> ScramCredentials {
    let salted_password = pbkdf2_sha256(password.as_bytes(), salt, iterations);
    let (_client_key, stored_key, server_key) = derive_keys(&salted_password);
    ScramCredentials {
        salt: salt.to_vec(),
        iterations,
        stored_key,
        server_key,
    }
}

/// Generate a random nonce string (base64-encoded 18 random bytes).
pub fn generate_nonce() -> String {
    let mut bytes = [0u8; 18];
    rand::rng().fill(&mut bytes);
    BASE64.encode(bytes)
}

/// Client-side: Create the client-first-message data (base64-encoded).
///
/// Returns `(client_nonce, client_first_data_base64)`.
///
/// The client-first-message-bare is `n=<username>,r=<client_nonce>`.
/// The full message prepends the GS2 header `n,,` (no channel binding).
pub fn client_first_message(username: &str) -> (String, String) {
    let client_nonce = generate_nonce();
    let bare = make_client_first_bare(username, &client_nonce);
    let full = format!("n,,{}", bare);
    let encoded = BASE64.encode(full.as_bytes());
    (client_nonce, encoded)
}

/// Server-side: Create the server-first-message data and handshake state.
///
/// `username` is taken from the HELLO phase. `client_nonce_b64` is the raw
/// client nonce (as returned by [`client_first_message`]). `credentials` are
/// the pre-computed SCRAM credentials for this user.
///
/// Returns `(handshake_state, server_first_data_base64)`.
pub fn server_first_message(
    username: &str,
    client_nonce_b64: &str,
    credentials: &ScramCredentials,
) -> (ScramHandshake, String) {
    let server_nonce = generate_nonce();
    let combined_nonce = format!("{}{}", client_nonce_b64, server_nonce);
    let salt_b64 = BASE64.encode(&credentials.salt);

    // server-first-message: r=<combined>,s=<salt_b64>,i=<iterations>
    let server_first_msg = format!(
        "r={},s={},i={}",
        combined_nonce, salt_b64, credentials.iterations
    );

    // client-first-message-bare (includes username per SCRAM spec)
    let cfmb = make_client_first_bare(username, client_nonce_b64);

    // client-final-message-without-proof (anticipated)
    let client_final_without_proof = format!("c=biws,r={}", combined_nonce);

    // AuthMessage = client-first-bare "," server-first-msg "," client-final-without-proof
    let auth_message = format!("{},{},{}", cfmb, server_first_msg, client_final_without_proof);

    // Pre-compute server signature
    let server_signature = hmac_sha256(&credentials.server_key, auth_message.as_bytes());

    let server_first_b64 = BASE64.encode(server_first_msg.as_bytes());

    let handshake = ScramHandshake {
        username: username.to_string(),
        client_nonce: client_nonce_b64.to_string(),
        server_nonce,
        salt: credentials.salt.clone(),
        iterations: credentials.iterations,
        auth_message,
        server_signature,
        stored_key: credentials.stored_key.clone(),
    };

    (handshake, server_first_b64)
}

/// Client-side: Process server-first-message, produce client-final-message.
///
/// `username` is the same value originally passed to [`client_first_message`].
/// `password` is the user's plaintext password. `client_nonce` is the nonce
/// returned by [`client_first_message`]. `server_first_b64` is the base64
/// server-first-message data received from the server.
///
/// Returns `(client_final_data_base64, expected_server_signature)`.
pub fn client_final_message(
    password: &str,
    client_nonce: &str,
    server_first_b64: &str,
    username: &str,
) -> Result<(String, Vec<u8>), AuthError> {
    // Decode and parse server-first-message
    let server_first_bytes = BASE64
        .decode(server_first_b64)
        .map_err(|e| AuthError::Base64Error(e.to_string()))?;
    let server_first_msg = String::from_utf8(server_first_bytes)
        .map_err(|e| AuthError::HandshakeFailed(e.to_string()))?;

    // Expected format: r=<combined_nonce>,s=<salt_b64>,i=<iterations>
    let parts: Vec<&str> = server_first_msg.splitn(3, ',').collect();
    if parts.len() != 3 {
        return Err(AuthError::HandshakeFailed(
            "invalid server-first-message format".to_string(),
        ));
    }

    let combined_nonce = parse_scram_param(parts[0], "r=")?;
    let salt_b64 = parse_scram_param(parts[1], "s=")?;
    let iterations_str = parse_scram_param(parts[2], "i=")?;

    // The combined nonce must start with our client nonce
    if !combined_nonce.starts_with(client_nonce) {
        return Err(AuthError::HandshakeFailed(
            "combined nonce does not start with client nonce".to_string(),
        ));
    }

    let salt = BASE64
        .decode(salt_b64)
        .map_err(|e| AuthError::Base64Error(e.to_string()))?;
    let iterations: u32 = iterations_str.parse().map_err(|e: std::num::ParseIntError| {
        AuthError::HandshakeFailed(e.to_string())
    })?;

    // Key derivation
    let salted_password = pbkdf2_sha256(password.as_bytes(), &salt, iterations);
    let (client_key, stored_key, server_key) = derive_keys(&salted_password);

    // Build AuthMessage
    let cfmb = make_client_first_bare(username, client_nonce);
    let client_final_without_proof = format!("c=biws,r={}", combined_nonce);
    let auth_message = format!("{},{},{}", cfmb, server_first_msg, client_final_without_proof);

    // ClientSignature = HMAC(StoredKey, AuthMessage)
    let client_signature = hmac_sha256(&stored_key, auth_message.as_bytes());
    // ClientProof = ClientKey XOR ClientSignature
    let client_proof = xor_bytes(&client_key, &client_signature);
    // ServerSignature = HMAC(ServerKey, AuthMessage)
    let server_signature = hmac_sha256(&server_key, auth_message.as_bytes());

    // client-final-message: c=biws,r=<combined>,p=<proof_b64>
    let proof_b64 = BASE64.encode(&client_proof);
    let client_final_msg = format!("{},p={}", client_final_without_proof, proof_b64);
    let client_final_b64 = BASE64.encode(client_final_msg.as_bytes());

    Ok((client_final_b64, server_signature))
}

/// Server-side: Verify client-final-message and produce server signature.
///
/// Decodes the client-final-message, verifies the client proof against the
/// stored key in the handshake state, and returns the server signature for
/// the client to verify (sent as the `v=` field in server-final-message).
pub fn server_verify_final(
    handshake: &ScramHandshake,
    client_final_b64: &str,
) -> Result<Vec<u8>, AuthError> {
    // Decode client-final-message
    let client_final_bytes = BASE64
        .decode(client_final_b64)
        .map_err(|e| AuthError::Base64Error(e.to_string()))?;
    let client_final_msg = String::from_utf8(client_final_bytes)
        .map_err(|e| AuthError::HandshakeFailed(e.to_string()))?;

    // Expected format: c=biws,r=<combined_nonce>,p=<proof_b64>
    let parts: Vec<&str> = client_final_msg.splitn(3, ',').collect();
    if parts.len() != 3 {
        return Err(AuthError::HandshakeFailed(
            "invalid client-final-message format".to_string(),
        ));
    }

    // Validate channel binding
    let channel_binding = parse_scram_param(parts[0], "c=")?;
    if channel_binding != "biws" {
        return Err(AuthError::HandshakeFailed(
            "unexpected channel binding".to_string(),
        ));
    }

    // Validate combined nonce
    let combined_nonce = parse_scram_param(parts[1], "r=")?;
    let expected_combined = format!("{}{}", handshake.client_nonce, handshake.server_nonce);
    if combined_nonce != expected_combined {
        return Err(AuthError::HandshakeFailed("nonce mismatch".to_string()));
    }

    // Extract and decode client proof
    let proof_b64 = parse_scram_param(parts[2], "p=")?;
    let client_proof = BASE64
        .decode(proof_b64)
        .map_err(|e| AuthError::Base64Error(e.to_string()))?;

    // Verify the proof per RFC 5802:
    //   ClientSignature = HMAC(StoredKey, AuthMessage)
    //   RecoveredClientKey = ClientProof XOR ClientSignature
    //   Check: SHA-256(RecoveredClientKey) == StoredKey
    let client_signature = hmac_sha256(&handshake.stored_key, handshake.auth_message.as_bytes());
    let recovered_client_key = xor_bytes(&client_proof, &client_signature);
    let recovered_stored_key = sha256(&recovered_client_key);

    if recovered_stored_key.ct_eq(&handshake.stored_key).unwrap_u8() == 0 {
        return Err(AuthError::InvalidCredentials);
    }

    // Proof verified -- return server signature for the client to verify
    Ok(handshake.server_signature.clone())
}

/// Parse a Haystack `Authorization` header value.
///
/// Supported formats:
/// - `HELLO username=<base64(username)>`
/// - `SCRAM handshakeToken=<token>, data=<data>`
/// - `BEARER authToken=<token>`
pub fn parse_auth_header(header: &str) -> Result<AuthHeader, AuthError> {
    let header = header.trim();

    if let Some(rest) = header.strip_prefix("HELLO ") {
        let username_b64 = rest
            .trim()
            .strip_prefix("username=")
            .ok_or_else(|| AuthError::InvalidHeader("missing username= in HELLO".into()))?;
        let username_bytes = BASE64
            .decode(username_b64.trim())
            .map_err(|e| AuthError::Base64Error(e.to_string()))?;
        let username = String::from_utf8(username_bytes)
            .map_err(|e| AuthError::InvalidHeader(e.to_string()))?;
        Ok(AuthHeader::Hello { username })
    } else if let Some(rest) = header.strip_prefix("SCRAM ") {
        let mut handshake_token = None;
        let mut data = None;
        for part in rest.split(',') {
            let part = part.trim();
            if let Some(val) = part.strip_prefix("handshakeToken=") {
                handshake_token = Some(val.trim().to_string());
            } else if let Some(val) = part.strip_prefix("data=") {
                data = Some(val.trim().to_string());
            }
        }
        let handshake_token = handshake_token.ok_or_else(|| {
            AuthError::InvalidHeader("missing handshakeToken= in SCRAM".into())
        })?;
        let data = data.ok_or_else(|| {
            AuthError::InvalidHeader("missing data= in SCRAM".into())
        })?;
        Ok(AuthHeader::Scram { handshake_token, data })
    } else if let Some(rest) = header.strip_prefix("BEARER ") {
        let token = rest
            .trim()
            .strip_prefix("authToken=")
            .ok_or_else(|| AuthError::InvalidHeader("missing authToken= in BEARER".into()))?;
        Ok(AuthHeader::Bearer {
            auth_token: token.trim().to_string(),
        })
    } else {
        Err(AuthError::InvalidHeader(format!(
            "unrecognized auth scheme: {}",
            header
        )))
    }
}

/// Format a Haystack `WWW-Authenticate` header for a SCRAM challenge.
///
/// Produces: `SCRAM handshakeToken=<token>, hash=<hash>, data=<data_b64>`
pub fn format_www_authenticate(
    handshake_token: &str,
    hash: &str,
    data_b64: &str,
) -> String {
    format!(
        "SCRAM handshakeToken={}, hash={}, data={}",
        handshake_token, hash, data_b64
    )
}

/// Format a Haystack `Authentication-Info` header with the auth token.
///
/// Produces: `authToken=<token>, data=<data_b64>`
pub fn format_auth_info(auth_token: &str, data_b64: &str) -> String {
    format!("authToken={}, data={}", auth_token, data_b64)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_credentials() {
        let password = "pencil";
        let salt = b"random-salt-value";
        let iterations = 4096;

        let creds = derive_credentials(password, salt, iterations);

        // Fields are populated correctly
        assert_eq!(creds.salt, salt.to_vec());
        assert_eq!(creds.iterations, iterations);
        assert_eq!(creds.stored_key.len(), 32); // SHA-256 output length
        assert_eq!(creds.server_key.len(), 32);

        // Deterministic: same inputs produce same outputs
        let creds2 = derive_credentials(password, salt, iterations);
        assert_eq!(creds.stored_key, creds2.stored_key);
        assert_eq!(creds.server_key, creds2.server_key);

        // Different password yields different credentials
        let creds3 = derive_credentials("other", salt, iterations);
        assert_ne!(creds.stored_key, creds3.stored_key);
        assert_ne!(creds.server_key, creds3.server_key);
    }

    #[test]
    fn test_generate_nonce() {
        let n1 = generate_nonce();
        let n2 = generate_nonce();

        // Each call produces a unique nonce
        assert_ne!(n1, n2);

        // Valid base64 encoding of 18 bytes
        let decoded1 = BASE64.decode(&n1).expect("nonce must be valid base64");
        assert_eq!(decoded1.len(), 18);

        let decoded2 = BASE64.decode(&n2).expect("nonce must be valid base64");
        assert_eq!(decoded2.len(), 18);
    }

    #[test]
    fn test_parse_auth_header_hello() {
        let username = "user";
        let username_b64 = BASE64.encode(username.as_bytes());
        let header = format!("HELLO username={}", username_b64);

        let parsed = parse_auth_header(&header).unwrap();
        assert_eq!(
            parsed,
            AuthHeader::Hello {
                username: "user".to_string(),
            }
        );
    }

    #[test]
    fn test_parse_auth_header_scram() {
        let header = "SCRAM handshakeToken=abc123, data=c29tZWRhdGE=";
        let parsed = parse_auth_header(header).unwrap();
        assert_eq!(
            parsed,
            AuthHeader::Scram {
                handshake_token: "abc123".to_string(),
                data: "c29tZWRhdGE=".to_string(),
            }
        );
    }

    #[test]
    fn test_parse_auth_header_bearer() {
        let header = "BEARER authToken=mytoken123";
        let parsed = parse_auth_header(header).unwrap();
        assert_eq!(
            parsed,
            AuthHeader::Bearer {
                auth_token: "mytoken123".to_string(),
            }
        );
    }

    #[test]
    fn test_parse_auth_header_invalid() {
        // Unknown scheme
        assert!(parse_auth_header("UNKNOWN foo=bar").is_err());
        // HELLO missing username=
        assert!(parse_auth_header("HELLO foo=bar").is_err());
        // SCRAM missing data=
        assert!(parse_auth_header("SCRAM handshakeToken=abc").is_err());
        // BEARER missing authToken=
        assert!(parse_auth_header("BEARER token=abc").is_err());
        // Empty
        assert!(parse_auth_header("").is_err());
    }

    #[test]
    fn test_full_handshake() {
        // Simulate the complete HELLO -> SCRAM -> BEARER flow.
        let username = "testuser";
        let password = "s3cret";
        let salt = b"test-salt-12345";
        let iterations = 4096;

        // --- Server: pre-compute credentials (user registration) ---
        let credentials = derive_credentials(password, salt, iterations);

        // --- Client: HELLO phase ---
        let username_b64 = BASE64.encode(username.as_bytes());
        let hello_header = format!("HELLO username={}", username_b64);
        let parsed = parse_auth_header(&hello_header).unwrap();
        match &parsed {
            AuthHeader::Hello { username: u } => assert_eq!(u, username),
            _ => panic!("expected Hello variant"),
        }

        // --- Client: generate client-first-message ---
        let (client_nonce, _client_first_b64) = client_first_message(username);

        // --- Server: generate server-first-message ---
        let (handshake, server_first_b64) =
            server_first_message(username, &client_nonce, &credentials);

        // --- Server: format WWW-Authenticate header ---
        let www_auth =
            format_www_authenticate("handshake-token-xyz", "SHA-256", &server_first_b64);
        assert!(www_auth.contains("SCRAM"));
        assert!(www_auth.contains("SHA-256"));
        assert!(www_auth.contains("handshake-token-xyz"));

        // --- Client: process server-first, produce client-final ---
        let (client_final_b64, expected_server_sig) =
            client_final_message(password, &client_nonce, &server_first_b64, username)
                .unwrap();

        // --- Server: verify client-final ---
        let server_sig = server_verify_final(&handshake, &client_final_b64).unwrap();

        // Server signature should match what the client expects
        assert_eq!(server_sig, expected_server_sig);

        // --- Server: format Authentication-Info header ---
        let server_final_msg = format!("v={}", BASE64.encode(&server_sig));
        let server_final_b64 = BASE64.encode(server_final_msg.as_bytes());
        let auth_info = format_auth_info("auth-token-abc", &server_final_b64);
        assert!(auth_info.contains("authToken=auth-token-abc"));

        // --- Client: verify server signature from server-final ---
        let server_final_decoded = BASE64.decode(&server_final_b64).unwrap();
        let server_final_str = String::from_utf8(server_final_decoded).unwrap();
        let sig_b64 = server_final_str.strip_prefix("v=").unwrap();
        let received_server_sig = BASE64.decode(sig_b64).unwrap();
        assert_eq!(received_server_sig, expected_server_sig);
    }

    #[test]
    fn test_client_server_roundtrip() {
        // Full roundtrip using the public API functions.
        let username = "admin";
        let password = "correcthorsebatterystaple";
        let salt = b"unique-salt-value";
        let iterations = DEFAULT_ITERATIONS;

        // 1. Server: create credentials during user registration
        let credentials = derive_credentials(password, salt, iterations);

        // 2. Client: create client-first-message
        let (client_nonce, client_first_b64) = client_first_message(username);

        // Verify client-first is valid base64 and well-formed
        let client_first_decoded = BASE64.decode(&client_first_b64).unwrap();
        let client_first_str = String::from_utf8(client_first_decoded).unwrap();
        assert!(client_first_str.starts_with("n,,"));
        assert!(client_first_str.contains(&format!("r={}", client_nonce)));

        // 3. Server: create server-first-message
        let (handshake, server_first_b64) =
            server_first_message(username, &client_nonce, &credentials);

        // Verify server-first contains expected SCRAM fields
        let server_first_decoded = BASE64.decode(&server_first_b64).unwrap();
        let server_first_str = String::from_utf8(server_first_decoded).unwrap();
        assert!(server_first_str.starts_with("r="));
        assert!(server_first_str.contains(",s="));
        assert!(server_first_str.contains(",i="));
        assert!(server_first_str.contains(&client_nonce));

        // 4. Client: create client-final-message
        let (client_final_b64, expected_server_sig) =
            client_final_message(password, &client_nonce, &server_first_b64, username)
                .unwrap();

        // Verify client-final structure
        let client_final_decoded = BASE64.decode(&client_final_b64).unwrap();
        let client_final_str = String::from_utf8(client_final_decoded).unwrap();
        assert!(client_final_str.starts_with("c=biws,"));
        assert!(client_final_str.contains(",p="));

        // 5. Server: verify and get server signature
        let server_sig = server_verify_final(&handshake, &client_final_b64).unwrap();
        assert_eq!(server_sig, expected_server_sig);

        // 6. Wrong password: server rejects the proof
        let (wrong_final_b64, _) =
            client_final_message("wrongpassword", &client_nonce, &server_first_b64, username)
                .unwrap();
        let result = server_verify_final(&handshake, &wrong_final_b64);
        assert!(result.is_err());
        match result {
            Err(AuthError::InvalidCredentials) => {} // expected
            other => panic!("expected InvalidCredentials, got {:?}", other),
        }
    }

    #[test]
    fn test_format_www_authenticate() {
        let result = format_www_authenticate("tok123", "SHA-256", "c29tZQ==");
        assert_eq!(
            result,
            "SCRAM handshakeToken=tok123, hash=SHA-256, data=c29tZQ=="
        );
    }

    #[test]
    fn test_format_auth_info() {
        let result = format_auth_info("auth-tok", "ZGF0YQ==");
        assert_eq!(result, "authToken=auth-tok, data=ZGF0YQ==");
    }
}

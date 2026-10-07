//! SCRAM SHA-256 for the published Haystack HTTP authentication exchange.
//! Outer username/data use unpadded base64url; inner salt/proof/verifier use
//! padded standard Base64. Received SCRAM transcripts are never normalized.
use base64::{
    Engine,
    engine::general_purpose::{STANDARD as BASE64, URL_SAFE_NO_PAD as OUTER},
};
use hmac::{Hmac, KeyInit, Mac};
use pbkdf2::pbkdf2_hmac;
use rand::RngExt;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, Zeroizing};
type HmacSha256 = Hmac<Sha256>;

pub const DEFAULT_ITERATIONS: u32 = 100_000;
pub const MAX_CLIENT_ITERATIONS: u32 = 1_000_000;
pub const MAX_AUTH_HEADER_BYTES: usize = 8192;
pub const MAX_AUTH_DATA_BYTES: usize = 4096;
pub const MAX_USERNAME_BYTES: usize = 1024;
const MAX_NONCE_BYTES: usize = 1024;

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error("invalid auth header: {0}")]
    InvalidHeader(String),
    #[error("handshake failed: {0}")]
    HandshakeFailed(String),
    #[error("invalid message: {0}")]
    InvalidMessage(String),
    #[error("base64 decode error: {0}")]
    Base64Error(String),
}
fn invalid(message: &str) -> AuthError {
    AuthError::InvalidMessage(message.into())
}

/// Pre-computed credentials. Stored salt/key encodings remain standard Base64.
pub struct ScramCredentials {
    pub salt: Vec<u8>,
    pub iterations: u32,
    pub stored_key: Vec<u8>,
    pub server_key: Vec<u8>,
}
impl std::fmt::Debug for ScramCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ScramCredentials { [REDACTED] }")
    }
}
impl Drop for ScramCredentials {
    fn drop(&mut self) {
        self.stored_key.zeroize();
        self.server_key.zeroize();
    }
}

/// Server proof state, preserving the exact client-first-bare transcript.
pub struct ScramHandshake {
    pub username: String,
    pub client_nonce: String,
    pub server_nonce: String,
    pub salt: Vec<u8>,
    pub iterations: u32,
    pub auth_message: String,
    pub server_signature: Vec<u8>,
    stored_key: Vec<u8>,
}
impl std::fmt::Debug for ScramHandshake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ScramHandshake { [REDACTED] }")
    }
}
impl Drop for ScramHandshake {
    fn drop(&mut self) {
        self.stored_key.zeroize();
        self.server_signature.zeroize();
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum AuthHeader {
    Hello {
        username: String,
    },
    Scram {
        handshake_token: Option<String>,
        data: String,
    },
    Bearer {
        auth_token: String,
    },
}
impl std::fmt::Debug for AuthHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AuthHeader { [REDACTED] }")
    }
}

/// Compute HMAC-SHA-256(key, msg).
fn hmac_sha256(key: &[u8], msg: &[u8]) -> Vec<u8> {
    // HMAC-SHA256 accepts keys of any size per RFC 2104; this cannot fail.
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts keys of any size");
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
    // Both operands are always 32-byte SHA-256 outputs in SCRAM.
    // A length mismatch here indicates a programming bug, not a runtime condition.
    debug_assert_eq!(a.len(), b.len(), "XOR operands must be same length");
    a.iter().zip(b.iter()).map(|(x, y)| x ^ y).collect()
}

/// PBKDF2-HMAC-SHA-256 key derivation, producing a 32-byte salted password.
fn pbkdf2_sha256(password: &[u8], salt: &[u8], iterations: u32) -> Vec<u8> {
    let mut salted_password = vec![0u8; 32];
    pbkdf2_hmac::<Sha256>(password, salt, iterations, &mut salted_password);
    salted_password
    // Note: caller is responsible for zeroizing via Zeroize trait on Vec<u8>
}

/// Derive (ClientKey, StoredKey, ServerKey) from a salted password.
fn derive_keys(salted_password: &[u8]) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let client_key = hmac_sha256(salted_password, b"Client Key");
    let stored_key = sha256(&client_key);
    let server_key = hmac_sha256(salted_password, b"Server Key");
    (client_key, stored_key, server_key)
}

/// Derive stored credentials during explicit user provisioning.
pub fn derive_credentials(password: &str, salt: &[u8], iterations: u32) -> ScramCredentials {
    let mut salted_password = pbkdf2_sha256(password.as_bytes(), salt, iterations);
    let (mut _client_key, stored_key, server_key) = derive_keys(&salted_password);
    salted_password.zeroize();
    _client_key.zeroize();
    ScramCredentials {
        salt: salt.to_vec(),
        iterations,
        stored_key,
        server_key,
    }
}

/// Generate 144 bits of random nonce entropy (printable SCRAM nonce bytes).
pub fn generate_nonce() -> String {
    BASE64.encode(rand::rng().random::<[u8; 18]>())
}
fn escape_scram_username(username: &str) -> String {
    username.replace('=', "=3D").replace(',', "=2C")
}
fn make_client_first_bare(username: &str, nonce: &str) -> String {
    format!("n={},r={nonce}", escape_scram_username(username))
}

/// Create `(client_nonce, client_first_data)` with unpadded base64url data.
/// The exact returned data must be passed to `client_final_message`.
pub fn client_first_message(username: &str) -> (String, String) {
    let nonce = generate_nonce();
    let data = OUTER.encode(format!("n,,{}", make_client_first_bare(username, &nonce)));
    (nonce, data)
}

pub fn validate_username(username: &str) -> Result<(), AuthError> {
    if username.is_empty()
        || username.len() > MAX_USERNAME_BYTES
        || username.chars().any(char::is_control)
    {
        return Err(invalid("invalid username"));
    }
    Ok(())
}
/// Decode bounded outer data without accepting padding, whitespace or another alphabet.
pub fn decode_auth_data(data: &str) -> Result<String, AuthError> {
    if data.len() > MAX_AUTH_DATA_BYTES * 4 / 3 + 4 {
        return Err(invalid("auth data exceeds limit"));
    }
    let bytes = OUTER
        .decode(data)
        .map_err(|_| invalid("invalid base64url data"))?;
    if bytes.len() > MAX_AUTH_DATA_BYTES {
        return Err(invalid("decoded auth data exceeds limit"));
    }
    String::from_utf8(bytes).map_err(|_| invalid("invalid auth UTF-8"))
}
fn nonce_valid(nonce: &str) -> bool {
    !nonce.is_empty()
        && nonce.len() <= MAX_NONCE_BYTES
        && nonce
            .bytes()
            .all(|b| (0x21..=0x7e).contains(&b) && b != b',')
}
struct ClientFirst {
    username: String,
    nonce: String,
    bare: String,
}
fn parse_client_first(data: &str) -> Result<ClientFirst, AuthError> {
    let full = decode_auth_data(data)?;
    let bare = full
        .strip_prefix("n,,")
        .ok_or_else(|| invalid("unsupported GS2 header"))?;
    let (name, nonce) = bare
        .split_once(",r=")
        .ok_or_else(|| invalid("invalid client-first fields"))?;
    let escaped = name
        .strip_prefix("n=")
        .ok_or_else(|| invalid("invalid client-first username"))?;
    if !nonce_valid(nonce) || escaped.contains(',') {
        return Err(invalid("invalid client-first fields"));
    }
    let mut username = String::new();
    let mut rest = escaped;
    while let Some(index) = rest.find('=') {
        username.push_str(&rest[..index]);
        rest = &rest[index..];
        if let Some(tail) = rest.strip_prefix("=2C") {
            username.push(',');
            rest = tail;
        } else if let Some(tail) = rest.strip_prefix("=3D") {
            username.push('=');
            rest = tail;
        } else {
            return Err(invalid("invalid SCRAM username escape"));
        }
    }
    username.push_str(rest);
    validate_username(&username)?;
    Ok(ClientFirst {
        username,
        nonce: nonce.into(),
        bare: bare.into(),
    })
}

/// Verify a client-first message's exact username and retain its original bytes.
pub fn server_first_message(
    username: &str,
    client_first_data: &str,
    credentials: &ScramCredentials,
) -> Result<(ScramHandshake, String), AuthError> {
    validate_credentials(credentials)?;
    let first = parse_client_first(client_first_data)?;
    if first.username != username {
        return Err(AuthError::InvalidCredentials);
    }
    let server_nonce = generate_nonce();
    if first.nonce.len() + server_nonce.len() > MAX_NONCE_BYTES {
        return Err(invalid("nonce exceeds limit"));
    }
    let combined = format!("{}{server_nonce}", first.nonce);
    let message = format!(
        "r={combined},s={},i={}",
        BASE64.encode(&credentials.salt),
        credentials.iterations
    );
    let auth_message = format!("{},{message},c=biws,r={combined}", first.bare);
    let server_signature = hmac_sha256(&credentials.server_key, auth_message.as_bytes());
    Ok((
        ScramHandshake {
            username: username.into(),
            client_nonce: first.nonce,
            server_nonce,
            salt: credentials.salt.clone(),
            iterations: credentials.iterations,
            auth_message,
            server_signature,
            stored_key: credentials.stored_key.clone(),
        },
        OUTER.encode(message),
    ))
}

/// Validate stored credential structure without deriving a password.
pub fn validate_credentials(credentials: &ScramCredentials) -> Result<(), AuthError> {
    if credentials.salt.is_empty()
        || credentials.salt.len() > 1024
        || !(1..=MAX_CLIENT_ITERATIONS).contains(&credentials.iterations)
        || credentials.stored_key.len() != 32
        || credentials.server_key.len() != 32
    {
        return Err(invalid("invalid SCRAM credentials"));
    }
    Ok(())
}
struct ServerFirst {
    message: String,
    nonce: String,
    salt: Vec<u8>,
    iterations: u32,
}
fn parse_server_first(client_nonce: &str, data: &str) -> Result<ServerFirst, AuthError> {
    let message = decode_auth_data(data)?;
    let parts: Vec<_> = message.split(',').collect();
    if parts.len() != 3 {
        return Err(invalid("invalid server-first fields"));
    }
    let nonce = parts[0]
        .strip_prefix("r=")
        .ok_or_else(|| invalid("missing server nonce"))?;
    if !nonce_valid(nonce) || !nonce.starts_with(client_nonce) || nonce.len() <= client_nonce.len()
    {
        return Err(invalid("server nonce does not extend client nonce"));
    }
    let salt = parts[1]
        .strip_prefix("s=")
        .ok_or_else(|| invalid("missing salt"))?;
    if salt.len() > 1368 {
        return Err(invalid("salt exceeds limit"));
    }
    let salt = BASE64
        .decode(salt)
        .map_err(|_| invalid("invalid salt base64"))?;
    if salt.is_empty() || salt.len() > 1024 {
        return Err(invalid("invalid salt length"));
    }
    let iterations = parts[2]
        .strip_prefix("i=")
        .ok_or_else(|| invalid("missing iterations"))?;
    if !iterations.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid("invalid iterations"));
    }
    let iterations: u32 = iterations
        .parse()
        .map_err(|_| invalid("invalid iterations"))?;
    if !(1..=MAX_CLIENT_ITERATIONS).contains(&iterations) {
        return Err(invalid("iterations exceed bounds"));
    }
    Ok(ServerFirst {
        nonce: nonce.into(),
        message,
        salt,
        iterations,
    })
}
/// Validate a received challenge before scheduling bounded PBKDF2 work.
pub fn validate_server_first(
    client_first_data: &str,
    server_first_data: &str,
) -> Result<(), AuthError> {
    let first = parse_client_first(client_first_data)?;
    parse_server_first(&first.nonce, server_first_data).map(|_| ())
}
/// Derive the final proof from the exact client-first and server-first data.
pub fn client_final_message(
    password: &str,
    client_first_data: &str,
    server_first_data: &str,
) -> Result<(String, Vec<u8>), AuthError> {
    let first = parse_client_first(client_first_data)?;
    let server = parse_server_first(&first.nonce, server_first_data)?;
    let salted = Zeroizing::new(pbkdf2_sha256(
        password.as_bytes(),
        &server.salt,
        server.iterations,
    ));
    let (client_key, stored_key, server_key) = derive_keys(&salted);
    let client_key = Zeroizing::new(client_key);
    let stored_key = Zeroizing::new(stored_key);
    let server_key = Zeroizing::new(server_key);
    let without_proof = format!("c=biws,r={}", server.nonce);
    let message = format!("{},{},{without_proof}", first.bare, server.message);
    let signature = hmac_sha256(&stored_key, message.as_bytes());
    let proof = Zeroizing::new(xor_bytes(&client_key, &signature));
    let server_signature = hmac_sha256(&server_key, message.as_bytes());
    Ok((
        OUTER.encode(format!("{without_proof},p={}", BASE64.encode(&*proof))),
        server_signature,
    ))
}
/// Reject malformed proofs before XOR, then verify using constant-time comparison.
pub fn server_verify_final(handshake: &ScramHandshake, data: &str) -> Result<Vec<u8>, AuthError> {
    let message = decode_auth_data(data)?;
    let parts: Vec<_> = message.split(',').collect();
    if parts.len() != 3 || parts[0] != "c=biws" {
        return Err(invalid("invalid client-final fields"));
    }
    let nonce = parts[1]
        .strip_prefix("r=")
        .ok_or_else(|| invalid("missing final nonce"))?;
    if !bool::from(
        nonce
            .as_bytes()
            .ct_eq(format!("{}{}", handshake.client_nonce, handshake.server_nonce).as_bytes()),
    ) {
        return Err(invalid("nonce mismatch"));
    }
    let proof = parts[2]
        .strip_prefix("p=")
        .ok_or_else(|| invalid("missing proof"))?;
    if proof.len() != 44 {
        return Err(invalid("invalid proof length"));
    }
    let proof = BASE64
        .decode(proof)
        .map_err(|_| invalid("invalid proof base64"))?;
    if proof.len() != 32 {
        return Err(invalid("invalid proof length"));
    }
    let signature = hmac_sha256(&handshake.stored_key, handshake.auth_message.as_bytes());
    let recovered = Zeroizing::new(xor_bytes(&proof, &signature));
    if !bool::from(sha256(&recovered).ct_eq(&handshake.stored_key)) {
        return Err(AuthError::InvalidCredentials);
    }
    Ok(handshake.server_signature.clone())
}
/// Authenticate the final server verifier without accepting extensions or short signatures.
pub fn verify_server_final(data: &str, expected_signature: &[u8]) -> Result<(), AuthError> {
    let message = decode_auth_data(data)?;
    let signature = message
        .strip_prefix("v=")
        .ok_or_else(|| invalid("missing server verifier"))?;
    if signature.len() != 44 || expected_signature.len() != 32 {
        return Err(invalid("invalid verifier length"));
    }
    let signature = BASE64
        .decode(signature)
        .map_err(|_| invalid("invalid verifier base64"))?;
    if signature.len() != 32 || !bool::from(signature.ct_eq(expected_signature)) {
        return Err(AuthError::InvalidCredentials);
    }
    Ok(())
}
pub fn extract_client_nonce(data: &str) -> Result<String, AuthError> {
    Ok(parse_client_first(data)?.nonce)
}

/// RFC HTTP token syntax (used for opaque auth tokens, not decoded SCRAM fields).
pub fn is_auth_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}
/// Parse bounded case-insensitive HTTP auth parameter names. Values remain exact.
pub fn parse_auth_parameters(input: &str) -> Result<HashMap<String, &str>, AuthError> {
    if input.len() > MAX_AUTH_HEADER_BYTES {
        return Err(invalid("auth header exceeds limit"));
    }
    let mut values = HashMap::new();
    for part in input.split(',') {
        let (key, value) = part
            .trim()
            .split_once('=')
            .ok_or_else(|| invalid("malformed auth parameter"))?;
        let key = key.trim();
        let value = value.trim();
        if !is_auth_token(key)
            || !is_auth_token(value)
            || values.insert(key.to_ascii_lowercase(), value).is_some()
        {
            return Err(invalid("invalid or duplicate auth parameter"));
        }
    }
    Ok(values)
}
/// Parse credentials only; challenges are handled by the HTTP client's negotiation.
pub fn parse_auth_header(header: &str) -> Result<AuthHeader, AuthError> {
    if header.len() > MAX_AUTH_HEADER_BYTES {
        return Err(invalid("auth header exceeds limit"));
    }
    let (scheme, parameters) = header
        .trim()
        .split_once(char::is_whitespace)
        .ok_or_else(|| invalid("missing auth scheme"))?;
    let fields = parse_auth_parameters(parameters)?;
    let get = |name| {
        fields
            .get(name)
            .copied()
            .ok_or_else(|| invalid("missing auth parameter"))
    };
    if scheme.eq_ignore_ascii_case("HELLO") {
        if fields.len() != 1 {
            return Err(invalid("unexpected HELLO parameters"));
        }
        let username = decode_auth_data(get("username")?)?;
        validate_username(&username)?;
        Ok(AuthHeader::Hello { username })
    } else if scheme.eq_ignore_ascii_case("SCRAM") {
        if fields
            .keys()
            .any(|k| !["handshaketoken", "data", "hash"].contains(&k.as_str()))
            || fields.get("hash").is_some_and(|v| *v != "SHA-256")
        {
            return Err(invalid("unsupported SCRAM parameters"));
        }
        let data = get("data")?;
        decode_auth_data(data)?;
        Ok(AuthHeader::Scram {
            handshake_token: fields.get("handshaketoken").map(|v| (*v).into()),
            data: data.into(),
        })
    } else if scheme.eq_ignore_ascii_case("BEARER") {
        if fields.len() != 1 {
            return Err(invalid("unexpected BEARER parameters"));
        }
        Ok(AuthHeader::Bearer {
            auth_token: get("authtoken")?.into(),
        })
    } else {
        Err(invalid("unsupported auth scheme"))
    }
}
/// Format a SHA-256 discovery or server-first challenge. Tokens are response-scoped.
pub fn format_www_authenticate(handshake_token: Option<&str>, data: Option<&str>) -> String {
    let mut value = String::from("SCRAM hash=SHA-256");
    if let Some(token) = handshake_token {
        value.push_str(&format!(", handshakeToken={token}"));
    }
    if let Some(data) = data {
        value.push_str(&format!(", data={data}"));
    }
    value
}
pub fn format_auth_info(auth_token: &str, data: &str) -> String {
    format!("authToken={auth_token}, hash=SHA-256, data={data}")
}

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
        let username_b64 = OUTER.encode(username.as_bytes());
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
        let header = "SCRAM handshakeToken=abc123, data=c29tZWRhdGE";
        let parsed = parse_auth_header(header).unwrap();
        assert_eq!(
            parsed,
            AuthHeader::Scram {
                handshake_token: Some("abc123".to_string()),
                data: "c29tZWRhdGE".to_string(),
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
    fn escape_scram_username_escapes_comma_and_equals() {
        assert_eq!(escape_scram_username("a,b=c"), "a=2Cb=3Dc");
        assert_eq!(escape_scram_username("plain"), "plain");
        // '=' is escaped before ',' so the '=' in '=2C' is not double-escaped.
        assert_eq!(escape_scram_username("=,"), "=3D=2C");
    }

    #[test]
    fn extract_client_nonce_resists_username_injection() {
        // A username containing a literal "r=" must not hijack nonce extraction.
        let (client_nonce, client_first_b64) = client_first_message("evil,r=hijack");
        assert_eq!(
            extract_client_nonce(&client_first_b64).unwrap(),
            client_nonce
        );
    }

    #[test]
    fn scram_handshake_succeeds_with_comma_equals_username() {
        // Usernames with ',' and '=' complete the full handshake because both
        // client and server escape the username identically per RFC 5802.
        let username = "od,al=ice";
        let password = "s3cret";
        let salt = b"test-salt-12345";
        let iterations = 4096;

        let credentials = derive_credentials(password, salt, iterations);
        let (_client_nonce, client_first_b64) = client_first_message(username);
        let (handshake, server_first_b64) =
            server_first_message(username, &client_first_b64, &credentials).unwrap();
        let (client_final_b64, expected_server_sig) =
            client_final_message(password, &client_first_b64, &server_first_b64).unwrap();
        let server_sig = server_verify_final(&handshake, &client_final_b64).unwrap();
        assert_eq!(server_sig, expected_server_sig);
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
        let username_b64 = OUTER.encode(username.as_bytes());
        let hello_header = format!("HELLO username={}", username_b64);
        let parsed = parse_auth_header(&hello_header).unwrap();
        match &parsed {
            AuthHeader::Hello { username: u, .. } => assert_eq!(u, username),
            _ => panic!("expected Hello variant"),
        }

        // --- Client: generate client-first-message ---
        let (_client_nonce, client_first_b64) = client_first_message(username);

        // --- Server: generate server-first-message ---
        let (handshake, server_first_b64) =
            server_first_message(username, &client_first_b64, &credentials).unwrap();

        // --- Server: format WWW-Authenticate header ---
        let www_auth =
            format_www_authenticate(Some("handshake-token-xyz"), Some(&server_first_b64));
        assert!(www_auth.contains("SCRAM"));
        assert!(www_auth.contains("SHA-256"));
        assert!(www_auth.contains("handshake-token-xyz"));

        // --- Client: process server-first, produce client-final ---
        let (client_final_b64, expected_server_sig) =
            client_final_message(password, &client_first_b64, &server_first_b64).unwrap();

        // --- Server: verify client-final ---
        let server_sig = server_verify_final(&handshake, &client_final_b64).unwrap();

        // Server signature should match what the client expects
        assert_eq!(server_sig, expected_server_sig);

        // --- Server: format Authentication-Info header ---
        let server_final_msg = format!("v={}", BASE64.encode(&server_sig));
        let server_final_b64 = OUTER.encode(server_final_msg.as_bytes());
        let auth_info = format_auth_info("auth-token-abc", &server_final_b64);
        assert!(auth_info.contains("authToken=auth-token-abc"));

        // --- Client: verify server signature from server-final ---
        let server_final_decoded = OUTER.decode(&server_final_b64).unwrap();
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
        let client_first_decoded = OUTER.decode(&client_first_b64).unwrap();
        let client_first_str = String::from_utf8(client_first_decoded).unwrap();
        assert!(client_first_str.starts_with("n,,"));
        assert!(client_first_str.contains(&format!("r={}", client_nonce)));

        // 3. Server: create server-first-message
        let (handshake, server_first_b64) =
            server_first_message(username, &client_first_b64, &credentials).unwrap();

        // Verify server-first contains expected SCRAM fields
        let server_first_decoded = OUTER.decode(&server_first_b64).unwrap();
        let server_first_str = String::from_utf8(server_first_decoded).unwrap();
        assert!(server_first_str.starts_with("r="));
        assert!(server_first_str.contains(",s="));
        assert!(server_first_str.contains(",i="));
        assert!(server_first_str.contains(&client_nonce));

        // 4. Client: create client-final-message
        let (client_final_b64, expected_server_sig) =
            client_final_message(password, &client_first_b64, &server_first_b64).unwrap();

        // Verify client-final structure
        let client_final_decoded = OUTER.decode(&client_final_b64).unwrap();
        let client_final_str = String::from_utf8(client_final_decoded).unwrap();
        assert!(client_final_str.starts_with("c=biws,"));
        assert!(client_final_str.contains(",p="));

        // 5. Server: verify and get server signature
        let server_sig = server_verify_final(&handshake, &client_final_b64).unwrap();
        assert_eq!(server_sig, expected_server_sig);

        // 6. Wrong password: server rejects the proof
        let (wrong_final_b64, _) =
            client_final_message("wrongpassword", &client_first_b64, &server_first_b64).unwrap();
        let result = server_verify_final(&handshake, &wrong_final_b64);
        assert!(result.is_err());
        match result {
            Err(AuthError::InvalidCredentials) => {} // expected
            other => panic!("expected InvalidCredentials, got {:?}", other),
        }
    }

    #[test]
    fn test_format_www_authenticate() {
        let result = format_www_authenticate(Some("tok123"), Some("c29tZQ"));
        assert_eq!(
            result,
            "SCRAM hash=SHA-256, handshakeToken=tok123, data=c29tZQ"
        );
    }

    #[test]
    fn test_format_auth_info() {
        let result = format_auth_info("auth-tok", "ZGF0YQ");
        assert_eq!(result, "authToken=auth-tok, hash=SHA-256, data=ZGF0YQ");
    }
}

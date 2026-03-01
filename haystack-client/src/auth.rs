//! Client-side SCRAM SHA-256 authentication handshake.
//!
//! Performs the three-phase Haystack auth handshake (HELLO, SCRAM, BEARER)
//! against a Haystack server, returning the auth token on success.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use reqwest::Client;

use crate::error::ClientError;
use haystack_core::auth;

/// Perform SCRAM SHA-256 authentication handshake against a Haystack server.
///
/// Executes the three-phase handshake:
/// 1. HELLO: sends username, receives SCRAM challenge
/// 2. SCRAM: sends client proof, receives auth token and server signature
/// 3. Returns the auth token for subsequent Bearer authentication
///
/// # Arguments
/// * `client` - The reqwest HTTP client to use
/// * `base_url` - The server API root (e.g. `http://localhost:8080/api`)
/// * `username` - The username to authenticate as
/// * `password` - The user's plaintext password
///
/// # Returns
/// The auth token string on success.
pub async fn authenticate(
    client: &Client,
    base_url: &str,
    username: &str,
    password: &str,
) -> Result<String, ClientError> {
    let base_url = base_url.trim_end_matches('/');
    let about_url = format!("{}/about", base_url);

    // -----------------------------------------------------------------------
    // Phase 1: HELLO
    // -----------------------------------------------------------------------
    // Send GET /api/about with Authorization: HELLO username=<base64(username)>, data=<client_first>
    let username_b64 = BASE64.encode(username.as_bytes());
    let (client_nonce, client_first_b64) = auth::client_first_message(username);
    let hello_header = format!("HELLO username={}, data={}", username_b64, client_first_b64);

    let hello_resp = client
        .get(&about_url)
        .header("Authorization", &hello_header)
        .send()
        .await
        .map_err(|e| ClientError::Transport(e.to_string()))?;

    if hello_resp.status() != reqwest::StatusCode::UNAUTHORIZED {
        return Err(ClientError::AuthFailed(format!(
            "expected 401 from HELLO, got {}",
            hello_resp.status()
        )));
    }

    // Parse WWW-Authenticate header
    // Expected: SCRAM handshakeToken=..., hash=SHA-256, data=<server_first_b64>
    let www_auth = hello_resp
        .headers()
        .get("www-authenticate")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            ClientError::AuthFailed("missing WWW-Authenticate header in 401 response".to_string())
        })?
        .to_string();

    let (handshake_token, server_first_b64) = parse_www_authenticate(&www_auth)?;

    // -----------------------------------------------------------------------
    // Phase 2: SCRAM
    // -----------------------------------------------------------------------
    // Compute client-final-message from server-first-message
    let (client_final_b64, expected_server_sig) =
        auth::client_final_message(password, &client_nonce, &server_first_b64, username)
            .map_err(|e| ClientError::AuthFailed(e.to_string()))?;

    // Send GET /api/about with Authorization: SCRAM handshakeToken=..., data=<client_final>
    let scram_header = format!(
        "SCRAM handshakeToken={}, data={}",
        handshake_token, client_final_b64
    );

    let scram_resp = client
        .get(&about_url)
        .header("Authorization", &scram_header)
        .send()
        .await
        .map_err(|e| ClientError::Transport(e.to_string()))?;

    if !scram_resp.status().is_success() {
        return Err(ClientError::AuthFailed(format!(
            "SCRAM phase failed with status {}",
            scram_resp.status()
        )));
    }

    // -----------------------------------------------------------------------
    // Phase 3: Extract auth token
    // -----------------------------------------------------------------------
    // Parse Authentication-Info header: authToken=..., data=<server_final_b64>
    let auth_info = scram_resp
        .headers()
        .get("authentication-info")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            ClientError::AuthFailed(
                "missing Authentication-Info header in SCRAM response".to_string(),
            )
        })?
        .to_string();

    let (auth_token, server_final_b64) = parse_auth_info(&auth_info)?;

    // Verify the server signature from the server-final-message
    let server_final_bytes = BASE64.decode(&server_final_b64).map_err(|e| {
        ClientError::AuthFailed(format!("invalid base64 in server-final data: {}", e))
    })?;
    let server_final_msg = String::from_utf8(server_final_bytes).map_err(|e| {
        ClientError::AuthFailed(format!("invalid UTF-8 in server-final data: {}", e))
    })?;
    let server_sig_b64 = server_final_msg.strip_prefix("v=").ok_or_else(|| {
        ClientError::AuthFailed("server-final message missing v= prefix".to_string())
    })?;
    let received_server_sig = BASE64.decode(server_sig_b64).map_err(|e| {
        ClientError::AuthFailed(format!("invalid base64 in server signature: {}", e))
    })?;

    if received_server_sig != expected_server_sig {
        return Err(ClientError::AuthFailed(
            "server signature verification failed".to_string(),
        ));
    }

    Ok(auth_token)
}

/// Parse the WWW-Authenticate header from a SCRAM challenge response.
///
/// Expected format: `SCRAM handshakeToken=<token>, hash=SHA-256, data=<b64>`
///
/// Returns `(handshake_token, server_first_data_b64)`.
fn parse_www_authenticate(header: &str) -> Result<(String, String), ClientError> {
    let rest = header
        .trim()
        .strip_prefix("SCRAM ")
        .ok_or_else(|| ClientError::AuthFailed("WWW-Authenticate not SCRAM scheme".to_string()))?;

    let mut handshake_token = None;
    let mut data = None;

    for part in rest.split(',') {
        let part = part.trim();
        if let Some(val) = part.strip_prefix("handshakeToken=") {
            handshake_token = Some(val.trim().to_string());
        } else if let Some(val) = part.strip_prefix("data=") {
            data = Some(val.trim().to_string());
        }
        // hash= is informational; we always use SHA-256
    }

    let handshake_token = handshake_token.ok_or_else(|| {
        ClientError::AuthFailed("missing handshakeToken in WWW-Authenticate".to_string())
    })?;
    let data = data
        .ok_or_else(|| ClientError::AuthFailed("missing data in WWW-Authenticate".to_string()))?;

    Ok((handshake_token, data))
}

/// Parse the Authentication-Info header to extract the auth token and server-final data.
///
/// Expected format: `authToken=<token>, data=<b64>`
///
/// Returns `(auth_token, server_final_data_b64)`.
fn parse_auth_info(header: &str) -> Result<(String, String), ClientError> {
    let mut auth_token = None;
    let mut data = None;

    for part in header.split(',') {
        let part = part.trim();
        if let Some(val) = part.strip_prefix("authToken=") {
            auth_token = Some(val.trim().to_string());
        } else if let Some(val) = part.strip_prefix("data=") {
            data = Some(val.trim().to_string());
        }
    }

    let auth_token = auth_token.ok_or_else(|| {
        ClientError::AuthFailed("missing authToken in Authentication-Info header".to_string())
    })?;
    let data = data.ok_or_else(|| {
        ClientError::AuthFailed("missing data in Authentication-Info header".to_string())
    })?;

    Ok((auth_token, data))
}

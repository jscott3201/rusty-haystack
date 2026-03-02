// SCRAM SHA-256 authentication bindings.

use pyo3::prelude::*;
use pyo3::types::PyBytes;

use haystack_core::auth;

use crate::exceptions;

/// Derive SCRAM credentials from a password, salt, and iteration count.
/// Returns (stored_key, server_key) as bytes.
#[pyfunction]
pub fn derive_credentials(
    py: Python<'_>,
    password: &str,
    salt: &Bound<'_, PyBytes>,
    iterations: u32,
) -> (Py<PyAny>, Py<PyAny>) {
    let creds = auth::derive_credentials(password, salt.as_bytes(), iterations);
    let stored = PyBytes::new(py, &creds.stored_key);
    let server = PyBytes::new(py, &creds.server_key);
    (stored.into_any().unbind(), server.into_any().unbind())
}

/// Generate a random SCRAM nonce string.
#[pyfunction]
pub fn generate_nonce() -> String {
    auth::generate_nonce()
}

/// Create the SCRAM client-first-message for a given username.
/// Returns (client_first_message, client_nonce).
#[pyfunction]
pub fn client_first_message(username: &str) -> (String, String) {
    auth::client_first_message(username)
}

/// Compute the client-final-message from password and server challenge.
/// Returns (client_final_b64, server_signature_bytes).
#[pyfunction]
pub fn client_final_message(
    py: Python<'_>,
    password: &str,
    client_nonce: &str,
    server_first_b64: &str,
    username: &str,
) -> PyResult<(String, Py<PyAny>)> {
    let (final_msg, server_sig) =
        auth::client_final_message(password, client_nonce, server_first_b64, username)
            .map_err(|e| PyErr::new::<exceptions::AuthError, _>(e.to_string()))?;
    let sig_bytes = PyBytes::new(py, &server_sig);
    Ok((final_msg, sig_bytes.into_any().unbind()))
}

/// Extract the client nonce from a base64-encoded client-first-message.
#[pyfunction]
pub fn extract_client_nonce(client_first_b64: &str) -> PyResult<String> {
    auth::extract_client_nonce(client_first_b64)
        .map_err(|e| PyErr::new::<exceptions::AuthError, _>(e.to_string()))
}

/// Parse an Authorization/WWW-Authenticate header into its components.
/// Returns a dict with keys depending on the header type:
/// - "hello": {"username": str, "data": str|None}
/// - "scram": {"handshake_token": str, "data": str}
/// - "bearer": {"auth_token": str}
#[pyfunction]
pub fn parse_auth_header(py: Python<'_>, header: &str) -> PyResult<Py<PyAny>> {
    let parsed = auth::parse_auth_header(header)
        .map_err(|e| PyErr::new::<exceptions::AuthError, _>(e.to_string()))?;

    let dict = pyo3::types::PyDict::new(py);
    match parsed {
        auth::AuthHeader::Hello { username, data } => {
            dict.set_item("type", "hello")?;
            dict.set_item("username", username)?;
            dict.set_item("data", data)?;
        }
        auth::AuthHeader::Scram {
            handshake_token,
            data,
        } => {
            dict.set_item("type", "scram")?;
            dict.set_item("handshake_token", handshake_token)?;
            dict.set_item("data", data)?;
        }
        auth::AuthHeader::Bearer { auth_token } => {
            dict.set_item("type", "bearer")?;
            dict.set_item("auth_token", auth_token)?;
        }
    }
    Ok(dict.into_any().unbind())
}

/// Format a WWW-Authenticate SCRAM challenge header.
#[pyfunction]
pub fn format_www_authenticate(handshake_token: &str, hash: &str, data_b64: &str) -> String {
    auth::format_www_authenticate(handshake_token, hash, data_b64)
}

/// Format an Authentication-Info header with bearer token and data.
#[pyfunction]
pub fn format_auth_info(auth_token: &str, data_b64: &str) -> String {
    auth::format_auth_info(auth_token, data_b64)
}

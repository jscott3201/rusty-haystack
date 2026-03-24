//! The `about` op — server identity and SCRAM authentication handshake.
//!
//! `GET /api/about` is dual-purpose:
//! - **Unauthenticated** (HELLO / SCRAM): drives the three-phase SCRAM
//!   SHA-256 handshake.
//! - **Authenticated** (BEARER token): returns the server about grid.
//!
//! `POST /api/close` revokes the bearer token (logout).

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use haystack_core::auth::{AuthHeader, parse_auth_header};
use haystack_core::data::{HCol, HDict, HGrid};
use haystack_core::kinds::Kind;

use crate::content;
use crate::error::error_grid;
use crate::state::SharedState;

/// Build a haystack response from body bytes and content type.
fn haystack_response(body: Vec<u8>, content_type: &str) -> Response {
    (
        [(axum::http::header::CONTENT_TYPE, content_type.to_string())],
        body,
    )
        .into_response()
}

/// GET /api/about
pub async fn handle(State(state): State<SharedState>, headers: HeaderMap) -> Response {
    let accept = headers
        .get("Accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    // If auth is not enabled, just return the about grid
    if !state.auth.is_enabled() {
        return respond_about_grid(accept);
    }

    let auth_header = headers.get("Authorization").and_then(|v| v.to_str().ok());

    match auth_header {
        None => {
            // No auth header: return 401 prompting for HELLO
            (
                StatusCode::UNAUTHORIZED,
                [("WWW-Authenticate", "HELLO")],
                "Authentication required",
            )
                .into_response()
        }
        Some(header) => match parse_auth_header(header) {
            Ok(AuthHeader::Hello { username, data }) => {
                match state.auth.handle_hello(&username, data.as_deref()) {
                    Ok(www_auth) => (
                        StatusCode::UNAUTHORIZED,
                        [("WWW-Authenticate", www_auth.as_str())],
                        "",
                    )
                        .into_response(),
                    Err(e) => {
                        log::warn!("HELLO failed for {username}: {e}");
                        let grid = error_grid(&format!("authentication failed: {e}"));
                        respond_error_grid(&grid, accept, StatusCode::FORBIDDEN)
                    }
                }
            }
            Ok(AuthHeader::Scram {
                handshake_token,
                data,
            }) => match state.auth.handle_scram(&handshake_token, &data) {
                Ok((_auth_token, auth_info)) => (
                    StatusCode::OK,
                    [("Authentication-Info", auth_info.as_str())],
                    "",
                )
                    .into_response(),
                Err(e) => {
                    log::warn!("SCRAM verification failed: {e}");
                    let grid = error_grid("authentication failed");
                    respond_error_grid(&grid, accept, StatusCode::FORBIDDEN)
                }
            },
            Ok(AuthHeader::Bearer { auth_token }) => match state.auth.validate_token(&auth_token) {
                Some(_user) => respond_about_grid(accept),
                None => {
                    let grid = error_grid("invalid or expired auth token");
                    respond_error_grid(&grid, accept, StatusCode::UNAUTHORIZED)
                }
            },
            Err(e) => {
                log::warn!("Invalid Authorization header: {e}");
                (
                    StatusCode::BAD_REQUEST,
                    format!("Invalid Authorization header: {e}"),
                )
                    .into_response()
            }
        },
    }
}

/// POST /api/close — revoke the bearer token (logout).
pub async fn handle_close(State(state): State<SharedState>, headers: HeaderMap) -> Response {
    let accept = headers
        .get("Accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    // Find the token from the Authorization header and revoke it
    if let Some(auth_header) = headers.get("Authorization").and_then(|v| v.to_str().ok())
        && let Ok(AuthHeader::Bearer { auth_token }) = parse_auth_header(auth_header)
    {
        state.auth.revoke_token(&auth_token);
        log::info!("User logged out");
    }

    // Return empty grid
    let grid = HGrid::new();
    match content::encode_response_grid(&grid, accept) {
        Ok((body, ct)) => haystack_response(body, ct),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "encoding error").into_response(),
    }
}

/// Build and encode the about grid response.
fn respond_about_grid(accept: &str) -> Response {
    let mut row = HDict::new();
    row.set("haystackVersion", Kind::Str("4.0".to_string()));
    row.set("serverName", Kind::Str("rusty-haystack".to_string()));
    row.set("serverVersion", Kind::Str("0.7.2".to_string()));
    row.set("productName", Kind::Str("rusty-haystack".to_string()));
    row.set(
        "productUri",
        Kind::Uri(haystack_core::kinds::Uri::new(
            "https://github.com/jscott3201/rusty-haystack",
        )),
    );
    row.set("moduleName", Kind::Str("haystack-server".to_string()));
    row.set("moduleVersion", Kind::Str("0.7.2".to_string()));

    let cols = vec![
        HCol::new("haystackVersion"),
        HCol::new("serverName"),
        HCol::new("serverVersion"),
        HCol::new("productName"),
        HCol::new("productUri"),
        HCol::new("moduleName"),
        HCol::new("moduleVersion"),
    ];

    let grid = HGrid::from_parts(HDict::new(), cols, vec![row]);
    match content::encode_response_grid(&grid, accept) {
        Ok((body, ct)) => haystack_response(body, ct),
        Err(e) => {
            log::error!("Failed to encode about grid: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "encoding error").into_response()
        }
    }
}

/// Encode an error grid and return it as a Response.
fn respond_error_grid(grid: &HGrid, accept: &str, status: StatusCode) -> Response {
    match content::encode_response_grid(grid, accept) {
        Ok((body, ct)) => (
            status,
            [(axum::http::header::CONTENT_TYPE, ct.to_string())],
            body,
        )
            .into_response(),
        Err(_) => (status, "error").into_response(),
    }
}

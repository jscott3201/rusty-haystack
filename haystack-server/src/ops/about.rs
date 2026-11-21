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

    if headers.get_all("Authorization").iter().count() > 1 {
        return auth_failure(crate::auth::AuthFailure::Rejected, accept);
    }
    let Some(value) = headers.get("Authorization") else {
        return (
            StatusCode::UNAUTHORIZED,
            [("WWW-Authenticate", "HELLO")],
            "Authentication required",
        )
            .into_response();
    };
    let parsed = value.to_str().ok().and_then(|v| parse_auth_header(v).ok());
    match parsed {
        Some(AuthHeader::Hello { username }) => match state.auth.handle_hello(&username) {
            Ok(challenge) => (
                StatusCode::UNAUTHORIZED,
                [("WWW-Authenticate", challenge)],
                "",
            )
                .into_response(),
            Err(error) => auth_failure(error, accept),
        },
        Some(AuthHeader::Scram {
            handshake_token,
            data,
        }) => match state.auth.handle_scram(handshake_token.as_deref(), &data) {
            Ok(crate::auth::ScramResponse::Challenge(challenge)) => (
                StatusCode::UNAUTHORIZED,
                [("WWW-Authenticate", challenge)],
                "",
            )
                .into_response(),
            Ok(crate::auth::ScramResponse::Authenticated(info)) => {
                (StatusCode::OK, [("Authentication-Info", info)], "").into_response()
            }
            Err(error) => auth_failure(error, accept),
        },
        Some(AuthHeader::Bearer { auth_token }) => match state.auth.validate_token(&auth_token) {
            Some(_) => respond_about_grid(accept),
            None => respond_error_grid(
                &error_grid("invalid or expired auth token"),
                accept,
                StatusCode::UNAUTHORIZED,
            ),
        },
        None => auth_failure(crate::auth::AuthFailure::Rejected, accept),
    }
}

fn auth_failure(failure: crate::auth::AuthFailure, accept: &str) -> Response {
    let (status, message) = match failure {
        crate::auth::AuthFailure::Rejected => (StatusCode::FORBIDDEN, "authentication failed"),
        crate::auth::AuthFailure::Capacity => (
            StatusCode::SERVICE_UNAVAILABLE,
            "authentication capacity exceeded",
        ),
    };
    respond_error_grid(&error_grid(message), accept, status)
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
    row.set(
        "serverVersion",
        Kind::Str(env!("CARGO_PKG_VERSION").to_string()),
    );
    row.set("productName", Kind::Str("rusty-haystack".to_string()));
    row.set(
        "productUri",
        Kind::Uri(haystack_core::kinds::Uri::new(
            "https://github.com/jscott3201/rusty-haystack",
        )),
    );
    row.set("moduleName", Kind::Str("haystack-server".to_string()));
    row.set(
        "moduleVersion",
        Kind::Str(env!("CARGO_PKG_VERSION").to_string()),
    );

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

//! The `about` op — server identity and SCRAM authentication handshake.
//!
//! # Overview
//!
//! `GET /api/about` is dual-purpose:
//! - **Unauthenticated** (HELLO / SCRAM): drives the three-phase SCRAM
//!   SHA-256 handshake (HELLO → server challenge, SCRAM → client proof
//!   verification, BEARER → authenticated access).
//! - **Authenticated** (BEARER token): returns the server about grid.
//!
//! `POST /api/close` revokes the bearer token (logout).
//!
//! # Request
//!
//! No request grid is required. Authentication state is conveyed via the
//! `Authorization` header (`HELLO`, `SCRAM`, or `BEARER` scheme).
//!
//! # Response Grid Columns
//!
//! | Column          | Kind | Description                          |
//! |-----------------|------|--------------------------------------|
//! | `haystackVersion` | Str | Haystack specification version (e.g. `"4.0"`) |
//! | `serverName`    | Str  | Server implementation name           |
//! | `serverVersion` | Str  | Server software version              |
//! | `productName`   | Str  | Product name                         |
//! | `productUri`    | Uri  | Product homepage URI                 |
//! | `moduleName`    | Str  | Module / crate name                  |
//! | `moduleVersion` | Str  | Module / crate version               |
//!
//! # Errors
//!
//! - **401 Unauthorized** — missing or invalid `Authorization` header.
//! - **403 Forbidden** — HELLO lookup failed or SCRAM proof verification failed.
//! - **500 Internal Server Error** — grid encoding failure.

use actix_web::http::StatusCode;
use actix_web::{HttpMessage, HttpRequest, HttpResponse, web};

use haystack_core::auth::{AuthHeader, parse_auth_header};
use haystack_core::data::{HCol, HDict, HGrid};
use haystack_core::kinds::Kind;

use crate::content;
use crate::error::error_grid;
use crate::state::AppState;

/// GET /api/about
///
/// Handles three cases:
/// 1. No Authorization header or HELLO -> 401 with WWW-Authenticate
/// 2. SCRAM -> verify client proof, return 200 with Authentication-Info
/// 3. BEARER -> return about grid
pub async fn handle(req: HttpRequest, state: web::Data<AppState>) -> HttpResponse {
    let accept = req
        .headers()
        .get("Accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    // If auth is not enabled, just return the about grid
    if !state.auth.is_enabled() {
        return respond_about_grid(accept);
    }

    let auth_header = req
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok());

    match auth_header {
        None => {
            // No auth header: return 401 prompting for HELLO
            HttpResponse::Unauthorized()
                .insert_header(("WWW-Authenticate", "HELLO"))
                .body("Authentication required")
        }
        Some(header) => {
            match parse_auth_header(header) {
                Ok(AuthHeader::Hello { username, data }) => {
                    // HELLO phase: create handshake and return challenge
                    match state.auth.handle_hello(&username, data.as_deref()) {
                        Ok(www_auth) => HttpResponse::Unauthorized()
                            .insert_header(("WWW-Authenticate", www_auth))
                            .body(""),
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
                }) => {
                    // SCRAM phase: verify client proof
                    match state.auth.handle_scram(&handshake_token, &data) {
                        Ok((_auth_token, auth_info)) => HttpResponse::Ok()
                            .insert_header(("Authentication-Info", auth_info))
                            .body(""),
                        Err(e) => {
                            log::warn!("SCRAM verification failed: {e}");
                            let grid = error_grid("authentication failed");
                            respond_error_grid(&grid, accept, StatusCode::FORBIDDEN)
                        }
                    }
                }
                Ok(AuthHeader::Bearer { auth_token }) => {
                    // BEARER phase: validate token and return about grid
                    match state.auth.validate_token(&auth_token) {
                        Some(_user) => respond_about_grid(accept),
                        None => {
                            let grid = error_grid("invalid or expired auth token");
                            respond_error_grid(&grid, accept, StatusCode::UNAUTHORIZED)
                        }
                    }
                }
                Err(e) => {
                    log::warn!("Invalid Authorization header: {e}");
                    HttpResponse::BadRequest().body(format!("Invalid Authorization header: {e}"))
                }
            }
        }
    }
}

/// POST /api/close — revoke the bearer token (logout).
///
/// Invalidates the BEARER auth token supplied in the `Authorization`
/// header and returns an empty grid on success.
pub async fn handle_close(req: HttpRequest, state: web::Data<AppState>) -> HttpResponse {
    let accept = req
        .headers()
        .get("Accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if let Some(user) = req.extensions().get::<crate::auth::AuthUser>() {
        // Find the token from the Authorization header
        if let Some(auth_header) = req
            .headers()
            .get("Authorization")
            .and_then(|v| v.to_str().ok())
            && let Ok(AuthHeader::Bearer { auth_token }) = parse_auth_header(auth_header)
        {
            state.auth.revoke_token(&auth_token);
        }
        log::info!("User {} logged out", user.username);
    }

    // Return empty grid
    let grid = HGrid::new();
    match content::encode_response_grid(&grid, accept) {
        Ok((body, ct)) => HttpResponse::Ok().content_type(ct).body(body),
        Err(_) => HttpResponse::InternalServerError().body("encoding error"),
    }
}

/// Build and encode the about grid response.
fn respond_about_grid(accept: &str) -> HttpResponse {
    let mut row = HDict::new();
    row.set("haystackVersion", Kind::Str("4.0".to_string()));
    row.set("serverName", Kind::Str("rusty-haystack".to_string()));
    row.set("serverVersion", Kind::Str("0.6.1".to_string()));
    row.set("productName", Kind::Str("rusty-haystack".to_string()));
    row.set(
        "productUri",
        Kind::Uri(haystack_core::kinds::Uri::new(
            "https://github.com/jscott3201/rusty-haystack",
        )),
    );
    row.set("moduleName", Kind::Str("haystack-server".to_string()));
    row.set("moduleVersion", Kind::Str("0.6.1".to_string()));

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
        Ok((body, ct)) => HttpResponse::Ok().content_type(ct).body(body),
        Err(e) => {
            log::error!("Failed to encode about grid: {e}");
            HttpResponse::InternalServerError().body("encoding error")
        }
    }
}

/// Encode an error grid and return it as an HttpResponse.
fn respond_error_grid(grid: &HGrid, accept: &str, status: StatusCode) -> HttpResponse {
    match content::encode_response_grid(grid, accept) {
        Ok((body, ct)) => HttpResponse::build(status).content_type(ct).body(body),
        Err(_) => HttpResponse::build(status).body("error"),
    }
}

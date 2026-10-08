//! Pinned executable HTTP adapter. Authentication precedes protocol resolution;
//! raw collection, native binding, authorization and encoding share admission.
use crate::{auth::AuthManager, state::SharedState};
use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode, header},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use haystack_app::{
    ApiError, CancellationToken, Principal, ReadContext, ReadError, TypedInvocationInput,
};
use haystack_core::auth::{AuthHeader, parse_auth_header};
use std::{sync::Arc, time::Instant};

pub(crate) fn selected(request: &Request<Body>) -> bool {
    let path = request.uri().path();
    if path == "/api/ops" {
        return !legacy_ops(request);
    }
    path.starts_with("/api/")
        && !crate::capabilities::CAPABILITIES
            .iter()
            .any(|capability| capability.path == path)
}
// Only absent or one explicitly selected v4 control retains the public legacy
// GET path. This is a routing hint, not version validation: every malformed,
// duplicate or other selection goes through authentication first.
fn legacy_ops(request: &Request<Body>) -> bool {
    if request.method() != axum::http::Method::GET {
        return false;
    }
    let mut query_version = None;
    for pair in request.uri().query().unwrap_or("").split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if wire_equal(key, b"xeto-version") {
            if query_version.is_some() {
                return false;
            }
            query_version = Some(wire_equal(value, b"4"));
        }
    }
    let mut headers = request.headers().get_all("xeto-version").iter();
    let first = headers.next();
    if headers.next().is_some() {
        return false;
    }
    query_version.unwrap_or_else(|| first.is_none_or(|value| value == "4"))
}
pub(crate) fn error(error: ApiError) -> Response {
    let post_only = matches!(error, ApiError::MethodNotAllowed);
    let mut response = (
        StatusCode::from_u16(error.status()).expect("fixed status"),
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::HeaderName::from_static("xeto-version"), "5"),
        ],
        error.json(),
    )
        .into_response();
    if post_only {
        response
            .headers_mut()
            .insert(header::ALLOW, "POST".parse().expect("fixed method"));
    }
    response
}
/// This hint affects only the documented auth-status compatibility choice. It
/// does not validate or reject any protocol control before authentication.
fn v5_hint(request: &Request<Body>) -> bool {
    let mut selected = None;
    for pair in request.uri().query().unwrap_or("").split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if wire_equal(key, b"xeto-version") {
            if selected.is_some() {
                return false;
            }
            selected = Some(wire_equal(value, b"5"));
        }
    }
    selected.unwrap_or_else(|| {
        let mut versions = request.headers().get_all("xeto-version").iter();
        versions.next().is_some_and(|v| v == "5") && versions.next().is_none()
    })
}
// Non-allocating hint only: malformed controls remain unresolved until after
// authentication; a unique query control has the same precedence as dispatch.
fn wire_equal(encoded: &str, expected: &[u8]) -> bool {
    let mut source = encoded.bytes();
    for target in expected {
        let byte = match source.next() {
            Some(b'%') => {
                let Some(a) = source.next().and_then(|c| (c as char).to_digit(16)) else {
                    return false;
                };
                let Some(b) = source.next().and_then(|c| (c as char).to_digit(16)) else {
                    return false;
                };
                (a * 16 + b) as u8
            }
            Some(b'+') => b' ',
            Some(c) => c,
            None => return false,
        };
        if byte != *target {
            return false;
        }
    }
    source.next().is_none()
}

pub(crate) async fn handle(State(state): State<SharedState>, request: Request<Body>) -> Response {
    match execute(state, request).await {
        Ok(response) => response,
        Err(err) => error(err),
    }
}
async fn execute(state: SharedState, request: Request<Body>) -> Result<Response, ApiError> {
    let application = state.application.as_ref().ok_or(ApiError::Unavailable)?;
    let service = application.read_service();
    let started = request
        .extensions()
        .get::<crate::ops::shared_read::ReadStarted>()
        .map_or_else(Instant::now, |s| s.0);
    let guard = request
        .extensions()
        .get::<Arc<haystack_app::WorkGuard>>()
        .ok_or(ApiError::Unavailable)?
        .child();
    let mut admission = service
        .begin_admitted(
            ReadContext::new(
                Principal::Anonymous,
                started + service.limits().max_duration,
                CancellationToken::new(),
            ),
            guard,
        )
        .await?;
    let raw_bytes = request.headers().iter().fold(
        request
            .uri()
            .path()
            .len()
            .saturating_add(request.uri().query().map_or(0, str::len)),
        |n, (key, value)| {
            n.saturating_add(key.as_str().len())
                .saturating_add(value.as_bytes().len())
                .saturating_add(64)
        },
    );
    admission.reserve_wire_input(raw_bytes)?;
    let v5 = v5_hint(&request);
    if state.auth.is_enabled() {
        let mut headers = request.headers().get_all(header::AUTHORIZATION).iter();
        let first = headers.next();
        let malformed = if v5 {
            ApiError::AuthMalformed
        } else {
            ApiError::AuthRequired
        };
        if headers.next().is_some() {
            return Err(malformed);
        }
        let text = first
            .ok_or(if v5 {
                ApiError::AuthRejected
            } else {
                ApiError::AuthRequired
            })?
            .to_str()
            .map_err(|_| malformed.clone())?;
        let AuthHeader::Bearer { auth_token } =
            parse_auth_header(text).map_err(|_| malformed.clone())?
        else {
            return Err(malformed);
        };
        let (user, session) = state.auth.validate_session(&auth_token).ok_or(if v5 {
            ApiError::AuthRejected
        } else {
            ApiError::AuthRequired
        })?;
        if !AuthManager::check_permission(&user, "read") {
            return Err(ApiError::Permission);
        }
        admission.bind_wire_session(
            Principal::authenticated(user.username, user.permissions),
            session,
        )?;
    }
    if !matches!(
        *request.method(),
        axum::http::Method::GET | axum::http::Method::POST
    ) {
        return Err(ApiError::NotImplemented);
    }
    let mut input = {
        let copy_headers = |name| -> Result<Vec<String>, ApiError> {
            request
                .headers()
                .get_all(name)
                .iter()
                .map(|v| {
                    v.to_str()
                        .map(str::to_owned)
                        .map_err(|_| ApiError::InvalidArgs)
                })
                .collect()
        };
        TypedInvocationInput {
            operation: request
                .uri()
                .path()
                .strip_prefix("/api/")
                .unwrap_or("")
                .to_owned(),
            post: request.method() == axum::http::Method::POST,
            query: request.uri().query().unwrap_or("").to_owned(),
            versions: copy_headers("xeto-version")?,
            content_types: copy_headers("content-type")?,
            accepts: copy_headers("accept")?,
            accept_encodings: copy_headers("accept-encoding")?,
            content_encoded: request.headers().contains_key(header::CONTENT_ENCODING),
            body: Vec::new(),
        }
    };
    let mut stream = request.into_body().into_data_stream();
    loop {
        let chunk = tokio::select! {
            biased;
            _ = admission.cancelled() => return Err(ApiError::from(ReadError::Cancelled)),
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(admission.deadline())) => return Err(ApiError::Timeout),
            chunk = stream.next() => chunk,
        };
        let Some(chunk) = chunk else {
            break;
        };
        let chunk = chunk.map_err(|_| ApiError::InvalidArgs)?;
        admission.reserve_wire_input(chunk.len())?;
        let needed = input.body.len().saturating_add(chunk.len());
        if needed > input.body.capacity() {
            // Geometric growth bounds all old-buffer copies and cumulative
            // allocations by the reservation made before this frame. Exact
            // one-frame growth would permit quadratic copies on tiny frames.
            let capacity = needed
                .next_power_of_two()
                .min(service.limits().max_input_bytes);
            input
                .body
                .try_reserve_exact(capacity - input.body.len())
                .map_err(|_| ApiError::Unavailable)?;
        }
        input.body.extend_from_slice(&chunk);
    }
    let response = admission.invoke_wire(input).await?;
    let gzip = response.gzip;
    let mut response = (
        [
            (header::CONTENT_TYPE, response.content_type),
            (
                header::HeaderName::from_static("xeto-version"),
                response.version,
            ),
        ],
        response.body,
    )
        .into_response();
    response.headers_mut().insert(
        header::VARY,
        "Accept, Xeto-Version, Accept-Encoding"
            .parse()
            .expect("fixed header"),
    );
    if gzip {
        response.headers_mut().insert(
            header::CONTENT_ENCODING,
            "gzip".parse().expect("fixed header"),
        );
    }
    Ok(response)
}
// Layer only the original default fallback before adding routes. Keeping
// Axum's default-fallback identity lets a trusted custom fallback take priority.
pub(crate) async fn fallback(_request: Request<Body>, _next: axum::middleware::Next) -> Response {
    error(ApiError::InvalidPath)
}
/// Built-in errors carry the current version, including errors raised by the
/// existing H4 authentication and processing paths. Their bodies stay intact.
pub(crate) async fn version_header(
    request: Request<Body>,
    next: axum::middleware::Next,
) -> Response {
    let builtin = selected(&request)
        || crate::capabilities::CAPABILITIES
            .iter()
            .any(|c| c.path == request.uri().path());
    let mut response = next.run(request).await;
    if builtin && !response.headers().contains_key("xeto-version") {
        let version = if response.status().is_success() {
            "4"
        } else {
            "5"
        };
        response
            .headers_mut()
            .insert("xeto-version", version.parse().expect("fixed version"));
    }
    response
}

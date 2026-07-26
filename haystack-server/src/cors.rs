//! Cross-origin resource sharing policy for the HTTP API.
//!
//! A Haystack server is often read by a browser dashboard served from a
//! different origin, which makes CORS a real requirement rather than a
//! convenience. It is off by default: a server that grows a cross-origin
//! surface should do so because someone asked for it, not because a
//! dependency shipped a permissive default.

use std::time::Duration;

use axum::http::{HeaderValue, Method, header};
use tower_http::cors::CorsLayer;

/// How the server answers cross-origin requests.
///
/// The default is [`CorsPolicy::Disabled`], which emits no CORS headers at
/// all. Browsers then apply the same-origin policy unchanged, which is the
/// behaviour every release before this one had.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum CorsPolicy {
    /// Emit no CORS headers. Cross-origin browser requests are refused by the
    /// browser, exactly as they were before CORS support existed.
    #[default]
    Disabled,

    /// Allow the listed origins, and nothing else.
    ///
    /// Each entry is matched against the request's `Origin` header verbatim,
    /// scheme and port included — `https://ops.example.com` does not permit
    /// `http://ops.example.com` or `https://ops.example.com:8443`.
    Allow(Vec<String>),
}

impl CorsPolicy {
    /// Build the tower-http layer for this policy.
    ///
    /// Returns `None` when the policy is [`CorsPolicy::Disabled`], so the
    /// caller can skip wrapping the router entirely rather than installing a
    /// layer that does nothing.
    ///
    /// An origin this server cannot honour is dropped with a warning rather
    /// than aborting startup: a typo in one dashboard origin should not take
    /// the whole server down, and a dropped origin fails closed — it simply is
    /// not allowed.
    ///
    /// `null` is honoured if it is listed explicitly. It is a real `Origin`
    /// value, sent by sandboxed iframes, `file://` documents and some
    /// redirects, and it stands for *any* opaque origin — so list it only
    /// deliberately.
    pub fn layer(&self) -> Option<CorsLayer> {
        match self {
            CorsPolicy::Disabled => None,
            CorsPolicy::Allow(origins) => Some(build_allow_layer(
                origins
                    .iter()
                    .map(String::as_str)
                    .filter_map(usable)
                    .collect(),
            )),
        }
    }
}

/// Convert one configured origin into a header value, or drop it.
///
/// `*` is refused rather than honoured, for two reasons. tower-http panics
/// outright if a wildcard reaches `AllowOrigin::list`, so passing it through
/// would crash the server after it had already loaded its data. And this
/// server's policy is an explicit list by design — silently widening it to
/// every origin is not a reasonable reading of a one-character flag value.
fn usable(origin: &str) -> Option<HeaderValue> {
    if origin == "*" {
        log::warn!(
            "ignoring CORS origin \"*\": this server allows only an explicit list; \
             name each origin in full, scheme and port included"
        );
        return None;
    }

    HeaderValue::from_str(origin)
        .inspect_err(|_| log::warn!("ignoring invalid CORS origin {origin:?}"))
        .ok()
}

/// Construct the layer for an explicit origin allowlist.
///
/// The allowance is deliberately narrower than the request: `GET` and `POST`
/// are the only verbs the API answers (see the route table in
/// `HaystackServer::build_router`), and `Authorization`
/// and `Content-Type` the only request headers it reads — the first carries
/// SCRAM, Basic or Bearer credentials, the second drives codec negotiation.
/// Adding a route that needs more than this means revisiting here, which is
/// the point: mirroring the request instead would leave the origin list as the
/// only thing enforcing anything.
///
/// `Access-Control-Allow-Credentials` is absent on purpose. It governs
/// cookies, and this server sets none — its auth is header-based, and a header
/// crosses origins by being named in `Access-Control-Allow-Headers` alone.
/// Leaving it off also keeps a wildcard origin available, which the Fetch spec
/// forbids once credentials are allowed.
///
/// The ten-minute preflight cache trades one round trip per request against
/// how long a policy change takes to reach a browser that already cached it.
fn build_allow_layer(allowed: Vec<HeaderValue>) -> CorsLayer {
    CorsLayer::new()
        .allow_origin(allowed)
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
        .max_age(Duration::from_secs(600))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_is_the_default_and_installs_no_layer() {
        assert_eq!(CorsPolicy::default(), CorsPolicy::Disabled);
        assert!(CorsPolicy::Disabled.layer().is_none());
    }

    #[test]
    fn an_allowlist_installs_a_layer() {
        let policy = CorsPolicy::Allow(vec!["https://ops.example.com".to_string()]);
        assert!(policy.layer().is_some());
    }

    // `CorsLayer` is opaque, so a test that only asks whether a layer exists
    // cannot tell "kept the origin" from "dropped everything". These assert on
    // `usable` instead, which is where the decision is actually made; the
    // granted-header behaviour is covered end to end by
    // `app::tests::cors_layering`.

    #[test]
    fn a_well_formed_origin_is_kept() {
        assert_eq!(
            usable("https://ops.example.com").unwrap(),
            "https://ops.example.com"
        );
    }

    #[test]
    fn a_value_that_cannot_be_a_header_is_dropped() {
        // A newline cannot appear in a header value. The server should still
        // start, with that origin simply not allowed.
        assert!(usable("bad\norigin").is_none());
    }

    #[test]
    fn a_wildcard_is_dropped_rather_than_forwarded() {
        // tower-http's AllowOrigin::list panics on `*`, so forwarding one would
        // take the server down at startup rather than fail closed.
        assert!(usable("*").is_none());
    }

    #[test]
    fn an_allowlist_of_only_unusable_origins_still_yields_a_layer() {
        // Fails closed: a layer that grants nothing, rather than no layer at
        // all (which would read as "CORS disabled" and mask the typo).
        let policy = CorsPolicy::Allow(vec!["bad\norigin".to_string()]);
        assert!(policy.layer().is_some());
    }
}

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
    /// An origin that is not a valid HTTP header value is dropped with a
    /// warning rather than aborting startup: a typo in one dashboard origin
    /// should not take the whole server down, and the dropped origin fails
    /// closed (it simply is not allowed).
    pub fn layer(&self) -> Option<CorsLayer> {
        match self {
            CorsPolicy::Disabled => None,
            CorsPolicy::Allow(origins) => {
                let allowed: Vec<HeaderValue> = origins
                    .iter()
                    .filter_map(|o| match HeaderValue::from_str(o) {
                        Ok(v) => Some(v),
                        Err(_) => {
                            log::warn!("ignoring invalid CORS origin {o:?}");
                            None
                        }
                    })
                    .collect();

                Some(build_allow_layer(allowed))
            }
        }
    }
}

/// Construct the layer for an explicit origin allowlist.
///
/// The allowance is deliberately narrower than the request: `GET` and `POST`
/// are the only verbs the API answers (`app.rs:149-174`), and `Authorization`
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

    #[test]
    fn an_invalid_origin_is_dropped_rather_than_fatal() {
        // A newline cannot appear in a header value. The server should still
        // start, with that origin simply not allowed.
        let policy = CorsPolicy::Allow(vec![
            "https://ok.example.com".to_string(),
            "bad\norigin".to_string(),
        ]);
        assert!(policy.layer().is_some());
    }

    #[test]
    fn an_allowlist_of_only_invalid_origins_still_yields_a_layer() {
        // Fails closed: a layer that allows nothing, rather than no layer at
        // all (which would read as "CORS disabled" and mask the typo).
        let policy = CorsPolicy::Allow(vec!["bad\norigin".to_string()]);
        assert!(policy.layer().is_some());
    }
}

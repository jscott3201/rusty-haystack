use reqwest::Client;

use crate::error::ClientError;
use crate::transport::Transport;
use haystack_core::codecs::codec_for;
use haystack_core::data::HGrid;

/// Operations that use GET (noSideEffects).
const GET_OPS: &[&str] = &["about", "ops", "formats"];

enum AuthCredential {
    Bearer(zeroize::Zeroizing<String>),
    Basic {
        username: String,
        password: zeroize::Zeroizing<String>,
    },
}

/// HTTP transport for communicating with a Haystack server.
///
/// Sends requests as encoded grids over HTTP using the configured wire format
/// (default: `text/zinc`). GET is used for side-effect-free ops; POST for all others.
pub struct HttpTransport {
    client: Client,
    base_url: String,
    auth: AuthCredential,
    format: String,
}

impl HttpTransport {
    /// SCRAM session transport using a bearer token.
    ///
    /// The supplied client's TLS, timeout, redirect, and retry policies are
    /// caller-owned. Use `ClientConfig::build_reqwest_client` for safe defaults.
    pub fn with_bearer(base_url: &str, auth_token: String, client: Client, format: &str) -> Self {
        Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            auth: AuthCredential::Bearer(zeroize::Zeroizing::new(auth_token)),
            format: format.to_string(),
        }
    }

    /// HTTP Basic auth on every request (Niagara nHaystack).
    ///
    /// The supplied client's redirect and retry policies are caller-owned.
    ///
    /// Basic sends the password on **every** request, base64-encoded, which is
    /// encoding rather than encryption. Over plain HTTP that is the password in
    /// cleartext on the wire — a different exposure from SCRAM, which never
    /// transmits it at all.
    ///
    /// The warning below is defence in depth for callers constructing a
    /// transport directly, **not** the boundary: a library `log::warn!` reaches
    /// nobody unless the application installed a logger. The boundary is in
    /// [`HaystackClient::connect_with_config`], which refuses this combination
    /// unless `ClientConfig::allow_plaintext_basic` is set.
    ///
    /// [`HaystackClient::connect_with_config`]: crate::HaystackClient::connect_with_config
    pub fn with_basic(
        base_url: &str,
        username: &str,
        password: &str,
        client: Client,
        format: &str,
    ) -> Self {
        if !base_url.starts_with("https://") {
            log::warn!(
                "HTTP Basic auth over a non-HTTPS URL: the password is sent \
                 base64-encoded, not encrypted, on every request"
            );
        }
        Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            auth: AuthCredential::Basic {
                username: username.to_string(),
                password: zeroize::Zeroizing::new(password.to_string()),
            },
            format: format.to_string(),
        }
    }

    /// Create a new HTTP transport with SCRAM bearer token (strict TLS, default client).
    ///
    /// # Panics
    /// Panics if the default HTTP client cannot be initialized. Call
    /// `ClientConfig::build_reqwest_client` and `with_bearer` for fallible setup.
    pub fn new(base_url: &str, auth_token: String) -> Self {
        Self::with_bearer(
            base_url,
            auth_token,
            crate::config::ClientConfig::default()
                .build_reqwest_client()
                .expect("default HTTP client configuration"),
            "text/zinc",
        )
    }

    /// Create a new HTTP transport with a specific wire format.
    ///
    /// # Panics
    /// Panics if the default HTTP client cannot be initialized. Call
    /// `ClientConfig::build_reqwest_client` and `with_bearer` for fallible setup.
    pub fn with_format(base_url: &str, auth_token: String, format: &str) -> Self {
        Self::with_bearer(
            base_url,
            auth_token,
            crate::config::ClientConfig::default()
                .build_reqwest_client()
                .expect("default HTTP client configuration"),
            format,
        )
    }

    fn apply_auth(
        &self,
        builder: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder, ClientError> {
        match &self.auth {
            AuthCredential::Bearer(token) => {
                let value = zeroize::Zeroizing::new(format!("BEARER authToken={}", **token));
                Ok(builder.header("Authorization", crate::auth::sensitive_header(&value)?))
            }
            AuthCredential::Basic { username, password } => {
                Ok(builder.basic_auth(username, Some(password.as_str())))
            }
        }
    }
}

impl Transport for HttpTransport {
    async fn call(&self, op: &str, req: &HGrid) -> Result<HGrid, ClientError> {
        crate::config::validate_http_url(&self.base_url)?;
        if op.is_empty()
            || !op
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        {
            return Err(ClientError::Connection(
                "invalid HTTP operation name".into(),
            ));
        }
        let url = format!("{}/{}", self.base_url, op);

        let response = if GET_OPS.contains(&op) {
            self.apply_auth(self.client.get(&url))?
                .header("Accept", &self.format)
                .send()
                .await
                .map_err(crate::error::http_error)?
        } else {
            let codec = codec_for(&self.format).ok_or_else(|| {
                ClientError::Codec(format!("unsupported format: {}", self.format))
            })?;
            let text = codec
                .encode_grid(req)
                .map_err(|e| ClientError::Codec(e.to_string()))?;
            let body_bytes = text.into_bytes();
            let content_type = codec.mime_type();

            self.apply_auth(self.client.post(&url))?
                .header("Content-Type", content_type)
                .header("Accept", &self.format)
                .body(body_bytes)
                .send()
                .await
                .map_err(crate::error::http_error)?
        };

        let status = response.status();

        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(ClientError::AuthFailed(
                "HTTP credentials rejected; reconnect explicitly".into(),
            ));
        }
        if !status.is_success() {
            return Err(ClientError::ServerError(format!("HTTP {status}")));
        }
        let resp_body = response.text().await.map_err(crate::error::http_error)?;
        let codec = codec_for(&self.format)
            .ok_or_else(|| ClientError::Codec(format!("unsupported format: {}", self.format)))?;
        let grid = codec
            .decode_grid(&resp_body)
            .map_err(|_| ClientError::Codec("invalid HTTP response grid".into()))?;

        if grid.is_err() {
            return Err(ClientError::ServerError(
                "server returned an error grid".into(),
            ));
        }

        Ok(grid)
    }

    async fn close(&self) -> Result<(), ClientError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// reqwest builds a TLS-capable client even for these header-only checks, and
    /// this crate installs the rustls provider explicitly rather than relying on a
    /// default feature. Without it `Client::new()` panics.
    fn client() -> Client {
        crate::ensure_crypto_provider();
        Client::new()
    }

    fn header_of(t: &HttpTransport) -> String {
        let req = t
            .apply_auth(t.client.get("https://example.test/api/about"))
            .expect("valid auth header")
            .build()
            .expect("request builds");
        req.headers()
            .get("authorization")
            .expect("an Authorization header")
            .to_str()
            .expect("header is valid ascii")
            .to_string()
    }

    #[test]
    fn basic_auth_sends_rfc7617_credentials() {
        // base64("user:secret") == "dXNlcjpzZWNyZXQ="
        let t = HttpTransport::with_basic(
            "https://station.test/api",
            "user",
            "secret",
            client(),
            "text/zinc",
        );
        assert_eq!(header_of(&t), "Basic dXNlcjpzZWNyZXQ=");
    }

    /// The bearer header is byte-for-byte what it was before `apply_auth`
    /// existed. Both call sites used to build this string inline; routing them
    /// through one helper is only safe if the output did not move, and a
    /// Haystack server rejects anything but this exact shape.
    #[test]
    fn bearer_auth_keeps_the_haystack_header_shape() {
        let t = HttpTransport::with_bearer(
            "https://station.test/api",
            "abc123".to_string(),
            client(),
            "text/zinc",
        );
        assert_eq!(header_of(&t), "BEARER authToken=abc123");
    }

    #[test]
    fn authorization_headers_are_sensitive_and_debug_redacted() {
        let sentinel = "private-credential-sentinel";
        for transport in [
            HttpTransport::with_bearer(
                "https://station.test/api",
                sentinel.into(),
                client(),
                "text/zinc",
            ),
            HttpTransport::with_basic(
                "https://station.test/api",
                "user",
                sentinel,
                client(),
                "text/zinc",
            ),
        ] {
            let request = transport
                .apply_auth(transport.client.get("https://station.test/api/about"))
                .unwrap()
                .build()
                .unwrap();
            let header = request.headers().get("authorization").unwrap();
            assert!(header.is_sensitive());
            assert!(!format!("{request:?}").contains(sentinel));
            assert!(!format!("{request:?}").contains(header.to_str().unwrap()));
        }
    }

    #[test]
    fn trailing_slashes_are_trimmed_from_the_base_url() {
        let t =
            HttpTransport::with_basic("https://station.test/api/", "u", "p", client(), "text/zinc");
        assert_eq!(t.base_url, "https://station.test/api");
    }
}

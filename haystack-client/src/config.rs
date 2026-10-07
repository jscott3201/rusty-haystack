//! Client connection options (TLS verification and auth mode).

use std::time::Duration;

use crate::error::ClientError;

/// How the client authenticates to the Haystack HTTP API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthMode {
    /// Published three-request Haystack SCRAM SHA-256 with unpadded base64url data.
    #[default]
    Scram,
    /// HTTP Basic on every request (`Authorization: Basic …`).
    /// Required for Niagara nHaystack when the service user uses `HTTPBasicScheme`.
    Basic,
}

/// TLS and auth settings for [`crate::HaystackClient::connect_with_config`].
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// When false, accept self-signed or otherwise untrusted server certificates **and**
    /// skip hostname (SAN) verification via `danger_accept_invalid_certs` (lab/dev only).
    pub tls_verify: bool,
    /// Which authentication scheme the client uses (SCRAM or HTTP Basic).
    pub auth_mode: AuthMode,
    /// Response wire format MIME type (default `text/zinc`).
    pub wire_format: String,
    /// Overall per-request timeout applied to the underlying reqwest client.
    pub timeout: Duration,
    /// Total SCRAM handshake budget, including queueing and key derivation.
    pub auth_timeout: Duration,
    /// Additional CA trust and optional client identity, reused for every HTTP request.
    pub tls: Option<crate::tls::TlsConfig>,
    /// Permit [`AuthMode::Basic`] against a non-HTTPS URL.
    ///
    /// Off by default, and refused rather than warned about, because Basic sends
    /// a reusable password on every request and a library `log::warn!` reaches
    /// nobody unless the consuming application happens to have installed a
    /// logger. A boundary that is commonly invisible is not a boundary.
    ///
    /// Setting this puts the decision in the caller's own source, where it is
    /// greppable and reviewable.
    pub allow_plaintext_basic: bool,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            tls_verify: true,
            auth_mode: AuthMode::Scram,
            wire_format: "text/zinc".to_string(),
            timeout: Duration::from_secs(30),
            auth_timeout: Duration::from_secs(30),
            tls: None,
            allow_plaintext_basic: false,
        }
    }
}

impl ClientConfig {
    /// Preset for Niagara nHaystack lab stations (self-signed HTTPS + HTTP Basic).
    pub fn niagara_lab() -> Self {
        Self {
            tls_verify: false,
            auth_mode: AuthMode::Basic,
            ..Self::default()
        }
    }

    /// SCRAM against a server with a self-signed certificate.
    pub fn scram_insecure_tls() -> Self {
        Self {
            tls_verify: false,
            auth_mode: AuthMode::Scram,
            ..Self::default()
        }
    }

    /// Build a `reqwest` client with the configured trust, identity and timeout.
    /// Redirects and automatic retries are disabled, including for authentication.
    pub fn build_reqwest_client(&self) -> Result<reqwest::Client, ClientError> {
        crate::ensure_crypto_provider();
        let mut builder = reqwest::Client::builder()
            .timeout(self.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never());
        if let Some(tls) = &self.tls {
            builder = tls.apply(builder)?;
        }
        if !self.tls_verify {
            log::warn!(
                "TLS certificate AND hostname verification disabled (danger_accept_invalid_certs); \
                 connection is vulnerable to MITM — lab/dev use only"
            );
            builder = builder.tls_danger_accept_invalid_certs(true);
        }
        builder
            .build()
            .map_err(|_| ClientError::Connection("HTTP client build failed".into()))
    }
}

/// Validate before constructing authenticated requests; never echo a URL that may contain secrets.
pub(crate) fn validate_http_url(input: &str) -> Result<url::Url, ClientError> {
    let url = url::Url::parse(input)
        .map_err(|_| ClientError::Connection("invalid HTTP API URL".into()))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(ClientError::Connection(
            "HTTP API URL must use http or https with a host".into(),
        ));
    }
    // Inspect the authority as well: Url normalizes empty userinfo away.
    let has_userinfo = input.split_once("://").is_some_and(|(_, rest)| {
        rest.split(['/', '?', '#'])
            .next()
            .is_some_and(|authority| authority.contains('@'))
    });
    if has_userinfo
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ClientError::Connection(
            "HTTP API URL must not contain userinfo, query, or fragment".into(),
        ));
    }
    Ok(url)
}

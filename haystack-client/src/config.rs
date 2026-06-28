//! Client connection options (TLS verification and auth mode).

use std::time::Duration;

use crate::error::ClientError;

/// How the client authenticates to the Haystack HTTP API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthMode {
    /// Project Haystack SCRAM SHA-256 (`HELLO` → `SCRAM` → `BEARER`).
    /// Used by [`rusty-haystack` server](https://github.com/jscott3201/rusty-haystack), SkySpark, etc.
    #[default]
    Scram,
    /// HTTP Basic on every request (`Authorization: Basic …`).
    /// Required for Niagara nHaystack when the service user uses `HTTPBasicScheme`.
    Basic,
}

/// TLS and auth settings for [`crate::HaystackClient::connect_with_config`].
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// When false, accept self-signed or otherwise untrusted server certificates (lab use).
    pub tls_verify: bool,
    pub auth_mode: AuthMode,
    /// Response wire format MIME type (default `text/zinc`).
    pub wire_format: String,
    pub timeout: Duration,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            tls_verify: true,
            auth_mode: AuthMode::Scram,
            wire_format: "text/zinc".to_string(),
            timeout: Duration::from_secs(30),
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

    /// Build a `reqwest` client from this configuration.
    pub fn build_reqwest_client(&self) -> Result<reqwest::Client, ClientError> {
        let mut builder = reqwest::Client::builder().timeout(self.timeout);
        if !self.tls_verify {
            builder = builder.danger_accept_invalid_certs(true);
        }
        builder
            .build()
            .map_err(|e| ClientError::Connection(format!("HTTP client build failed: {e}")))
    }
}

use std::time::Duration;

use reqwest::Client;

use crate::error::ClientError;
use crate::transport::Transport;
use haystack_core::codecs::codec_for;
use haystack_core::data::HGrid;
use haystack_core::kinds::Kind;

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
    pub fn with_bearer(base_url: &str, auth_token: String, client: Client, format: &str) -> Self {
        Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            auth: AuthCredential::Bearer(zeroize::Zeroizing::new(auth_token)),
            format: format.to_string(),
        }
    }

    /// HTTP Basic auth on every request (Niagara nHaystack).
    pub fn with_basic(
        base_url: &str,
        username: &str,
        password: &str,
        client: Client,
        format: &str,
    ) -> Self {
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
    pub fn new(base_url: &str, auth_token: String) -> Self {
        Self::with_bearer(
            base_url,
            auth_token,
            Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap_or_else(|_| Client::new()),
            "text/zinc",
        )
    }

    /// Create a new HTTP transport with a specific wire format.
    pub fn with_format(base_url: &str, auth_token: String, format: &str) -> Self {
        Self::with_bearer(
            base_url,
            auth_token,
            Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap_or_else(|_| Client::new()),
            format,
        )
    }

    fn apply_auth(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.auth {
            AuthCredential::Bearer(token) => {
                builder.header("Authorization", format!("BEARER authToken={}", &**token))
            }
            AuthCredential::Basic { username, password } => {
                builder.basic_auth(username, Some(password.as_str()))
            }
        }
    }
}

impl Transport for HttpTransport {
    async fn call(&self, op: &str, req: &HGrid) -> Result<HGrid, ClientError> {
        let url = format!("{}/{}", self.base_url, op);

        let response = if GET_OPS.contains(&op) {
            self.apply_auth(self.client.get(&url))
                .header("Accept", &self.format)
                .send()
                .await
                .map_err(|e| ClientError::Transport(e.to_string()))?
        } else {
            let codec = codec_for(&self.format).ok_or_else(|| {
                ClientError::Codec(format!("unsupported format: {}", self.format))
            })?;
            let text = codec
                .encode_grid(req)
                .map_err(|e| ClientError::Codec(e.to_string()))?;
            let body_bytes = text.into_bytes();
            let content_type = codec.mime_type();

            self.apply_auth(self.client.post(&url))
                .header("Content-Type", content_type)
                .header("Accept", &self.format)
                .body(body_bytes)
                .send()
                .await
                .map_err(|e| ClientError::Transport(e.to_string()))?
        };

        let status = response.status();

        let resp_body = response
            .text()
            .await
            .map_err(|e| ClientError::Transport(e.to_string()))?;
        if !status.is_success() {
            return Err(ClientError::ServerError(format!(
                "HTTP {} — {}",
                status, resp_body
            )));
        }
        let codec = codec_for(&self.format)
            .ok_or_else(|| ClientError::Codec(format!("unsupported format: {}", self.format)))?;
        let grid = codec
            .decode_grid(&resp_body)
            .map_err(|e| ClientError::Codec(e.to_string()))?;

        if grid.is_err() {
            let dis = grid
                .meta
                .get("dis")
                .and_then(|k| {
                    if let Kind::Str(s) = k {
                        Some(s.as_str())
                    } else {
                        None
                    }
                })
                .unwrap_or("unknown server error");
            return Err(ClientError::ServerError(dis.to_string()));
        }

        Ok(grid)
    }

    async fn close(&self) -> Result<(), ClientError> {
        Ok(())
    }
}

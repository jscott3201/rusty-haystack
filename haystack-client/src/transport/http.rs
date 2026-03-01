use reqwest::Client;

use crate::error::ClientError;
use crate::transport::Transport;
use haystack_core::codecs::codec_for;
use haystack_core::data::HGrid;
use haystack_core::kinds::Kind;

/// Operations that use GET (noSideEffects).
const GET_OPS: &[&str] = &["about", "ops", "formats"];

/// HTTP transport for communicating with a Haystack server.
///
/// Sends requests as encoded grids over HTTP using the configured wire format
/// (default: `text/zinc`). GET is used for side-effect-free ops; POST for all others.
pub struct HttpTransport {
    client: Client,
    base_url: String,
    auth_token: String,
    format: String,
}

impl HttpTransport {
    /// Create a new HTTP transport.
    ///
    /// `base_url` should be the server API root (e.g. `http://localhost:8080/api`).
    /// `auth_token` is the bearer token obtained from SCRAM authentication.
    pub fn new(base_url: &str, auth_token: String) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            auth_token,
            format: "text/zinc".to_string(),
        }
    }

    /// Create a new HTTP transport with a specific wire format.
    pub fn with_format(base_url: &str, auth_token: String, format: &str) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            auth_token,
            format: format.to_string(),
        }
    }
}

impl Transport for HttpTransport {
    async fn call(&self, op: &str, req: &HGrid) -> Result<HGrid, ClientError> {
        let url = format!("{}/{}", self.base_url, op);
        let codec = codec_for(&self.format)
            .ok_or_else(|| ClientError::Codec(format!("unsupported format: {}", self.format)))?;

        let response = if GET_OPS.contains(&op) {
            // GET request for side-effect-free ops
            self.client
                .get(&url)
                .header(
                    "Authorization",
                    format!("BEARER authToken={}", self.auth_token),
                )
                .header("Accept", codec.mime_type())
                .send()
                .await
                .map_err(|e| ClientError::Transport(e.to_string()))?
        } else {
            // POST request for all other ops
            let body = codec
                .encode_grid(req)
                .map_err(|e| ClientError::Codec(e.to_string()))?;

            self.client
                .post(&url)
                .header(
                    "Authorization",
                    format!("BEARER authToken={}", self.auth_token),
                )
                .header("Content-Type", codec.mime_type())
                .header("Accept", codec.mime_type())
                .body(body)
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

        // Decode the response grid
        let grid = codec
            .decode_grid(&resp_body)
            .map_err(|e| ClientError::Codec(e.to_string()))?;

        // Check for error grid (meta has "err" marker)
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
        // HTTP is stateless; nothing to close.
        Ok(())
    }
}

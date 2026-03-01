//! TLS configuration for mutual TLS (mTLS) client authentication.
//!
//! Provides [`TlsConfig`] which holds the PEM-encoded client certificate,
//! private key, and optional CA certificate needed to establish an mTLS
//! connection to a Haystack server.

/// Configuration for mutual TLS (mTLS) client authentication.
///
/// Holds the raw PEM bytes for the client certificate, private key, and an
/// optional CA certificate used to verify the server.
#[derive(Debug, Clone)]
pub struct TlsConfig {
    /// PEM-encoded client certificate.
    pub client_cert_pem: Vec<u8>,
    /// PEM-encoded client private key.
    pub client_key_pem: Vec<u8>,
    /// Optional PEM-encoded CA certificate for server verification.
    pub ca_cert_pem: Option<Vec<u8>>,
}

impl TlsConfig {
    /// Load TLS configuration from files on disk.
    ///
    /// # Arguments
    /// * `cert_path` - Path to the PEM-encoded client certificate file
    /// * `key_path` - Path to the PEM-encoded client private key file
    /// * `ca_path` - Optional path to a PEM-encoded CA certificate file
    ///
    /// # Errors
    /// Returns an error string if any file cannot be read.
    pub fn from_files(
        cert_path: &str,
        key_path: &str,
        ca_path: Option<&str>,
    ) -> Result<Self, String> {
        let client_cert_pem =
            std::fs::read(cert_path).map_err(|e| format!("reading cert '{cert_path}': {e}"))?;
        let client_key_pem =
            std::fs::read(key_path).map_err(|e| format!("reading key '{key_path}': {e}"))?;
        let ca_cert_pem = if let Some(ca) = ca_path {
            Some(std::fs::read(ca).map_err(|e| format!("reading CA '{ca}': {e}"))?)
        } else {
            None
        };
        Ok(Self {
            client_cert_pem,
            client_key_pem,
            ca_cert_pem,
        })
    }
}

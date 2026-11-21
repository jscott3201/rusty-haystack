//! Additive server trust and optional mutual TLS (mTLS) client identity.
//!
//! Provides [`TlsConfig`] which holds the PEM-encoded client certificate,
//! private key, and optional CA certificate needed to establish an mTLS
//! connection to a Haystack server.

/// Additional CA trust and optional mutual TLS (mTLS) client authentication.
///
/// Holds the raw PEM bytes for the client certificate, private key, and an
/// optional CA certificate used to verify the server.
#[derive(Clone, Default)]
pub struct TlsConfig {
    /// PEM-encoded client certificate. Leave both identity buffers empty for CA-only trust.
    pub client_cert_pem: Vec<u8>,
    /// PEM-encoded client private key, zeroized when this configuration is dropped.
    pub client_key_pem: Vec<u8>,
    /// Optional PEM-encoded CA certificate for server verification.
    pub ca_cert_pem: Option<Vec<u8>>,
}

impl std::fmt::Debug for TlsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsConfig")
            .field("client_identity", &(!self.client_cert_pem.is_empty()))
            .field("additional_ca", &self.ca_cert_pem.is_some())
            .finish_non_exhaustive()
    }
}

impl Drop for TlsConfig {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.client_key_pem.zeroize();
    }
}

impl TlsConfig {
    /// Trust an additional PEM-encoded CA bundle without presenting a client identity.
    pub fn with_ca(ca_cert_pem: Vec<u8>) -> Self {
        Self {
            ca_cert_pem: Some(ca_cert_pem),
            client_cert_pem: Vec::new(),
            client_key_pem: Vec::new(),
        }
    }

    pub(crate) fn apply(
        &self,
        mut builder: reqwest::ClientBuilder,
    ) -> Result<reqwest::ClientBuilder, crate::ClientError> {
        if self.client_cert_pem.is_empty() != self.client_key_pem.is_empty() {
            return Err(crate::ClientError::Connection(
                "client certificate and key must be supplied together".into(),
            ));
        }
        if !self.client_cert_pem.is_empty() {
            let mut pem = zeroize::Zeroizing::new(self.client_cert_pem.clone());
            pem.extend_from_slice(&self.client_key_pem);
            let identity = reqwest::Identity::from_pem(&pem).map_err(|_| {
                crate::ClientError::Connection("invalid client certificate or key".into())
            })?;
            builder = builder.identity(identity);
        }
        if let Some(ca) = &self.ca_cert_pem {
            let certs = reqwest::Certificate::from_pem_bundle(ca).map_err(|_| {
                crate::ClientError::Connection("invalid CA certificate bundle".into())
            })?;
            if certs.is_empty() {
                return Err(crate::ClientError::Connection(
                    "empty CA certificate bundle".into(),
                ));
            }
            builder = builder.tls_certs_merge(certs);
        }
        Ok(builder)
    }

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
        let client_cert_pem = std::fs::read(cert_path)
            .map_err(|_| "reading client certificate failed".to_string())?;
        let client_key_pem = zeroize::Zeroizing::new(
            std::fs::read(key_path).map_err(|_| "reading client key failed".to_string())?,
        );
        let ca_cert_pem = if let Some(ca) = ca_path {
            Some(std::fs::read(ca).map_err(|_| "reading CA certificate failed".to_string())?)
        } else {
            None
        };
        Ok(Self {
            client_cert_pem,
            client_key_pem: client_key_pem.to_vec(),
            ca_cert_pem,
        })
    }
}

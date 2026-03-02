//! Client error types for the Haystack client library.
//!
//! [`ClientError`] covers all failure modes that can occur during client operations.

/// Errors that can occur during Haystack client operations.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// SCRAM authentication handshake failed (invalid credentials, server rejected).
    #[error("authentication failed: {0}")]
    AuthFailed(String),

    /// Server returned an error grid or non-200 HTTP status.
    #[error("server error: {0}")]
    ServerError(String),

    /// Low-level HTTP or WebSocket transport failure.
    #[error("transport error: {0}")]
    Transport(String),

    /// Failed to establish a connection (DNS, TCP, TLS handshake).
    #[error("connection error: {0}")]
    Connection(String),

    /// Zinc/JSON/CSV encoding or decoding failure.
    #[error("codec error: {0}")]
    Codec(String),

    /// The WebSocket or HTTP connection was closed unexpectedly.
    #[error("connection closed")]
    ConnectionClosed,

    /// An operation exceeded the configured timeout duration.
    #[error("request timed out after {0:?}")]
    Timeout(std::time::Duration),

    /// WebSocket concurrent request limit exceeded (backpressure).
    #[error("too many in-flight requests")]
    TooManyRequests,
}

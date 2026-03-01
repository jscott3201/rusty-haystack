#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("authentication failed: {0}")]
    AuthFailed(String),
    #[error("server error: {0}")]
    ServerError(String),
    #[error("transport error: {0}")]
    Transport(String),
    #[error("connection error: {0}")]
    Connection(String),
    #[error("codec error: {0}")]
    Codec(String),
    #[error("connection closed")]
    ConnectionClosed,
}

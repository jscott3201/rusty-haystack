//! Haystack HTTP and WebSocket client library.
//!
//! Provides [`HaystackClient`] for communicating with Project Haystack servers
//! using the standard REST API and optional WebSocket transport for real-time watches.
//!
//! ## Features
//!
//! - **SCRAM SHA-256 authentication** — Automatic handshake on connect
//! - **HTTP + WebSocket** — HTTP for standard ops, WebSocket for watch subscriptions
//! - **mTLS** — Mutual TLS via [`tls::TlsConfig`] for certificate-based auth
//! - **Zinc wire format** — Default encoding for fastest serialization
//! - **30+ Haystack ops** — about, read, nav, hisRead, hisWrite, pointWrite,
//!   watchSub, watchPoll, watchUnsub, invokeAction, defs, libs, and more
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use haystack_client::HaystackClient;
//!
//! # async fn example() -> Result<(), haystack_client::ClientError> {
//! let client = HaystackClient::connect("http://localhost:8080", "user", "pass").await?;
//! let about = client.about().await?;
//! let sites = client.read("site", None).await?;
//! client.close().await;
//! # Ok(())
//! # }
//! ```
//!
//! ## Error Handling
//!
//! All operations return `Result<_, ClientError>`. See [`ClientError`] for the
//! error variants covering authentication, transport, codec, and timeout failures.

pub mod auth;
pub mod client;
pub mod error;
pub mod tls;
pub mod transport;

pub use client::HaystackClient;
pub use error::ClientError;

/// Install the ring crypto provider for rustls.
///
/// Called automatically by client constructors. Safe to call multiple times;
/// only the first call has effect.
fn ensure_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

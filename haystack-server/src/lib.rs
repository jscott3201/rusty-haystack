//! Haystack HTTP API server with SCRAM auth, WebSocket watches, and federation.
//!
//! Provides [`HaystackServer`] — an Actix Web-based HTTP server implementing
//! the full Project Haystack REST API with 30+ standard operations.
//!
//! ## Features
//!
//! - **30+ Haystack ops** — about, read, nav, hisRead, hisWrite, pointWrite,
//!   watchSub, watchPoll, watchUnsub, invokeAction, defs, libs, formats, and more
//! - **SCRAM SHA-256 auth** — Token-based authentication with configurable TTL
//! - **WebSocket watches** — Real-time entity change subscriptions with compression
//! - **Federation** — Hub-and-spoke topology with automatic entity routing,
//!   delta sync, and adaptive intervals
//! - **History store** — In-memory time-series storage for his-tagged points
//! - **Content negotiation** — Zinc, JSON, JSON v3, CSV via Accept header
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use haystack_core::graph::{EntityGraph, SharedGraph};
//! use haystack_server::HaystackServer;
//!
//! # async fn example() -> std::io::Result<()> {
//! let graph = SharedGraph::new(EntityGraph::new());
//! HaystackServer::new(graph)
//!     .port(8080)
//!     .host("0.0.0.0")
//!     .run()
//!     .await
//! # }
//! ```

pub mod actions;
pub mod app;
pub mod auth;
pub mod connector;
pub mod content;
pub mod demo;
pub mod domain_scope;
pub mod error;
pub mod federation;
pub mod his_provider;
pub mod his_store;
pub mod ops;
pub mod session;
pub mod state;
pub mod ws;

pub use app::HaystackServer;
pub use domain_scope::DomainScope;
pub use federation::Federation;
pub use his_provider::HistoryProvider;

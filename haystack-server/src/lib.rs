//! Haystack HTTP API server with SCRAM auth and WebSocket watches.
//!
//! Provides [`HaystackServer`] — an Axum-based HTTP server implementing
//! the full Project Haystack REST API with 30+ standard operations.
//!
//! ## Features
//!
//! - **30+ Haystack ops** — about, read, nav, hisRead, hisWrite, pointWrite,
//!   watchSub, watchPoll, watchUnsub, invokeAction, defs, libs, formats, and more
//! - **SCRAM SHA-256 auth** — Token-based authentication with configurable TTL
//! - **WebSocket watches** — Real-time entity change subscriptions
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
pub mod content;
pub mod demo;
pub mod error;
pub mod his_provider;
pub mod his_store;
pub mod ops;
pub mod state;
pub mod ws;

pub use app::HaystackServer;
pub use his_provider::HistoryProvider;

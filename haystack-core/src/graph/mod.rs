//! In-memory entity graph with indexed querying, change tracking, and thread-safe access.
//!
//! The graph module provides a high-performance entity store optimized for
//! the Haystack read/write/subscribe pattern:
//!
//! - [`EntityGraph`] — The core graph store with entities, indexes, and query engine.
//!   Supports bitmap tag indexes, B-tree value indexes, and bidirectional ref adjacency.
//! - [`SharedGraph`] — Thread-safe wrapper (`Arc<RwLock<EntityGraph>>`) for concurrent access
//!   from server handlers and WebSocket watches.
//! - [`GraphDiff`] / [`DiffOp`] — Change tracking entries (add/update/remove) stored in
//!   a bounded changelog for incremental sync and watch notification.
//!
//! ## Submodules
//!
//! | Module | Description |
//! |--------|-------------|
//! | [`entity_graph`] | Core `EntityGraph` with CRUD, filter queries, namespace-aware evaluation |
//! | [`shared`] | `SharedGraph` — concurrent read/write access with `parking_lot::RwLock` |
//! | [`bitmap`] | `TagBitmapIndex` — bitset-per-tag for O(popcount) Has/Missing filters |
//! | [`value_index`] | `ValueIndex` — B-tree indexes for range queries (`temp > 72`) |
//! | [`adjacency`] | `RefAdjacency` — bidirectional `HashMap<SmallVec>` for ref edges |
//! | [`changelog`] | `GraphDiff` / `DiffOp` — bounded change log with version tracking |

pub mod adjacency;
pub mod bitmap;
pub mod changelog;
pub mod entity_graph;
pub mod shared;
pub mod subscriber;
pub mod value_index;

pub use changelog::{ChangelogGap, DEFAULT_CHANGELOG_CAPACITY, DiffOp, GraphDiff};
pub use entity_graph::{EntityGraph, GraphError, HierarchyNode};
pub use shared::SharedGraph;
pub use subscriber::GraphSubscriber;

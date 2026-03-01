// EntityGraph layer — in-memory entity graph with bitmap indexing,
// bidirectional ref adjacency, query planning, change tracking,
// and thread-safe concurrent access.

pub mod adjacency;
pub mod bitmap;
pub mod changelog;
pub mod entity_graph;
pub mod query_planner;
pub mod shared;

pub use changelog::{DiffOp, GraphDiff};
pub use entity_graph::{EntityGraph, GraphError};
pub use shared::SharedGraph;

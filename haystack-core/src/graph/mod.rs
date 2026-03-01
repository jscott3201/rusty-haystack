// EntityGraph layer — in-memory entity graph with bitmap indexing,
// bidirectional ref adjacency, query planning, change tracking,
// and thread-safe concurrent access.

pub mod bitmap;
pub mod adjacency;
pub mod entity_graph;
pub mod query_planner;
pub mod shared;
pub mod changelog;

pub use entity_graph::{EntityGraph, GraphError};
pub use shared::SharedGraph;
pub use changelog::{GraphDiff, DiffOp};

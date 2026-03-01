pub mod http;
pub mod ws;

use haystack_core::data::HGrid;
use crate::error::ClientError;

/// Transport abstraction for communicating with a Haystack server.
///
/// Implementations send a request grid to a named op and return the response grid.
/// Rust 2024 edition supports async fn in traits natively.
pub trait Transport: Send + Sync {
    /// Call a Haystack op with the given request grid and return the response grid.
    fn call(&self, op: &str, req: &HGrid) -> impl std::future::Future<Output = Result<HGrid, ClientError>> + Send;

    /// Close the transport, releasing any resources.
    fn close(&self) -> impl std::future::Future<Output = Result<(), ClientError>> + Send;
}

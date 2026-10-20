//! Bounded-history-v1 consumers. The buffered helper and incremental HTTP
//! session consume the same finite remote profile; neither creates a server
//! pull protocol or an atomic cross-store snapshot.
//!
//! ```no_run
//! use haystack_client::{HaystackClient, history::{HistoryStreamOptions, HistoryStreamEnd}};
//! use haystack_core::codecs::history::HistoryReadRequest;
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let client = HaystackClient::connect("https://example.com/api", "user", "password").await?;
//! let request = HistoryReadRequest { id: "point".into(), range: "2024-06-01".into() };
//! let mut session = client.his_read_stream(&request, HistoryStreamOptions::default()).await?;
//! loop {
//!     let batch = session.next().await;
//!     for sample in batch.samples { println!("{sample:?}"); }
//!     if let Some(end) = batch.end {
//!         match end {
//!             HistoryStreamEnd::Remote(terminal) => println!("{terminal:?}"),
//!             HistoryStreamEnd::Local(error) => eprintln!("partial read: {error}"),
//!         }
//!         break;
//!     }
//! }
//! session.close().await;
//! # Ok(())
//! # }
//! ```
mod decoder;
pub(crate) mod stream;
use crate::{
    ClientError, HaystackClient,
    transport::{Transport, http::HttpTransport},
};
use haystack_core::codecs::history::{self, HistoryReadRequest, HistoryReadResult};
pub use stream::*;
/// Explicit contract for a bounded single response, with no automatic replay.
/// Custom transports own the same byte bounds and complete body validation.
pub trait HistoryTransport: Transport {
    fn check_history_read(&self) -> Result<(), ClientError> {
        Ok(())
    }
}
impl HistoryTransport for HttpTransport {
    fn check_history_read(&self) -> Result<(), ClientError> {
        self.check_history_read_policy()
    }
}
impl<T: HistoryTransport> HaystackClient<T> {
    /// Preserve Complete/Limited/Interrupted/Failed and every admitted partial
    /// sample. Wrong-point, malformed, contradictory, or absent metadata errors
    /// are protocol failures; this helper does not retry or infer completeness.
    pub async fn his_read_scoped(
        &self,
        request: &HistoryReadRequest,
    ) -> Result<HistoryReadResult, ClientError> {
        self.transport_history_check()?;
        let grid = history::request_grid(request)
            .map_err(|error| ClientError::Codec(error.to_string()))?;
        let grid = self.call("hisRead", &grid).await?;
        let result = history::result_from_grid(&grid)
            .map_err(|_| ClientError::Codec("invalid bounded history result".into()))?;
        history::validate_for_request(&result, request)
            .map_err(|_| ClientError::Codec("history result does not match request".into()))?;
        Ok(result)
    }
}

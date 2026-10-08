//! Transport-independent, authorized and bounded application reads.
//!
//! A service borrows the caller's Tokio runtime and shares an existing graph. It
//! owns neither a listener nor runtime shutdown. Pure `EntityGraph` remains a
//! separate trusted in-process API. HTTP adapters authenticate and select a wire
//! profile; they do not implement resource authorization.
mod budget;
mod output;
mod policy;
mod sanitize;
mod service;
mod types;
mod wire;

pub use haystack_core::filter::CatalogKind;
pub use policy::{AllowAll, PolicySnapshot, ReadPolicy};
pub use service::{ReadAdmission, ReadLoad, ReadService};
pub use tokio_util::sync::CancellationToken;
pub use types::*;

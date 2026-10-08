//! Transport-independent, authorized and bounded application reads.
//!
//! [`ApplicationBuilder`] creates one managed [`ReadService`] and an explicit
//! lifecycle owner. Adapters share its policy, cursor authority and admission;
//! selected resources and actual worker completion precede termination.
//! The runtime remains borrowed. Standalone callers await termination and then
//! drop their owned runtime outside async. [`ReadService::new`] is an unmanaged
//! compatibility API. Pure `EntityGraph` remains a trusted in-process API.
mod budget;
mod session;
mod subscription;
pub use session::SubscriptionSession;
pub use subscription::*;
mod history;
mod history_admission;
mod history_mutation;
mod history_mutation_wire;
mod history_provider;
pub use history_mutation_wire::HistoryMutationWireOperation;
mod history_range;
mod history_store;
pub use haystack_core::codecs::history::*;
pub use haystack_core::codecs::history_mutation::{
    HistoryOperationIdentity, HistoryReceiptQualification, HistoryWriteOutcome,
    HistoryWriteReceipt, HistoryWriteRejection, HistoryWriteRequest, HistoryWriteUnknown,
};
pub use history::*;
pub use history_mutation::*;
pub use history_provider::*;
pub use history_store::{HisStore, HistoryChangeRecord, HistoryStoreLimits};
mod entity_wire;
mod feed;
mod mutation;
pub use entity_wire::EntityWireOperation;
pub use mutation::{
    AllowAllMutations, EphemeralMutationStore, EphemeralProvider, MutationLimits, MutationPolicy,
    MutationProvider, MutationService, PreparedMutation,
};
mod lifecycle;
mod output;
mod policy;
mod registry;
pub use registry::{FunctionDescriptor, FunctionIdentity};
mod sanitize;
mod service;
mod typed_http;
mod types;
pub use typed_http::{ApiError, TypedInvocationInput, TypedInvocationResponse};
mod wire;

pub use haystack_core::filter::CatalogKind;
pub use lifecycle::{
    ApplicationBuilder, ApplicationError, ApplicationHandle, ApplicationOwner, ApplicationResource,
    ApplicationState, CloseReport, ListenerInfo, ReadyInfo, ResourceContext, ResourceFuture,
    ShutdownPhase, ShutdownPolicy, TerminationReport, WorkGuard,
};
pub use policy::{AllowAll, PolicySnapshot, ReadPolicy};
pub use service::{ReadAdmission, ReadLoad, ReadService};
pub use tokio_util::sync::CancellationToken;
pub use types::*;

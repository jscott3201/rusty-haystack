use haystack_core::{data::HGrid, filter::FilterNode};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

/// Caller identity is distinct from any adapter's configured upstream identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    Anonymous,
    Authenticated {
        subject: String,
        permissions: Vec<String>,
    },
    TrustedEmbedding {
        subject: String,
    },
}
impl Principal {
    pub fn authenticated(subject: impl Into<String>, mut permissions: Vec<String>) -> Self {
        permissions.sort();
        permissions.dedup();
        Self::Authenticated {
            subject: subject.into(),
            permissions,
        }
    }
    pub(crate) fn bytes(&self) -> usize {
        match self {
            Self::Anonymous => 1,
            Self::Authenticated {
                subject,
                permissions,
            } => permissions.iter().fold(subject.len(), |n, p| {
                n.saturating_add(p.len()).saturating_add(32)
            }),
            Self::TrustedEmbedding { subject } => subject.len(),
        }
    }
}

/// One absolute deadline, including admission and lock waits. Caller cancellation
/// does not mean service shutdown; dropping one service clone does not cancel it.
#[derive(Clone)]
pub struct ReadContext {
    pub principal: Principal,
    pub deadline: Instant,
    pub cancellation: CancellationToken,
}
impl ReadContext {
    pub fn new(principal: Principal, deadline: Instant, cancellation: CancellationToken) -> Self {
        Self {
            principal,
            deadline,
            cancellation,
        }
    }
    pub fn with_timeout(principal: Principal, duration: Duration) -> Self {
        let now = Instant::now();
        Self::new(
            principal,
            now.checked_add(duration).unwrap_or(now),
            CancellationToken::new(),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOperation {
    Read,
    Nav,
    Definitions,
    Libraries,
    Specs,
    Spec,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadQuery {
    Ids(Vec<String>),
    Filter(String),
    Nav(Option<String>),
    Definitions { filter: Option<String> },
    Libraries,
    Specs { library: Option<String> },
    Spec(String),
}
impl ReadQuery {
    pub fn operation(&self) -> ReadOperation {
        match self {
            Self::Ids(_) | Self::Filter(_) => ReadOperation::Read,
            Self::Nav(_) => ReadOperation::Nav,
            Self::Definitions { .. } => ReadOperation::Definitions,
            Self::Libraries => ReadOperation::Libraries,
            Self::Specs { .. } => ReadOperation::Specs,
            Self::Spec(_) => ReadOperation::Spec,
        }
    }
}

/// H4 output is the actual selected codec's bytes. Its null/missing, temporal,
/// metadata and numeric rendering semantics are observable parts of this profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum H4Codec {
    Zinc,
    Json,
    JsonV3,
    Trio,
}
impl H4Codec {
    pub fn mime(self) -> &'static str {
        match self {
            Self::Zinc => "text/zinc",
            Self::Json => "application/json",
            Self::JsonV3 => "application/json;v=3",
            Self::Trio => "text/trio",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputProfile {
    Typed,
    H4(H4Codec),
}
#[derive(Debug, Clone)]
pub struct ReadRequest {
    pub query: ReadQuery,
    pub projection: Vec<String>,
    pub page_size: usize,
    pub cursor: Option<String>,
    pub profile: OutputProfile,
}
impl ReadRequest {
    pub fn new(query: ReadQuery, profile: OutputProfile) -> Self {
        Self {
            query,
            projection: Vec::new(),
            page_size: 100,
            cursor: None,
            profile,
        }
    }
}

#[derive(Debug)]
pub enum ReadOutput {
    Typed(HGrid),
    H4 { body: Vec<u8>, codec: H4Codec },
}
#[derive(Debug)]
pub struct ReadPage {
    pub output: ReadOutput,
    pub complete: bool,
    pub cursor: Option<String>,
    /// Authorized rows in this page; no raw candidate/denied counts are exposed.
    pub row_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetKind {
    Input,
    Ids,
    Ast,
    Candidates,
    Forward,
    Inverse,
    Values,
    Work,
    Retained,
    Depth,
    Rows,
    Output,
    Regex,
}
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReadError {
    #[error("invalid query: {0}")]
    InvalidQuery(&'static str),
    #[error("resource unavailable")]
    Unavailable,
    #[error("operation forbidden")]
    Forbidden,
    #[error("cursor is stale or invalid; restart the query")]
    StaleCursor,
    #[error("read capacity exhausted")]
    Capacity,
    #[error("read cancelled")]
    Cancelled,
    #[error("read deadline exceeded")]
    Deadline,
    #[error("read budget exceeded: {0:?}")]
    Budget(BudgetKind),
    #[error("value cannot be projected strictly to the selected H4 profile")]
    Projection,
    #[error("invalid read service limits")]
    InvalidLimits,
}

/// Finite per-request budgets. Retained bytes conservatively count allocations
/// even after temporaries are freed. Raising a configured budget never removes it.
#[derive(Debug, Clone)]
pub struct ReadLimits {
    pub max_input_bytes: usize,
    pub max_ids: usize,
    pub max_ast_nodes: usize,
    pub max_ast_depth: usize,
    pub max_candidates: usize,
    pub max_forward_edges: usize,
    pub max_inverse_edges: usize,
    pub max_value_nodes: usize,
    pub max_work: usize,
    pub max_retained_bytes: usize,
    pub max_value_depth: usize,
    pub max_rows: usize,
    pub max_output_bytes: usize,
    /// Source bytes checked before regex parser/AST/HIR allocation.
    pub max_regex_source_bytes: usize,
    /// Compiled NFA and lazy-DFA cache ceilings, separate from source parsing.
    pub max_regex_bytes: usize,
    pub max_concurrent: usize,
    pub max_queued: usize,
    pub max_duration: Duration,
    pub cursor_capacity: usize,
    pub max_cursor_bytes: usize,
    pub cursor_ttl: Duration,
}
impl Default for ReadLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 65_536,
            max_ids: 512,
            max_ast_nodes: 256,
            max_ast_depth: 32,
            max_candidates: 10_000,
            max_forward_edges: 4096,
            max_inverse_edges: 4096,
            max_value_nodes: 100_000,
            max_work: 4_000_000,
            max_retained_bytes: 16 * 1024 * 1024,
            max_value_depth: 64,
            max_rows: 1000,
            max_output_bytes: 1024 * 1024,
            max_regex_source_bytes: 65_536,
            max_regex_bytes: 256 * 1024,
            max_concurrent: 8,
            max_queued: 16,
            max_duration: Duration::from_secs(5),
            cursor_capacity: 128,
            max_cursor_bytes: 4 * 1024 * 1024,
            cursor_ttl: Duration::from_secs(60),
        }
    }
}
impl ReadLimits {
    pub(crate) fn validate(&self) -> Result<(), ReadError> {
        if self.max_input_bytes == 0
            || self.max_input_bytes > 1024 * 1024
            || self.max_ast_depth == 0
            || self.max_ast_depth > 64
            || self.max_value_depth == 0
            || self.max_value_depth > 64
            || self.max_rows == 0
            || self.max_rows > 10_000
            || self.max_concurrent == 0
            || self.max_concurrent > 1024
            || self.max_queued > 4096
            || self.max_duration.is_zero()
            || self.max_duration > Duration::from_secs(60)
            || self.cursor_ttl.is_zero()
            || self.cursor_ttl > Duration::from_secs(3600)
            || self.cursor_capacity > 4096
            || self.max_retained_bytes > 256 * 1024 * 1024
            || self.max_cursor_bytes > 256 * 1024 * 1024
            || self.max_output_bytes > 16 * 1024 * 1024
            || self.max_regex_source_bytes > 1024 * 1024
            || self.max_regex_bytes > 16 * 1024 * 1024
        {
            return Err(ReadError::InvalidLimits);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum NormalizedQuery {
    Ids(Vec<String>),
    Filter(Option<FilterNode>),
    Nav(Option<String>),
    Definitions(Option<String>),
    Libraries,
    Specs(Option<String>),
    Spec(String),
}
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RequestIdentity {
    pub query: NormalizedQuery,
    pub projection: Vec<String>,
    pub page_size: usize,
    pub profile: OutputProfile,
    pub principal: Principal,
    pub scope: Arc<str>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Stamp {
    pub dataset: [u8; 16],
    pub incarnation: [u8; 16],
    pub entities: u64,
    pub catalog: u64,
}

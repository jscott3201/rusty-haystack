//! Contextual Jeto for a closed, immutable codec context.
//!
//! This module implements the retained Xeto `873b9224` Jeto contract for
//! admitted native values. It does not change Hayson or typed payload v1.
//! Context construction validates references and scalar grammar; it is not a
//! general Xeto library loader or a claim of complete-library admission.
mod budget;
mod context;
mod decode;
mod encode;
mod parser;
mod scalar;
mod storage;

use crate::kinds::Kind;
pub use budget::{Charge, Limit, Limits, Meter};
pub use context::{Context, ContextError, Definition};

/// Wire syntax/type errors carry fixed reasons rather than request contents.
/// A caller-supplied meter keeps its original typed error intact.
#[derive(Debug, thiserror::Error)]
pub enum Error<E = Limit> {
    #[error("invalid Jeto at byte {offset}: {reason}")]
    Invalid { offset: usize, reason: &'static str },
    #[error("Jeto spec is not available in the admitted context")]
    UnknownSpec,
    #[error("Jeto allocation failed")]
    Allocation,
    #[error("Jeto resource limit or interruption: {0}")]
    Budget(E),
}
fn invalid<E>(reason: &'static str) -> Error<E> {
    Error::Invalid { offset: 0, reason }
}
fn charge<M: Meter>(meter: &mut M, cost: Charge) -> Result<(), Error<M::Error>> {
    meter.charge(cost).map_err(Error::Budget)
}

/// Decode with standalone cumulative limits. An explicit expected type must be
/// admitted even if the incoming value has its own explicit type. Fitting is a
/// separate operation: explicit boxes and ordinary Bool keep their own kind.
pub fn decode(
    bytes: &[u8],
    context: &Context,
    expected: Option<&str>,
    limits: Limits,
) -> Result<Kind, Error> {
    let mut meter = budget::Bounded::new(limits).map_err(Error::Budget)?;
    decode_metered(bytes, context, expected, &mut meter)
}

/// Decode using the caller's original cumulative meter. Charges precede source
/// copies and container growth; Depth checks are also cancellation checkpoints.
pub fn decode_metered<M: Meter>(
    bytes: &[u8],
    context: &Context,
    expected: Option<&str>,
    meter: &mut M,
) -> Result<Kind, Error<M::Error>> {
    if let Some(name) = expected {
        resolve(context, name, meter)?;
    }
    let wire = parser::parse(bytes, meter)?;
    decode::value(&wire, context, expected, meter, 0)
}

/// Decode transport scalar text without manufacturing an escaped JSON buffer.
/// The caller supplies the same meter and explicit expected type as for JSON.
/// Text resembling numbers or null remains scalar text under that context.
pub fn decode_scalar_text_metered<M: Meter>(
    text: &str,
    context: &Context,
    expected: &str,
    meter: &mut M,
) -> Result<Kind, Error<M::Error>> {
    charge(meter, Charge::Input(text.len()))?;
    charge(meter, Charge::Depth(0))?;
    charge(meter, Charge::Nodes(1))?;
    let class = resolve(context, expected, meter)?;
    text_charge(text, context, meter)?;
    scalar::string(text, context, Some(expected), Some(class))
}

/// Boxing affects wire shape; it never grants authority to discard identity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Boxing {
    #[default]
    Auto,
    None,
    All,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    TypeErased,
    ReferenceDisplay,
    NullDictMember,
    ReservedMetadata,
    UnsupportedScalar,
    InvalidScalar,
    CatalogMismatch,
    InvalidGridShape,
    ContextChangesIdentity,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub path: crate::kinds::ValuePath,
    pub reason: Reason,
}
/// Loss is deliberate and observable. Unsupported carries no output bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Encoding {
    Exact(Vec<u8>),
    Lossy { bytes: Vec<u8>, issues: Vec<Issue> },
    Unsupported { issues: Vec<Issue> },
}
impl Encoding {
    /// Consume bytes only when every native field is preserved by this context.
    pub fn into_exact(self) -> Result<Vec<u8>, Vec<Issue>> {
        match self {
            Self::Exact(bytes) => Ok(bytes),
            Self::Lossy { issues, .. } | Self::Unsupported { issues } => Err(issues),
        }
    }
}
/// Encode transactionally. Exact and deliberate lossy results are distinct;
/// unsupported native information never produces a partial output document.
pub fn encode(
    value: &Kind,
    context: &Context,
    expected: Option<&str>,
    boxing: Boxing,
    limits: Limits,
) -> Result<Encoding, Error> {
    let mut meter = budget::Bounded::new(limits).map_err(Error::Budget)?;
    encode_metered(value, context, expected, boxing, &mut meter)
}
pub fn encode_metered<M: Meter>(
    value: &Kind,
    context: &Context,
    expected: Option<&str>,
    boxing: Boxing,
    meter: &mut M,
) -> Result<Encoding, Error<M::Error>> {
    if let Some(name) = expected {
        resolve(context, name, meter)?;
    }
    encode::run(value, context, expected, boxing, meter)
}
fn resolve<'a, M: Meter>(
    context: &'a Context,
    name: &str,
    meter: &mut M,
) -> Result<&'a context::Class, Error<M::Error>> {
    // The context admits at most 274 entries. This bounds tree comparisons,
    // including long common prefixes, before resolving an untrusted qname.
    charge(
        meter,
        Charge::Work(name.len().saturating_mul(64).saturating_add(1)),
    )?;
    context.lookup(name).ok_or(Error::UnknownSpec)
}
fn text_charge<M: Meter>(
    text: &str,
    context: &Context,
    meter: &mut M,
) -> Result<(), Error<M::Error>> {
    // Immutable DFA membership and primitive scalar parsing perform linear
    // scans. Matcher tables were bounded and admitted with the context.
    charge(
        meter,
        Charge::Work(text.len().saturating_mul(8).saturating_add(1)),
    )?;
    charge(
        meter,
        Charge::Retained(
            text.len()
                .saturating_mul(4)
                .saturating_add(context.catalog().len())
                .saturating_add(context.revision().len())
                .saturating_add(512),
        ),
    )
}

fn tree_charge<M: Meter>(key: &str, count: usize, meter: &mut M) -> Result<(), Error<M::Error>> {
    let levels = usize::BITS as usize - count.saturating_add(1).leading_zeros() as usize;
    charge(
        meter,
        Charge::Work(
            key.len()
                .saturating_add(1)
                .saturating_mul(levels.saturating_mul(16).saturating_add(1)),
        ),
    )
}
fn member_type<'a, M: Meter>(
    class: Option<&'a context::Class>,
    name: &str,
    meter: &mut M,
) -> Result<Option<&'a str>, Error<M::Error>> {
    if let Some(context::Class::Dict(members)) = class {
        tree_charge(name, members.len(), meter)?;
        Ok(members.get(name).map(String::as_str))
    } else {
        Ok(None)
    }
}

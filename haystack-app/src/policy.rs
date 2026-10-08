use crate::{CatalogKind, Principal, ReadError, ReadOperation};
use haystack_core::kinds::NominalScalar;
use std::sync::Arc;

/// Supply an immutable policy snapshot on every request/page. Implementations
/// are trusted local code: callbacks must be quick and nonblocking. Scope keys
/// must change whenever any authorization decision or effective principal scope
/// changes. Keys are retained server-side and never copied into cursor tokens.
pub trait ReadPolicy: Send + Sync + 'static {
    fn snapshot(&self, principal: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError>;
}
pub trait PolicySnapshot: Send + Sync + 'static {
    fn scope_key(&self) -> &str;
    /// Explicit executable-function permission, independent of catalog visibility.
    fn function(&self, function: &crate::FunctionIdentity) -> bool;
    fn operation(&self, operation: ReadOperation) -> bool;
    fn entity(&self, id: &str) -> bool;
    fn tag(&self, entity: &str, tag: &str) -> bool;
    fn reference(&self, target: &str) -> bool;
    fn reference_display(&self, target: &str) -> bool;
    fn catalog(&self, kind: CatalogKind, name: &str) -> bool;
    fn nominal_provenance(&self, value: &NominalScalar) -> bool;
}
/// Explicit unrestricted policy for trusted embedding or compatibility use.
/// Scoped applications should supply their own versioned immutable snapshot.
#[derive(Debug, Clone, Copy)]
pub struct AllowAll;
impl ReadPolicy for AllowAll {
    fn snapshot(&self, _: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
        Ok(Arc::new(Self))
    }
}
impl PolicySnapshot for AllowAll {
    fn function(&self, _: &crate::FunctionIdentity) -> bool {
        true
    }
    fn scope_key(&self) -> &str {
        "explicit-allow-all-v1"
    }
    fn operation(&self, _: ReadOperation) -> bool {
        true
    }
    fn entity(&self, _: &str) -> bool {
        true
    }
    fn tag(&self, _: &str, _: &str) -> bool {
        true
    }
    fn reference(&self, _: &str) -> bool {
        true
    }
    fn reference_display(&self, _: &str) -> bool {
        true
    }
    fn catalog(&self, _: CatalogKind, _: &str) -> bool {
        true
    }
    fn nominal_provenance(&self, _: &NominalScalar) -> bool {
        true
    }
}

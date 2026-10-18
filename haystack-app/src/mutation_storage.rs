//! The small dispatch and principal-binding boundary shared by SQL authorities.
use super::*;
use crate::storage_value::PayloadError;
use haystack_core::{
    codecs::{entity::EntityWire, typed},
    kinds::Kind,
};

impl Binding {
    pub(super) fn storage_key(&self) -> Result<Vec<u8>, PayloadError> {
        let (kind, subject) = match &self.principal {
            PrincipalKey::Anonymous => ("anonymous", ""),
            PrincipalKey::Authenticated(s) => ("authenticated", s.as_str()),
            PrincipalKey::Embedding(s) => ("embedding", s.as_str()),
        };
        typed::encode(&Kind::List(vec![
            Kind::Str(kind.into()),
            Kind::Str(subject.into()),
            self.identity
                .to_kind()
                .map_err(|_| PayloadError::Incompatible)?,
        ]))
        .map_err(|_| PayloadError::Limit)
    }
    pub(super) fn from_storage_key(bytes: &[u8]) -> Result<Self, PayloadError> {
        let Kind::List(parts) = typed::decode(bytes).map_err(|_| PayloadError::Incompatible)?
        else {
            return Err(PayloadError::Incompatible);
        };
        let [Kind::Str(kind), Kind::Str(subject), identity] = parts.as_slice() else {
            return Err(PayloadError::Incompatible);
        };
        let principal = match kind.as_str() {
            "anonymous" if subject.is_empty() => PrincipalKey::Anonymous,
            "authenticated" => PrincipalKey::Authenticated(subject.clone()),
            "embedding" => PrincipalKey::Embedding(subject.clone()),
            _ => return Err(PayloadError::Incompatible),
        };
        let result = Self {
            principal,
            identity: OperationIdentity::from_kind(identity)
                .map_err(|_| PayloadError::Incompatible)?,
        };
        if result.storage_key()? != bytes {
            return Err(PayloadError::Incompatible);
        };
        Ok(result)
    }
}

#[derive(Clone)]
pub(super) enum Backend {
    #[cfg(feature = "postgres")]
    Postgres(Arc<crate::postgres::storage::Backend>),
    #[cfg(feature = "sqlite")]
    Sqlite(Arc<crate::sqlite::storage::Backend>),
}
impl Backend {
    pub(super) fn qualification(&self) -> ReceiptQualification {
        match self {
            #[cfg(feature = "postgres")]
            Self::Postgres(_) => ReceiptQualification::PostgresDurable,
            #[cfg(feature = "sqlite")]
            Self::Sqlite(_) => ReceiptQualification::SqliteDurable,
        }
    }
    pub(super) fn publication_guard(
        &self,
        budget: &Budget,
    ) -> Result<parking_lot::MutexGuard<'_, ()>, ReadError> {
        match self {
            #[cfg(feature = "postgres")]
            Self::Postgres(backend) => backend
                .publication_guard(budget.deadline)
                .map_err(|_| ReadError::Unavailable),
            #[cfg(feature = "sqlite")]
            Self::Sqlite(backend) => backend
                .publication_guard(&crate::sqlite::storage::Stop::from_budget(budget))
                .map_err(|_| ReadError::Unavailable),
        }
    }
    pub(super) fn reserve(
        &self,
        binding: &Binding,
        request: &[u8],
        pending: &MutationOutcome,
        budget: &Budget,
    ) -> Result<Option<Record>, ReadError> {
        match self {
            #[cfg(feature = "postgres")]
            Self::Postgres(backend) => {
                postgres::reserve(backend, binding, request, pending, budget.deadline)
                    .map_err(|_| ReadError::Unavailable)
            }
            #[cfg(feature = "sqlite")]
            Self::Sqlite(backend) => sqlite::reserve(backend, binding, request, pending, budget)
                .map_err(|_| ReadError::Unavailable),
        }
    }
    pub(super) fn lookup(
        &self,
        binding: &Binding,
        budget: &Budget,
    ) -> Result<Option<Record>, ReadError> {
        match self {
            #[cfg(feature = "postgres")]
            Self::Postgres(backend) => postgres::lookup(backend, binding, budget.deadline)
                .map_err(|_| ReadError::Unavailable),
            #[cfg(feature = "sqlite")]
            Self::Sqlite(backend) => {
                sqlite::lookup(backend, binding, budget).map_err(|_| ReadError::Unavailable)
            }
        }
    }
    pub(super) fn publish(self, prepared: PreparedMutation) -> MutationOutcome {
        match self {
            #[cfg(feature = "postgres")]
            Self::Postgres(backend) => prepared.publish_postgres(&backend),
            #[cfg(feature = "sqlite")]
            Self::Sqlite(backend) => prepared.publish_sqlite(&backend),
        }
    }
    pub(super) fn reject(
        self,
        prepared: PreparedMutation,
        reason: RejectionReason,
    ) -> MutationOutcome {
        match self {
            #[cfg(feature = "postgres")]
            Self::Postgres(backend) => prepared.reject_postgres(&backend, reason),
            #[cfg(feature = "sqlite")]
            Self::Sqlite(backend) => prepared.reject_sqlite(&backend, reason),
        }
    }
}

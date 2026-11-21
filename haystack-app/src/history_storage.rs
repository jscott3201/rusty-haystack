//! Private durable history dispatch and canonical request/receipt bindings.
use super::*;
use crate::storage_value::PayloadError;
use haystack_core::{codecs::typed, kinds::Kind};

pub(super) fn encode_outcome(outcome: &HistoryWriteOutcome) -> Result<Vec<u8>, PayloadError> {
    typed::encode(&Kind::Grid(Box::new(
        outcome_grid(outcome).map_err(|_| PayloadError::Incompatible)?,
    )))
    .map_err(|_| PayloadError::Limit)
}
pub(super) fn decode_outcome(bytes: &[u8]) -> Result<HistoryWriteOutcome, PayloadError> {
    let Kind::Grid(grid) = typed::decode(bytes).map_err(|_| PayloadError::Incompatible)? else {
        return Err(PayloadError::Incompatible);
    };
    let outcome = outcome_from_grid(&grid).map_err(|_| PayloadError::Incompatible)?;
    if encode_outcome(&outcome)? != bytes {
        return Err(PayloadError::Incompatible);
    };
    Ok(outcome)
}
impl HistoryBinding {
    pub(super) fn storage_key(&self) -> Result<Vec<u8>, PayloadError> {
        let (kind, subject) = match &self.principal {
            PrincipalKey::Anonymous => ("anonymous", ""),
            PrincipalKey::Authenticated(s) => ("authenticated", s.as_str()),
            PrincipalKey::Embedding(s) => ("embedding", s.as_str()),
        };
        typed::encode(&Kind::List(vec![
            Kind::Str(kind.into()),
            Kind::Str(subject.into()),
            Kind::Grid(Box::new(
                lookup_grid(&self.identity).map_err(|_| PayloadError::Incompatible)?,
            )),
        ]))
        .map_err(|_| PayloadError::Limit)
    }
    pub(super) fn from_storage_key(bytes: &[u8]) -> Result<Self, PayloadError> {
        let Kind::List(parts) = typed::decode(bytes).map_err(|_| PayloadError::Incompatible)?
        else {
            return Err(PayloadError::Incompatible);
        };
        let [Kind::Str(kind), Kind::Str(subject), Kind::Grid(grid)] = parts.as_slice() else {
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
            identity: lookup_from_grid(grid).map_err(|_| PayloadError::Incompatible)?,
        };
        if result.storage_key()? != bytes {
            return Err(PayloadError::Incompatible);
        };
        Ok(result)
    }
}
pub(super) fn record(
    request_bytes: Vec<u8>,
    outcome_bytes: Vec<u8>,
    binding: &HistoryBinding,
) -> Result<HistoryRecord, PayloadError> {
    let request =
        decode_canonical_request(&request_bytes).map_err(|_| PayloadError::Incompatible)?;
    let outcome = decode_outcome(&outcome_bytes)?;
    if request.identity != binding.identity || outcome.identity() != &binding.identity {
        return Err(PayloadError::Incompatible);
    };
    Ok(HistoryRecord {
        canonical: request_bytes.into(),
        request: Arc::new(request),
        outcome,
    })
}

#[derive(Clone)]
pub(crate) enum Backend {
    #[cfg(feature = "postgres")]
    Postgres(Arc<crate::postgres::storage::Backend>),
    #[cfg(feature = "sqlite")]
    Sqlite(Arc<crate::sqlite::storage::Backend>),
}
impl Backend {
    pub(crate) fn qualification(&self) -> HistoryReceiptQualification {
        match self {
            #[cfg(feature = "postgres")]
            Self::Postgres(_) => HistoryReceiptQualification::PostgresDurable,
            #[cfg(feature = "sqlite")]
            Self::Sqlite(_) => HistoryReceiptQualification::SqliteDurable,
        }
    }
    pub(crate) fn check(&self) -> Result<(), crate::HistoryProviderError> {
        match self {
            #[cfg(feature = "postgres")]
            Self::Postgres(backend) => backend
                .check()
                .map_err(|_| crate::HistoryProviderError::Stopped),
            #[cfg(feature = "sqlite")]
            Self::Sqlite(backend) => backend
                .check()
                .map_err(|_| crate::HistoryProviderError::Stopped),
        }
    }
    pub(crate) fn point_incarnation(&self, authority: [u8; 16], id: &str) -> [u8; 16] {
        use sha2::{Digest, Sha256};
        let domain: &[u8] = match self {
            #[cfg(feature = "postgres")]
            Self::Postgres(_) => b"rusty-haystack-postgres-point-v1\0",
            #[cfg(feature = "sqlite")]
            Self::Sqlite(_) => b"rusty-haystack-sqlite-history-point-v2\0",
        };
        let mut hash = Sha256::new();
        hash.update(domain);
        hash.update(authority);
        hash.update(id.as_bytes());
        let mut incarnation = [0; 16];
        incarnation.copy_from_slice(&hash.finalize()[..16]);
        incarnation
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
    pub(super) fn lookup(
        &self,
        binding: &HistoryBinding,
        budget: &Budget,
    ) -> Result<Option<HistoryRecord>, ReadError> {
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
    pub(super) fn reserve(
        &self,
        binding: &HistoryBinding,
        canonical: &[u8],
        outcome: &HistoryWriteOutcome,
        budget: &Budget,
    ) -> Result<Option<HistoryRecord>, ReadError> {
        match self {
            #[cfg(feature = "postgres")]
            Self::Postgres(backend) => {
                postgres::reserve(backend, binding, canonical, outcome, budget.deadline)
                    .map_err(|_| ReadError::Unavailable)
            }
            #[cfg(feature = "sqlite")]
            Self::Sqlite(backend) => sqlite::reserve(backend, binding, canonical, outcome, budget)
                .map_err(|_| ReadError::Unavailable),
        }
    }
    pub(super) fn publish(self, prepared: PreparedHistoryMutation) -> HistoryWriteOutcome {
        match self {
            #[cfg(feature = "postgres")]
            Self::Postgres(backend) => prepared.publish_postgres(&backend),
            #[cfg(feature = "sqlite")]
            Self::Sqlite(backend) => prepared.publish_sqlite(&backend),
        }
    }
    pub(super) fn reject(
        self,
        prepared: PreparedHistoryMutation,
        reason: HistoryWriteRejection,
    ) -> HistoryWriteOutcome {
        match self {
            #[cfg(feature = "postgres")]
            Self::Postgres(backend) => prepared.reject_postgres(&backend, reason),
            #[cfg(feature = "sqlite")]
            Self::Sqlite(backend) => prepared.reject_sqlite(&backend, reason),
        }
    }
}

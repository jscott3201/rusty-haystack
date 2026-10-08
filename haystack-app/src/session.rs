//! Noncredential session authority. Only trusted authentication adapters mint an
//! authenticated handle; request bodies cannot construct or recover one by ID.
use crate::{Principal, ReadError, SubscriptionResync};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct SubscriptionSession {
    inner: Arc<SessionInner>,
}
struct SessionInner {
    id: [u8; 16],
    principal: Principal,
    expires: Option<Instant>,
    revoked: CancellationToken,
    scoped: bool,
}
impl std::fmt::Debug for SubscriptionSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubscriptionSession")
            .field("active", &self.is_active())
            .field("scoped", &self.inner.scoped)
            .finish_non_exhaustive()
    }
}
impl SubscriptionSession {
    /// Trusted authentication boundary: mint once, atomically with bearer issuance.
    /// Clones retain the same identity and revocation; no credential is retained.
    pub fn authenticated(principal: Principal, expires: Instant) -> Result<Self, ReadError> {
        if !matches!(principal, Principal::Authenticated { .. }) {
            return Err(ReadError::Forbidden);
        }
        Self::new(principal, Some(expires), true)
    }
    /// Explicit trusted embedding authority. A new handle always means a new
    /// session, including when its subject is the same as an earlier handle.
    pub fn trusted(subject: impl Into<String>, lifetime: Duration) -> Result<Self, ReadError> {
        if lifetime.is_zero() {
            return Err(ReadError::InvalidLimits);
        }
        let expires = Instant::now()
            .checked_add(lifetime)
            .ok_or(ReadError::InvalidLimits)?;
        Self::new(
            Principal::TrustedEmbedding {
                subject: subject.into(),
            },
            Some(expires),
            true,
        )
    }
    fn new(
        principal: Principal,
        expires: Option<Instant>,
        scoped: bool,
    ) -> Result<Self, ReadError> {
        if let Principal::Authenticated { permissions, .. } = &principal
            && permissions.len() > 1024
        {
            return Err(ReadError::InvalidLimits);
        }
        if principal.bytes() > 65_536 {
            return Err(ReadError::InvalidLimits);
        }
        Ok(Self {
            inner: Arc::new(SessionInner {
                id: rand::random(),
                principal,
                expires,
                revoked: CancellationToken::new(),
                scoped,
            }),
        })
    }
    pub(crate) fn legacy_anonymous() -> Self {
        Self::new(Principal::Anonymous, None, false).expect("bounded anonymous identity")
    }
    pub fn principal(&self) -> &Principal {
        &self.inner.principal
    }
    pub fn expires_at(&self) -> Option<Instant> {
        self.inner.expires
    }
    pub fn close(&self) {
        self.inner.revoked.cancel();
    }
    pub fn is_active(&self) -> bool {
        self.reason().is_none()
    }
    pub async fn closed(&self) {
        tokio::select! {
            _=self.inner.revoked.cancelled()=>{},
            _=async {match self.inner.expires {Some(expires)=>tokio::time::sleep_until(tokio::time::Instant::from_std(expires)).await,None=>std::future::pending::<()>().await}}=>{},
        }
    }
    pub(crate) fn id(&self) -> [u8; 16] {
        self.inner.id
    }
    pub(crate) fn scoped(&self) -> bool {
        self.inner.scoped
    }
    pub(crate) fn same_session(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
    pub(crate) fn matches(&self, principal: &Principal) -> bool {
        &self.inner.principal == principal
    }
    pub(crate) fn reason(&self) -> Option<SubscriptionResync> {
        if self.inner.revoked.is_cancelled() {
            Some(SubscriptionResync::Revoked)
        } else if self
            .inner
            .expires
            .is_some_and(|expires| Instant::now() >= expires)
        {
            Some(SubscriptionResync::SessionExpired)
        } else {
            None
        }
    }
}

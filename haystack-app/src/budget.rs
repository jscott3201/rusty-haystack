use crate::{BudgetKind, ReadError, ReadLimits};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

pub(crate) struct Budget {
    pub limits: Arc<ReadLimits>,
    pub deadline: Instant,
    pub cancel: CancellationToken,
    pub owner_cancel: Option<CancellationToken>,
    pub owner_sealed: Option<CancellationToken>,
    pub lease: Option<Arc<crate::service::WorkLease>>,
    work: usize,
    retained: usize,
    values: usize,
    candidates: usize,
    forward: usize,
    inverse: usize,
}
impl Budget {
    pub fn new(limits: Arc<ReadLimits>, deadline: Instant, cancel: CancellationToken) -> Self {
        Self {
            limits,
            deadline,
            cancel,
            owner_cancel: None,
            owner_sealed: None,
            lease: None,
            work: 0,
            retained: 0,
            values: 0,
            candidates: 0,
            forward: 0,
            inverse: 0,
        }
    }
    pub fn retained_remaining(&self) -> usize {
        self.limits.max_retained_bytes.saturating_sub(self.retained)
    }
    pub fn check(&self) -> Result<(), ReadError> {
        if Instant::now() >= self.deadline {
            Err(ReadError::Deadline)
        } else if self.cancel.is_cancelled()
            || self
                .owner_cancel
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
        {
            Err(ReadError::Cancelled)
        } else {
            Ok(())
        }
    }
    pub async fn cancelled(&self) {
        tokio::select! {
            _ = self.cancel.cancelled() => {},
            _ = async {
                match &self.owner_cancel {
                    Some(token) => token.cancelled().await,
                    None => std::future::pending::<()>().await,
                }
            } => {},
        }
    }
    pub fn wait_quantum(&self) -> Result<Duration, ReadError> {
        self.check()?;
        Ok(self
            .deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(5)))
    }
    pub fn charge(&mut self, kind: BudgetKind, amount: usize) -> Result<(), ReadError> {
        self.check()?;
        let (used, limit) = match kind {
            BudgetKind::Work => (&mut self.work, self.limits.max_work),
            BudgetKind::Retained => (&mut self.retained, self.limits.max_retained_bytes),
            BudgetKind::Values => (&mut self.values, self.limits.max_value_nodes),
            BudgetKind::Candidates => (&mut self.candidates, self.limits.max_candidates),
            BudgetKind::Forward => (&mut self.forward, self.limits.max_forward_edges),
            BudgetKind::Inverse => (&mut self.inverse, self.limits.max_inverse_edges),
            _ => return Err(ReadError::Budget(kind)),
        };
        if amount > limit.saturating_sub(*used) {
            return Err(ReadError::Budget(kind));
        }
        *used += amount;
        Ok(())
    }
    pub fn depth(&self, depth: usize) -> Result<(), ReadError> {
        self.check()?;
        if depth > self.limits.max_value_depth {
            Err(ReadError::Budget(BudgetKind::Depth))
        } else {
            Ok(())
        }
    }
    pub fn copy_string(&mut self, value: &str) -> Result<String, ReadError> {
        self.charge(BudgetKind::Work, value.len().saturating_add(1))?;
        self.charge(BudgetKind::Retained, value.len().saturating_add(32))?;
        Ok(value.to_owned())
    }
}

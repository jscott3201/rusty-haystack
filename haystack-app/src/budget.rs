use crate::{BudgetKind, ReadError, ReadLimits};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

/// Revocation belongs to one authenticated invocation. The exception is set
/// only by the exact close handler after its acknowledgment is fully encoded.
#[derive(Clone)]
pub(crate) struct SessionFence {
    pub session: crate::SubscriptionSession,
    acknowledgment: Arc<AtomicBool>,
}
impl SessionFence {
    pub fn new(session: crate::SubscriptionSession) -> Self {
        Self {
            session,
            acknowledgment: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn check(&self) -> Result<(), ReadError> {
        if self
            .session
            .expires_at()
            .is_some_and(|expires| Instant::now() >= expires)
            || (!self.session.is_active() && !self.acknowledgment.load(Ordering::Acquire))
        {
            Err(ReadError::Forbidden)
        } else {
            Ok(())
        }
    }
    pub async fn cancelled(&self) {
        self.session.closed().await;
        if self.acknowledgment.load(Ordering::Acquire) {
            match self.session.expires_at() {
                Some(expires) => {
                    tokio::time::sleep_until(tokio::time::Instant::from_std(expires)).await
                }
                None => std::future::pending::<()>().await,
            }
        }
    }
    pub fn close(&self) -> Result<(), ReadError> {
        self.check()?;
        if self.session.close_for_ack(&self.acknowledgment) {
            Ok(())
        } else {
            Err(ReadError::Forbidden)
        }
    }
}

/// Clones share cumulative counters and the original work lease.
#[derive(Clone)]
pub(crate) struct Budget {
    #[cfg(test)]
    pub typed_encode_hook: Option<Arc<dyn Fn() + Send + Sync>>,
    pub limits: Arc<ReadLimits>,
    pub deadline: Instant,
    pub cancel: CancellationToken,
    pub owner_cancel: Option<CancellationToken>,
    pub owner_sealed: Option<CancellationToken>,
    pub session: Option<SessionFence>,
    pub lease: Option<Arc<crate::service::WorkLease>>,
    usage: Arc<parking_lot::Mutex<Usage>>,
}
#[derive(Default)]
struct Usage {
    input: usize,
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
            #[cfg(test)]
            typed_encode_hook: None,
            limits,
            deadline,
            cancel,
            owner_cancel: None,
            owner_sealed: None,
            session: None,
            lease: None,
            usage: Arc::new(parking_lot::Mutex::new(Usage::default())),
        }
    }
    pub fn work_remaining(&self) -> usize {
        self.limits.max_work.saturating_sub(self.usage.lock().work)
    }
    pub fn retained_remaining(&self) -> usize {
        self.limits
            .max_retained_bytes
            .saturating_sub(self.usage.lock().retained)
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
        } else if let Some(session) = &self.session {
            session.check()
        } else {
            Ok(())
        }
    }
    pub async fn cancelled(&self) {
        tokio::select! {
            _ = self.cancel.cancelled() => {},
            _ = async { match &self.session { Some(session) => session.cancelled().await, None => std::future::pending::<()>().await } } => {},
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
        let mut usage = self.usage.lock();
        let (used, limit) = match kind {
            BudgetKind::Input => (&mut usage.input, self.limits.max_input_bytes),
            BudgetKind::Work => (&mut usage.work, self.limits.max_work),
            BudgetKind::Retained => (&mut usage.retained, self.limits.max_retained_bytes),
            BudgetKind::Values => (&mut usage.values, self.limits.max_value_nodes),
            BudgetKind::Candidates => (&mut usage.candidates, self.limits.max_candidates),
            BudgetKind::Forward => (&mut usage.forward, self.limits.max_forward_edges),
            BudgetKind::Inverse => (&mut usage.inverse, self.limits.max_inverse_edges),
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_budget_keeps_cumulative_charges_and_cancellation() {
        let limits = Arc::new(ReadLimits {
            max_work: 100,
            max_retained_bytes: 100,
            ..ReadLimits::default()
        });
        let mut producer = Budget::new(
            limits,
            Instant::now() + Duration::from_secs(1),
            CancellationToken::new(),
        );
        producer.charge(BudgetKind::Retained, 20).unwrap();
        let mut collector = producer.clone();
        producer.charge(BudgetKind::Retained, 60).unwrap();
        collector.charge(BudgetKind::Work, 70).unwrap();
        assert_eq!(collector.retained_remaining(), 20);
        assert_eq!(producer.work_remaining(), 30);
        assert!(matches!(
            collector.charge(BudgetKind::Retained, 21),
            Err(ReadError::Budget(BudgetKind::Retained))
        ));
        producer.cancel.cancel();
        assert!(matches!(collector.check(), Err(ReadError::Cancelled)));
    }
}

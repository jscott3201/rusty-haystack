use haystack_core::codecs::jeto::{self, Meter};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::ClientError;

/// The original absolute deadline and request/owner cancellation authorities.
/// Checks are synchronous so they also run inside a single future poll.
#[derive(Clone)]
pub struct ReadControl {
    deadline: Instant,
    cancelled: CancellationToken,
    closing: CancellationToken,
    #[cfg(test)]
    pub(super) clock: Option<std::sync::Arc<std::sync::Mutex<Instant>>>,
    #[cfg(test)]
    pub(super) codec_checkpoint: Option<std::sync::Arc<dyn Fn(jeto::Charge) + Send + Sync>>,
}
impl ReadControl {
    pub fn new(
        deadline: Instant,
        cancelled: CancellationToken,
        closing: CancellationToken,
    ) -> Self {
        Self {
            deadline,
            cancelled,
            closing,
            #[cfg(test)]
            clock: None,
            #[cfg(test)]
            codec_checkpoint: None,
        }
    }
    /// Check both cancellation authorities and the unchanged absolute deadline.
    pub fn check(&self) -> Result<(), ClientError> {
        if self.cancelled.is_cancelled() || self.closing.is_cancelled() {
            return Err(ClientError::ConnectionClosed);
        }
        #[cfg(not(test))]
        let now = Instant::now();
        #[cfg(test)]
        let now = self
            .clock
            .as_ref()
            .map_or_else(Instant::now, |clock| *clock.lock().unwrap());
        if now >= self.deadline {
            return Err(ClientError::Timeout(std::time::Duration::ZERO));
        }
        Ok(())
    }
    pub(super) async fn stopped(&self) -> ClientError {
        tokio::select! {
            biased;
            _ = self.cancelled.cancelled() => ClientError::ConnectionClosed,
            _ = self.closing.cancelled() => ClientError::ConnectionClosed,
            _ = tokio::time::sleep_until(self.deadline) => ClientError::Timeout(std::time::Duration::ZERO),
        }
    }
    pub(crate) fn meter(&self) -> Result<ControlledMeter<'_>, ClientError> {
        self.check()?;
        Ok(ControlledMeter {
            control: self,
            bounded: jeto::Bounded::new(jeto::Limits::default())
                .map_err(|_| super::invalid("invalid H5 codec bounds"))?,
        })
    }
}

pub(crate) struct ControlledMeter<'a> {
    control: &'a ReadControl,
    bounded: jeto::Bounded,
}
impl Meter for ControlledMeter<'_> {
    type Error = ClientError;
    fn charge(&mut self, charge: jeto::Charge) -> Result<(), ClientError> {
        #[cfg(test)]
        if let Some(checkpoint) = &self.control.codec_checkpoint {
            checkpoint(charge);
        }
        self.control.check()?;
        self.bounded
            .charge(charge)
            .map_err(|_| super::invalid("H5 request or response exceeds codec bounds"))
    }
}

use super::SqliteError;
use std::{path::PathBuf, time::Duration};

/// Finite local-file recovery, transaction, and cleanup limits.
#[derive(Debug, Clone)]
pub struct SqliteLimits {
    /// Initial connection configuration and bounded recovery deadline.
    pub operation_timeout: Duration,
    /// Fresh rollback, receipt proof, publication and close cleanup allowance.
    pub cleanup_timeout: Duration,
    pub recovery: haystack_core::graph::RecoveryLimits,
    pub max_receipts: usize,
    pub max_receipt_bytes: usize,
}
impl Default for SqliteLimits {
    fn default() -> Self {
        Self {
            operation_timeout: Duration::from_secs(5),
            cleanup_timeout: Duration::from_secs(2),
            recovery: Default::default(),
            max_receipts: 1024,
            max_receipt_bytes: 64 * 1024 * 1024,
        }
    }
}

/// One local file and its persistent `.owner.lock` sidecar. Parents must exist.
/// Network filesystems, hard links, external file replacement, and concurrent
/// writers outside this application are outside this single-owner profile.
#[derive(Debug, Clone)]
pub struct SqliteConfig {
    pub path: PathBuf,
    pub limits: SqliteLimits,
    #[cfg(test)]
    pub(crate) recovery_end: Option<std::sync::Arc<super::qualification::Gate>>,
}
impl SqliteConfig {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            limits: SqliteLimits::default(),
            #[cfg(test)]
            recovery_end: None,
        }
    }
    pub(crate) fn validate(&self) -> Result<(), SqliteError> {
        let bounded =
            |duration: Duration| !duration.is_zero() && duration <= Duration::from_secs(60);
        if !self.path.is_absolute()
            || self.path.file_name().is_none()
            || !bounded(self.limits.operation_timeout)
            || !bounded(self.limits.cleanup_timeout)
            || self.limits.recovery.max_rows == 0
            || self.limits.recovery.max_rows > 100_000
            || self.limits.recovery.max_changes == 0
            || self.limits.recovery.max_changes > 100_000
            || self.limits.recovery.max_bytes == 0
            || self.limits.recovery.max_bytes > 256 * 1024 * 1024
            || self.limits.recovery.max_work == 0
            || self.limits.recovery.max_work > 64_000_000
            || self.limits.max_receipts == 0
            || self.limits.max_receipts > 100_000
            || self.limits.max_receipt_bytes == 0
            || self.limits.max_receipt_bytes > 256 * 1024 * 1024
        {
            return Err(SqliteError::Configuration);
        }
        Ok(())
    }
}

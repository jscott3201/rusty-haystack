//! Internal fault seam. Production checkpoints are zero-sized no-ops; tests
//! can stop an owned worker at actual I/O/publication boundaries.
use crate::Result;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub(crate) enum Phase {
    Pull,
    Write,
    Footer,
    FileSync,
    SourceJoined,
    DirectorySync,
    ManifestWrite,
    ManifestSync,
    BeforePublish,
    AfterPublish,
    PublishedDirectorySync,
    WaitingPull,
    Cleanup,
}
#[derive(Clone, Default)]
pub(crate) struct Checkpoints {
    #[cfg(test)]
    pub hook: Option<std::sync::Arc<Hook>>,
}
impl Checkpoints {
    pub fn hit(&self, phase: Phase) -> Result<()> {
        #[cfg(test)]
        if let Some(hook) = &self.hook {
            return hook.hit(phase);
        }
        let _ = phase;
        Ok(())
    }
}
#[cfg(test)]
pub(crate) enum Action {
    Fail,
    Hold,
    Cancel(tokio_util::sync::CancellationToken),
    #[cfg(unix)]
    CrashReady(std::path::PathBuf),
}
#[cfg(test)]
pub(crate) struct Hook {
    phase: Phase,
    occurrence: usize,
    action: Action,
    pub reached: tokio::sync::Notify,
    counts: [std::sync::atomic::AtomicUsize; 13],
    release: (std::sync::Mutex<bool>, std::sync::Condvar),
}
#[cfg(test)]
impl Hook {
    pub fn new(phase: Phase, occurrence: usize, action: Action) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            phase,
            occurrence,
            action,
            reached: tokio::sync::Notify::new(),
            counts: std::array::from_fn(|_| std::sync::atomic::AtomicUsize::new(0)),
            release: (std::sync::Mutex::new(false), std::sync::Condvar::new()),
        })
    }
    pub fn count(&self, phase: Phase) -> usize {
        self.counts[phase as usize].load(std::sync::atomic::Ordering::SeqCst)
    }
    pub fn release(&self) {
        *self.release.0.lock().unwrap() = true;
        self.release.1.notify_all();
    }
    fn hit(&self, phase: Phase) -> Result<()> {
        let n = self.counts[phase as usize].fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        if phase != self.phase || n != self.occurrence {
            return Ok(());
        }
        self.reached.notify_one();
        match &self.action {
            Action::Fail => Err(crate::Error::Io(std::io::ErrorKind::Other)),
            Action::Cancel(token) => {
                token.cancel();
                Ok(())
            }
            Action::Hold => {
                let (guard, timeout) = self
                    .release
                    .1
                    .wait_timeout_while(
                        self.release.0.lock().unwrap(),
                        std::time::Duration::from_secs(15),
                        |v| !*v,
                    )
                    .unwrap();
                if timeout.timed_out() || !*guard {
                    Err(crate::Error::Worker)
                } else {
                    Ok(())
                }
            }
            #[cfg(unix)]
            Action::CrashReady(path) => {
                std::fs::write(path, b"ready")?;
                loop {
                    std::thread::park();
                }
            }
        }
    }
}

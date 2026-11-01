use crate::checkpoint::{Checkpoints, Phase};
use crate::{
    Error, Result,
    format::{self, Limits, Manifest},
    writer::Writer,
};
use haystack_client::history::{HistoryStreamBatch, HistoryStreamEnd, HttpHistorySession};
use haystack_core::codecs::history::{
    HistoryReadValidator, HistoryTerminal, MAX_GRID_BYTES, MAX_ROWS,
};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::{sync::Semaphore, task::JoinHandle, time::Instant};
use tokio_util::sync::CancellationToken;

/// Concurrently running exports per process. Admission never queues: the
/// next `Export::start` beyond this returns `Error::Capacity`.
pub const MAX_CONCURRENT_EXPORTS: usize = 4;
/// `ExportOptions::default().timeout`, absolute from admission.
pub const DEFAULT_EXPORT_TIMEOUT: Duration = Duration::from_secs(60);
static EXPORTS: Semaphore = Semaphore::const_new(MAX_CONCURRENT_EXPORTS);
/// Export timeout starts at admission and includes idle I/O. Cancellation and
/// deadlines are cooperative: a started filesystem operation is always joined.
#[derive(Clone)]
pub struct ExportOptions {
    pub limits: Limits,
    pub timeout: Duration,
    pub cancellation: CancellationToken,
    #[cfg(test)]
    pub(crate) checkpoints: Checkpoints,
}
impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            limits: Limits::default(),
            timeout: DEFAULT_EXPORT_TIMEOUT,
            cancellation: CancellationToken::new(),
            #[cfg(test)]
            checkpoints: Checkpoints::default(),
        }
    }
}
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct ExportStats {
    pub source_pulls: u64,
    pub arrow_batches: u64,
    pub peak_source_batch_bytes: u64,
    pub peak_arrow_bytes: usize,
    /// Upstream column-writer estimate; not allocator/RSS accounting.
    pub peak_writer_bytes: usize,
}
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ExportReceipt {
    pub manifest: Manifest,
    pub stats: ExportStats,
}
/// Owns one export. `wait` and `close` are cancellation-safe; dropping their
/// futures leaves the worker handle here. Drop requests cancellation but does
/// not claim a join. Failed exports leave an inspectable incomplete directory.
pub struct Export {
    cancel: CancellationToken,
    task: Option<JoinHandle<Result<ExportReceipt>>>,
    result: Option<Result<ExportReceipt>>,
}
impl Export {
    /// Immediate bounded admission ([`MAX_CONCURRENT_EXPORTS`], no waiting
    /// queue). The destination must not exist. Called outside a Tokio
    /// runtime it returns `Error::Runtime` (and drops `source`).
    pub fn start(
        source: HttpHistorySession,
        directory: impl Into<PathBuf>,
        options: ExportOptions,
    ) -> Result<Self> {
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| Error::Runtime)?;
        options.limits.validate()?;
        let deadline = Instant::now()
            .checked_add(options.timeout)
            .ok_or(Error::Options)?;
        let permit = EXPORTS.try_acquire().map_err(|_| Error::Capacity)?;
        let cancel = options.cancellation.child_token();
        let control = Control {
            cancel: cancel.clone(),
            deadline,
            #[cfg(test)]
            checkpoints: options.checkpoints,
        };
        control.check()?;
        let dir = directory.into();
        let task = runtime.spawn(async move {
            let _permit = permit;
            run(source, dir, options.limits, control).await
        });
        Ok(Self {
            cancel,
            task: Some(task),
            result: None,
        })
    }
    pub fn cancel(&self) {
        self.cancel.cancel();
    }
    pub async fn wait(&mut self) -> Result<ExportReceipt> {
        if self.result.is_none() {
            let result = self
                .task
                .as_mut()
                .ok_or(Error::Worker)?
                .await
                .unwrap_or(Err(Error::Worker));
            self.task = None;
            self.result = Some(result);
        }
        self.result.clone().ok_or(Error::Worker)?
    }
    /// Request cancellation and join both the source and any started writer.
    /// A publication race may return success or PublishedUncertain.
    pub async fn close(&mut self) -> Result<ExportReceipt> {
        self.cancel();
        self.wait().await
    }
}
impl Drop for Export {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
#[derive(Clone)]
struct Control {
    cancel: CancellationToken,
    deadline: Instant,
    #[cfg(test)]
    checkpoints: Checkpoints,
}
impl Control {
    fn checkpoints(&self) -> Checkpoints {
        #[cfg(test)]
        {
            self.checkpoints.clone()
        }
        #[cfg(not(test))]
        {
            Checkpoints::default()
        }
    }
    fn check(&self) -> Result<()> {
        if self.cancel.is_cancelled() {
            Err(Error::Cancelled)
        } else if Instant::now() >= self.deadline {
            Err(Error::Deadline)
        } else {
            Ok(())
        }
    }
}
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    // Never race this join against cancellation/timeout: side effects continue.
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| Error::Worker)?
}
async fn pull(source: &mut HttpHistorySession, control: &Control) -> Result<HistoryStreamBatch> {
    use std::{future::Future, task::Poll};
    let mut next = std::pin::pin!(source.next());
    let mut observed = false;
    std::future::poll_fn(|cx| match next.as_mut().poll(cx) {
        Poll::Ready(batch) => Poll::Ready(Ok(batch)),
        Poll::Pending => {
            if !observed {
                observed = true;
                if let Err(error) = control.checkpoints().hit(Phase::WaitingPull) {
                    return Poll::Ready(Err(error));
                }
            }
            Poll::Pending
        }
    })
    .await
}
async fn run(
    mut source: HttpHistorySession,
    dir: PathBuf,
    limits: Limits,
    control: Control,
) -> Result<ExportReceipt> {
    let header = Arc::new(source.header().clone());
    let header_for_writer = header.clone();
    let checkpoints = control.checkpoints();
    let init = blocking(move || {
        Writer::new(
            dir,
            &header_for_writer,
            limits,
            uuid::Uuid::new_v4().to_string(),
            checkpoints,
        )
    })
    .await;
    let result = async {
        let mut writer = init?;
        let mut stats = ExportStats::default();
        let mut validator =
            HistoryReadValidator::new((*header).clone()).map_err(|_| Error::Invalid)?;
        loop {
            control.check()?;
            control.checkpoints().hit(Phase::Pull)?;
            control.check()?;
            stats.source_pulls += 1;
            let incoming = tokio::select! { biased;
                _ = control.cancel.cancelled() => return Err(Error::Cancelled),
                _ = tokio::time::sleep_until(control.deadline) => return Err(Error::Deadline),
                batch = pull(&mut source, &control) => batch?,
            };
            // Overlap check: the client source already caps a batch within the
            // bounded-history-v1 grid and row ceilings.
            let source_bytes: u64 = incoming
                .samples
                .iter()
                .map(HistoryReadValidator::sample_bytes)
                .sum();
            stats.peak_source_batch_bytes = stats.peak_source_batch_bytes.max(source_bytes);
            if source_bytes > MAX_GRID_BYTES as u64 || incoming.samples.len() > MAX_ROWS {
                return Err(Error::Limit);
            }
            let mut start = 0;
            while start < incoming.samples.len() {
                control.check()?;
                let mut end = start;
                let mut bytes = 0;
                while end < incoming.samples.len()
                    && end - start < writer.manifest.limits.batch_rows
                {
                    let size = HistoryReadValidator::sample_bytes(&incoming.samples[end]) as usize;
                    if bytes + size > writer.manifest.limits.batch_bytes {
                        break;
                    }
                    bytes += size;
                    end += 1;
                }
                if end == start {
                    return Err(Error::Limit);
                }
                for sample in &incoming.samples[start..end] {
                    validator.accept(sample).map_err(|_| Error::Invalid)?;
                }
                let batch = format::batch(
                    &header,
                    &incoming.samples[start..end],
                    &writer.manifest.limits,
                )?;
                stats.peak_arrow_bytes = stats.peak_arrow_bytes.max(batch.get_array_memory_size());
                stats.arrow_batches += 1;
                writer = blocking(move || {
                    writer.write(batch)?;
                    Ok(writer)
                })
                .await?;
                start = end;
            }
            // Samples on a terminal batch have already been processed.
            if let Some(end) = incoming.end {
                if end != HistoryStreamEnd::Remote(HistoryTerminal::Complete) {
                    return Err(Error::Source(end));
                }
                validator.finish().map_err(|_| Error::Invalid)?;
                break;
            }
        }
        let end = source.close().await;
        if end != HistoryStreamEnd::Remote(HistoryTerminal::Complete) {
            return Err(Error::Source(end));
        }
        control.checkpoints().hit(Phase::SourceJoined)?;
        control.check()?;
        writer = blocking(move || {
            writer.prepare()?;
            Ok(writer)
        })
        .await?;
        control.check()?;
        let publish_control = control.clone();
        writer = blocking(move || {
            publish_control.check()?;
            writer.publish(|| publish_control.check())?;
            Ok(writer)
        })
        .await?;
        stats.peak_writer_bytes = writer.peak_writer_bytes;
        Ok(ExportReceipt {
            manifest: writer.manifest,
            stats,
        })
    }
    .await;
    // On every precommit failure, wait for actual source destruction. All
    // blocking writer jobs above were joined before this path can be reached.
    source.close().await;
    // Cleanup observation cannot revise a published outcome.
    let _ = control.checkpoints().hit(Phase::Cleanup);
    result
}

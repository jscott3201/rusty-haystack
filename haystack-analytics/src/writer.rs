use crate::checkpoint::{Checkpoints, Phase};
use crate::{Error, Result, format::*};
use arrow_array::RecordBatch;
use haystack_core::codecs::history::HistoryReadHeader;
use parquet::{
    arrow::ArrowWriter,
    basic::{Compression, Encoding},
    file::properties::{EnabledStatistics, WriterProperties},
};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

/// Parquet data page size target. Pages are additionally bounded by rows.
const DATA_PAGE_BYTES: usize = 64 * 1024;
/// Rows the Parquet writer encodes per internal step (capped by `batch_rows`).
const WRITE_BATCH_ROWS: usize = 128;
/// Unix mode of the capture directory: owner only, like the CLI backup output.
#[cfg(unix)]
const DIRECTORY_MODE: u32 = 0o700;
/// Unix mode of every part and manifest file.
#[cfg(unix)]
const FILE_MODE: u32 = 0o600;

/// Make a directory's entries durable. On Unix this is `fsync` of the opened
/// directory. Elsewhere it is skipped: Windows cannot open a directory as a
/// file without `FILE_FLAG_BACKUP_SEMANTICS`, NTFS journals metadata, and its
/// durability semantics for links are not the Unix ones, so Windows
/// durability is not qualified (see `docs/parquet-history.md`).
pub(crate) fn sync_directory(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}
/// `create_new` write handle, owner-only on Unix.
fn create_private(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(FILE_MODE);
    }
    options.open(path)
}
/// `create_dir` (never an existing directory), owner-only on Unix.
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().mode(DIRECTORY_MODE).create(path)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir(path)
    }
}

pub(crate) struct CappedFile {
    file: File,
    bytes: u64,
    cap: u64,
}
impl Write for CappedFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.len() as u64 > self.cap.saturating_sub(self.bytes) {
            return Err(std::io::Error::other("part byte limit"));
        }
        let n = self.file.write(buf)?;
        self.bytes += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}
pub(crate) struct Writer {
    pub dir: PathBuf,
    pub manifest: Manifest,
    header: HistoryReadHeader,
    current: Option<ArrowWriter<CappedFile>>,
    current_rows: u64,
    current_groups: usize,
    total_bytes: u64,
    pub peak_writer_bytes: usize,
    checkpoints: Checkpoints,
    /// Tests forge hash-valid incompatible parts by skipping self-verification.
    #[cfg(test)]
    pub verify_parts: bool,
}
impl Writer {
    pub fn new(
        dir: PathBuf,
        header: &HistoryReadHeader,
        limits: Limits,
        id: String,
        checkpoints: Checkpoints,
    ) -> Result<Self> {
        create_private_dir(&dir)?;
        sync_directory(
            dir.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new(".")),
        )?;
        Ok(Self {
            dir,
            header: header.clone(),
            manifest: Manifest {
                manifest_version: 1,
                schema_version: 1,
                export_id: id,
                consistency: "completedGenerationCheckedLiveCapture".into(),
                source_header: encode_header(header)?,
                limits,
                rows: 0,
                parts: vec![],
            },
            current: None,
            current_rows: 0,
            current_groups: 0,
            total_bytes: 0,
            peak_writer_bytes: 0,
            checkpoints,
            #[cfg(test)]
            verify_parts: true,
        })
    }
    pub fn write(&mut self, batch: RecordBatch) -> Result<()> {
        // Conservative admission includes a full footer reserve and one batch
        // of encoding overhead. The counted sink remains the hard byte cap.
        let reserve = writer_reserve(
            batch.get_array_memory_size() as u64,
            self.manifest.limits.footer_bytes,
        );
        if self.current.as_ref().is_some_and(|w| {
            w.inner().bytes.saturating_add(reserve) > self.manifest.limits.part_bytes
        }) {
            self.finish_part()?;
        }
        let l = &self.manifest.limits;
        if reserve > l.part_bytes || reserve > l.total_bytes.saturating_sub(self.total_bytes) {
            return Err(Error::Limit);
        }
        if self.current.is_none() {
            if self.manifest.parts.len() >= l.parts {
                return Err(Error::Limit);
            }
            let file = create_private(&self.dir.join(part_name(self.manifest.parts.len())))?;
            let sink = CappedFile {
                file,
                bytes: 0,
                cap: l
                    .part_bytes
                    .min(l.total_bytes.saturating_sub(self.total_bytes)),
            };
            let props = WriterProperties::builder()
                .set_compression(Compression::UNCOMPRESSED)
                .set_encoding(Encoding::PLAIN)
                .set_dictionary_enabled(false)
                .set_statistics_enabled(EnabledStatistics::None)
                .set_bloom_filter_enabled(false)
                .set_offset_index_disabled(true)
                .set_data_page_size_limit(DATA_PAGE_BYTES)
                .set_data_page_row_count_limit(l.batch_rows)
                .set_write_batch_size(WRITE_BATCH_ROWS.min(l.batch_rows))
                .set_max_row_group_row_count(Some(l.batch_rows))
                .build();
            self.current = Some(ArrowWriter::try_new(sink, batch.schema(), Some(props))?);
        }
        let writer = self.current.as_mut().ok_or(Error::Worker)?;
        self.checkpoints.hit(Phase::Write)?;
        writer.write(&batch)?;
        self.peak_writer_bytes = self.peak_writer_bytes.max(writer.memory_size());
        writer.flush()?;
        self.current_rows += batch.num_rows() as u64;
        self.current_groups += 1;
        if self.current_groups == self.manifest.limits.groups_per_part {
            self.finish_part()?;
        }
        Ok(())
    }
    pub fn finish_part(&mut self) -> Result<()> {
        if let Some(mut writer) = self.current.take() {
            self.checkpoints.hit(Phase::Footer)?;
            writer.finish()?;
            self.checkpoints.hit(Phase::FileSync)?;
            writer.inner().file.sync_all()?;
            let bytes = writer.inner().bytes;
            drop(writer);
            let ordinal = self.manifest.parts.len();
            let name = part_name(ordinal);
            // Read the synced part back once and run the reader's own guards
            // over those exact bytes, so an export never publishes a part its
            // reader would reject. Footer overruns surface here as Limit.
            let data = crate::reader::read_regular(&self.dir.join(&name), bytes, true)?;
            crate::reader::footer_guard(&data, &self.manifest.limits)?;
            let part = Part {
                ordinal,
                name,
                bytes,
                sha256: sha256_hex(&data),
                rows: self.current_rows,
                groups: self.current_groups,
            };
            #[cfg(test)]
            let verify = self.verify_parts;
            #[cfg(not(test))]
            let verify = true;
            if verify {
                crate::reader::checked_metadata(
                    &data.into(),
                    &part,
                    &self.header,
                    &self.manifest.limits,
                )?;
            }
            self.manifest.parts.push(part);
            self.total_bytes += bytes;
            self.manifest.rows += self.current_rows;
            self.current_rows = 0;
            self.current_groups = 0;
        }
        Ok(())
    }
    pub fn prepare(&mut self) -> Result<()> {
        self.finish_part()?;
        self.checkpoints.hit(Phase::DirectorySync)?;
        sync_directory(&self.dir)?;
        let bytes = serde_json::to_vec(&self.manifest).map_err(|_| Error::Invalid)?;
        if bytes.len() > self.manifest.limits.manifest_bytes {
            return Err(Error::Limit);
        }
        let mut file = create_private(&self.dir.join(TEMP_MANIFEST))?;
        self.checkpoints.hit(Phase::ManifestWrite)?;
        file.write_all(&bytes)?;
        self.checkpoints.hit(Phase::ManifestSync)?;
        file.sync_all()?;
        Ok(())
    }
    pub fn publish(&self, check: impl FnOnce() -> Result<()>) -> Result<()> {
        self.checkpoints.hit(Phase::BeforePublish)?;
        check()?;
        fs::hard_link(self.dir.join(TEMP_MANIFEST), self.dir.join(MANIFEST_NAME))?;
        // Every failure after this link must retain the published dataset.
        let result = (|| -> Result<()> {
            self.checkpoints.hit(Phase::AfterPublish)?;
            self.checkpoints.hit(Phase::PublishedDirectorySync)?;
            sync_directory(&self.dir)?;
            fs::remove_file(self.dir.join(TEMP_MANIFEST))?;
            sync_directory(&self.dir)?;
            Ok(())
        })();
        result.map_err(|_| Error::PublishedUncertain {
            export_id: self.manifest.export_id.clone(),
        })
    }
}
pub(crate) fn part_name(n: usize) -> String {
    format!("part-{n:04}.parquet")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn counted_sink_caps_every_write_including_footer_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("part");
        let mut sink = CappedFile {
            file: File::create(&path).unwrap(),
            bytes: 0,
            cap: 12,
        };
        sink.write_all(b"PAR1data").unwrap();
        assert!(sink.write_all(b"footer-too-large").is_err());
        assert_eq!(sink.bytes, 8);
        sink.write_all(b"tail").unwrap();
        assert!(sink.write_all(b"x").is_err());
        assert_eq!(std::fs::metadata(path).unwrap().len(), 12);
    }
}

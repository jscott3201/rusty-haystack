//! HLSS v1 binary snapshot format — write/read EntityGraph state to disk.
//!
//! Format: [Header][Zstd-compressed Zinc body][CRC32 footer]
//! Header: "HLSS" (4 bytes) + format_version (u16 LE) + entity_count (u32 LE)
//!         + timestamp (i64 LE, Unix nanos) + graph_version (u64 LE) = 26 bytes
//! Body: Zstd-compressed Zinc-encoded grid of all entities
//! Footer: CRC32 (u32 LE) over header + compressed body

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::codecs::Codec;
use crate::codecs::zinc::ZincCodec;
use crate::data::{HCol, HDict, HGrid};
use crate::graph::shared::SharedGraph;

/// Metadata from a loaded snapshot.
#[derive(Debug, Clone)]
pub struct SnapshotMeta {
    pub format_version: u16,
    pub entity_count: u32,
    pub timestamp: i64,
    pub graph_version: u64,
    pub path: PathBuf,
}

/// Errors during snapshot operations.
#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid magic bytes")]
    InvalidMagic,
    #[error("unsupported format version: {0}")]
    UnsupportedVersion(u16),
    #[error("CRC32 mismatch: expected {expected:#010x}, got {actual:#010x}")]
    CrcMismatch { expected: u32, actual: u32 },
    #[error("decompression error: {0}")]
    Decompression(String),
    #[error("codec error: {0}")]
    Codec(String),
}

/// Writes snapshots of a [`SharedGraph`] to disk in HLSS format.
pub struct SnapshotWriter {
    dir: PathBuf,
    max_snapshots: usize,
    compression_level: i32,
}

impl SnapshotWriter {
    pub fn new(dir: PathBuf, max_snapshots: usize) -> Self {
        Self {
            dir,
            max_snapshots,
            compression_level: 3,
        }
    }

    pub fn with_compression(mut self, level: i32) -> Self {
        self.compression_level = level;
        self
    }

    /// Write a snapshot of the graph. Returns path to the snapshot file.
    /// Uses atomic write: write to temp file, then rename.
    pub fn write(&self, graph: &SharedGraph) -> Result<PathBuf, SnapshotError> {
        std::fs::create_dir_all(&self.dir)?;

        let (grid, version) = graph.read(|g| {
            let entities = g.all();
            let version = g.version();

            // Collect all unique tag names across entities for columns.
            let mut col_names = BTreeSet::new();
            for entity in &entities {
                for (key, _) in entity.iter() {
                    col_names.insert(key.to_owned());
                }
            }
            let cols: Vec<HCol> = col_names.iter().map(HCol::new).collect();
            let rows: Vec<HDict> = entities.into_iter().cloned().collect();
            let grid = HGrid::from_parts(HDict::new(), cols, rows);
            (grid, version)
        });

        // Encode to Zinc
        let zinc = ZincCodec;
        let zinc_str = zinc
            .encode_grid(&grid)
            .map_err(|e| SnapshotError::Codec(e.to_string()))?;

        // Compress
        let compressed = zstd::encode_all(zinc_str.as_bytes(), self.compression_level)
            .map_err(|e| SnapshotError::Decompression(e.to_string()))?;

        // Build header
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as i64;
        let entity_count = grid.rows.len() as u32;

        let mut buf = Vec::new();
        buf.extend_from_slice(b"HLSS");
        buf.extend_from_slice(&1u16.to_le_bytes()); // format version
        buf.extend_from_slice(&entity_count.to_le_bytes());
        buf.extend_from_slice(&timestamp.to_le_bytes());
        buf.extend_from_slice(&version.to_le_bytes());
        buf.extend_from_slice(&compressed);

        // CRC32 over header + body
        let crc = crc32fast::hash(&buf);
        buf.extend_from_slice(&crc.to_le_bytes());

        // Atomic write
        let filename = format!("snapshot-{version}.hlss");
        let final_path = self.dir.join(&filename);
        let tmp_path = self.dir.join(format!(".{filename}.tmp"));
        std::fs::write(&tmp_path, &buf)?;
        std::fs::rename(&tmp_path, &final_path)?;

        // Rotate old snapshots
        self.rotate()?;

        Ok(final_path)
    }

    /// Remove old snapshots beyond max_snapshots. Returns number removed.
    pub fn rotate(&self) -> Result<usize, SnapshotError> {
        let mut snapshots = Self::list_snapshots(&self.dir)?;
        if snapshots.len() <= self.max_snapshots {
            return Ok(0);
        }
        // Sort by filename (has version number) — oldest first
        snapshots.sort();
        let to_remove = snapshots.len() - self.max_snapshots;
        for path in &snapshots[..to_remove] {
            std::fs::remove_file(path)?;
        }
        Ok(to_remove)
    }

    fn list_snapshots(dir: &Path) -> Result<Vec<PathBuf>, SnapshotError> {
        if !dir.exists() {
            return Ok(Vec::new());
        }
        Ok(std::fs::read_dir(dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "hlss"))
            .filter(|e| !e.file_name().to_str().unwrap_or("").starts_with('.'))
            .map(|e| e.path())
            .collect())
    }
}

/// Reads and restores snapshots from disk in HLSS format.
pub struct SnapshotReader;

impl SnapshotReader {
    /// Find the latest snapshot in a directory.
    pub fn find_latest(dir: &Path) -> Result<Option<PathBuf>, SnapshotError> {
        let mut snapshots = SnapshotWriter::list_snapshots(dir)?;
        if snapshots.is_empty() {
            return Ok(None);
        }
        snapshots.sort();
        Ok(Some(snapshots.pop().unwrap()))
    }

    /// Load a snapshot file and import entities into the graph.
    pub fn load(path: &Path, graph: &SharedGraph) -> Result<SnapshotMeta, SnapshotError> {
        let data = std::fs::read(path)?;
        Self::load_from_bytes(&data, path, graph)
    }

    /// Load from raw bytes (useful for testing).
    pub fn load_from_bytes(
        data: &[u8],
        path: &Path,
        graph: &SharedGraph,
    ) -> Result<SnapshotMeta, SnapshotError> {
        // Need at least header (26) + CRC (4) = 30 bytes
        if data.len() < 30 {
            return Err(SnapshotError::InvalidMagic);
        }

        // Validate magic
        if &data[0..4] != b"HLSS" {
            return Err(SnapshotError::InvalidMagic);
        }

        // Parse header
        let format_version = u16::from_le_bytes([data[4], data[5]]);
        if format_version != 1 {
            return Err(SnapshotError::UnsupportedVersion(format_version));
        }
        let entity_count = u32::from_le_bytes(data[6..10].try_into().unwrap());
        let timestamp = i64::from_le_bytes(data[10..18].try_into().unwrap());
        let graph_version = u64::from_le_bytes(data[18..26].try_into().unwrap());

        // Validate CRC32
        let crc_offset = data.len() - 4;
        let expected_crc = u32::from_le_bytes(data[crc_offset..].try_into().unwrap());
        let actual_crc = crc32fast::hash(&data[..crc_offset]);
        if expected_crc != actual_crc {
            return Err(SnapshotError::CrcMismatch {
                expected: expected_crc,
                actual: actual_crc,
            });
        }

        // Decompress
        let compressed = &data[26..crc_offset];
        const MAX_DECOMPRESSED_SIZE: usize = 1024 * 1024 * 1024; // 1 GB
        let decompressed = zstd::decode_all(compressed)
            .map_err(|e| SnapshotError::Decompression(e.to_string()))?;
        if decompressed.len() > MAX_DECOMPRESSED_SIZE {
            return Err(SnapshotError::Decompression(format!(
                "decompressed data too large: {} bytes (max {})",
                decompressed.len(),
                MAX_DECOMPRESSED_SIZE
            )));
        }

        // Decode Zinc
        let zinc_str =
            std::str::from_utf8(&decompressed).map_err(|e| SnapshotError::Codec(e.to_string()))?;
        let zinc = ZincCodec;
        let grid = zinc
            .decode_grid(zinc_str)
            .map_err(|e| SnapshotError::Codec(e.to_string()))?;

        // Import entities into graph — bulk load skips changelog tracking.
        graph.write(|g| {
            for row in &grid.rows {
                if row.id().is_some() {
                    g.add_bulk(row.clone()).ok();
                }
            }
            g.finalize_bulk(graph_version);
        });

        Ok(SnapshotMeta {
            format_version,
            entity_count,
            timestamp,
            graph_version,
            path: path.to_path_buf(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::EntityGraph;
    use crate::kinds::{HRef, Kind};

    fn make_site(id: &str) -> HDict {
        let mut d = HDict::new();
        d.set("id", Kind::Ref(HRef::from_val(id)));
        d.set("site", Kind::Marker);
        d.set("dis", Kind::Str(format!("Site {id}")));
        d
    }

    fn populated_graph() -> SharedGraph {
        let sg = SharedGraph::new(EntityGraph::new());
        sg.add(make_site("site-1")).unwrap();
        sg.add(make_site("site-2")).unwrap();
        sg.add(make_site("site-3")).unwrap();
        sg
    }

    #[test]
    fn write_and_read_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let graph = populated_graph();

        let writer = SnapshotWriter::new(dir.path().to_path_buf(), 5);
        let snap_path = writer.write(&graph).unwrap();
        assert!(snap_path.exists());

        // Load into a fresh graph
        let graph2 = SharedGraph::new(EntityGraph::new());
        let meta = SnapshotReader::load(&snap_path, &graph2).unwrap();

        assert_eq!(meta.format_version, 1);
        assert_eq!(meta.entity_count, 3);
        assert_eq!(meta.graph_version, 3);
        assert_eq!(graph2.len(), 3);
        assert!(graph2.contains("site-1"));
        assert!(graph2.contains("site-2"));
        assert!(graph2.contains("site-3"));
    }

    #[test]
    fn find_latest_returns_most_recent() {
        let dir = tempfile::tempdir().unwrap();
        let graph = SharedGraph::new(EntityGraph::new());
        let writer = SnapshotWriter::new(dir.path().to_path_buf(), 5);

        graph.add(make_site("site-1")).unwrap();
        let _path1 = writer.write(&graph).unwrap();

        graph.add(make_site("site-2")).unwrap();
        let path2 = writer.write(&graph).unwrap();

        let latest = SnapshotReader::find_latest(dir.path()).unwrap().unwrap();
        assert_eq!(latest, path2);
    }

    #[test]
    fn rotate_removes_old_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let graph = SharedGraph::new(EntityGraph::new());
        let writer = SnapshotWriter::new(dir.path().to_path_buf(), 2);

        // Create 4 snapshots
        for i in 0..4 {
            graph.add(make_site(&format!("s-{i}"))).unwrap();
            writer.write(&graph).unwrap();
        }

        let remaining = SnapshotWriter::list_snapshots(dir.path()).unwrap();
        assert_eq!(remaining.len(), 2);
    }

    #[test]
    fn corrupt_crc_detected() {
        let dir = tempfile::tempdir().unwrap();
        let graph = populated_graph();

        let writer = SnapshotWriter::new(dir.path().to_path_buf(), 5);
        let snap_path = writer.write(&graph).unwrap();

        let mut data = std::fs::read(&snap_path).unwrap();
        // Corrupt one byte in the compressed body
        if data.len() > 30 {
            data[28] ^= 0xFF;
        }

        let graph2 = SharedGraph::new(EntityGraph::new());
        let result = SnapshotReader::load_from_bytes(&data, &snap_path, &graph2);
        assert!(matches!(result, Err(SnapshotError::CrcMismatch { .. })));
    }

    #[test]
    fn invalid_magic_rejected() {
        let data = b"NOPE_this_is_not_a_snapshot_at_all";
        let graph = SharedGraph::new(EntityGraph::new());
        let result = SnapshotReader::load_from_bytes(data, Path::new("bad.hlss"), &graph);
        assert!(matches!(result, Err(SnapshotError::InvalidMagic)));
    }

    #[test]
    fn empty_graph_produces_valid_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let graph = SharedGraph::new(EntityGraph::new());

        let writer = SnapshotWriter::new(dir.path().to_path_buf(), 5);
        let snap_path = writer.write(&graph).unwrap();

        let graph2 = SharedGraph::new(EntityGraph::new());
        let meta = SnapshotReader::load(&snap_path, &graph2).unwrap();
        assert_eq!(meta.entity_count, 0);
        assert_eq!(graph2.len(), 0);
    }

    #[test]
    fn find_latest_on_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let result = SnapshotReader::find_latest(dir.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn too_short_data_rejected() {
        let data = b"HLSS_short";
        let graph = SharedGraph::new(EntityGraph::new());
        let result = SnapshotReader::load_from_bytes(data, Path::new("x.hlss"), &graph);
        assert!(matches!(result, Err(SnapshotError::InvalidMagic)));
    }

    #[test]
    fn unsupported_version_rejected() {
        // Build a minimal valid-looking buffer with version 99
        let mut data = Vec::new();
        data.extend_from_slice(b"HLSS");
        data.extend_from_slice(&99u16.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&0i64.to_le_bytes());
        data.extend_from_slice(&0u64.to_le_bytes());
        // Need some body bytes so len >= 30
        data.extend_from_slice(&[0u8; 4]); // fake body
        let crc = crc32fast::hash(&data);
        data.extend_from_slice(&crc.to_le_bytes());

        let graph = SharedGraph::new(EntityGraph::new());
        let result = SnapshotReader::load_from_bytes(&data, Path::new("x.hlss"), &graph);
        assert!(matches!(result, Err(SnapshotError::UnsupportedVersion(99))));
    }
}

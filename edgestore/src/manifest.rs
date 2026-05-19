use crate::error::EdgestoreError;
use crate::types::{SegmentId, SegmentMeta};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const MANIFEST_MAGIC: u32 = 0x4D414E46; // "MANF"
pub const MANIFEST_VERSION: u8 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub enum ManifestEntryType {
    Add,
    Remove,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub entry_type: ManifestEntryType,
    pub segment_id: SegmentId,
    pub meta: Option<SegmentMeta>,
}

fn write_framed_entry(file: &mut File, entry: &ManifestEntry) -> Result<(), EdgestoreError> {
    let json = serde_json::to_vec(entry)
        .map_err(|e| EdgestoreError::ManifestCorrupt(e.to_string()))?;
    if json.len() > 1_000_000 {
        return Err(EdgestoreError::ManifestCorrupt("entry too large".to_string()));
    }
    let crc = crc32c::crc32c(&json);
    let len = json.len() as u32;
    file.write_all(&crc.to_le_bytes())?;
    file.write_all(&len.to_le_bytes())?;
    file.write_all(&json)?;
    Ok(())
}

pub struct Manifest {
    #[allow(dead_code)]
    path: PathBuf,
    file: File,
    segments: Vec<SegmentMeta>,
}

impl Manifest {
    pub fn open(path: &Path) -> Result<Manifest, EdgestoreError> {
        let file_exists = path.exists();
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)?;

        let mut segments: Vec<SegmentMeta> = Vec::new();

        if !file_exists {
            file.write_all(&MANIFEST_MAGIC.to_le_bytes())?;
            file.write_all(&[MANIFEST_VERSION, 0, 0, 0])?;
            file.sync_all()?;
        } else {
            let mut header = [0u8; 8];
            file.read_exact(&mut header)
                .map_err(|_| EdgestoreError::ManifestCorrupt("truncated header".to_string()))?;
            let magic = u32::from_le_bytes(header[0..4].try_into().unwrap());
            if magic != MANIFEST_MAGIC {
                return Err(EdgestoreError::ManifestCorrupt(format!(
                    "wrong magic: 0x{:08X}",
                    magic
                )));
            }
            if header[4] != MANIFEST_VERSION {
                return Err(EdgestoreError::ManifestCorrupt(format!(
                    "wrong version: {}",
                    header[4]
                )));
            }

            loop {
                let mut frame_hdr = [0u8; 8];
                match file.read_exact(&mut frame_hdr) {
                    Ok(_) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                    Err(e) => return Err(EdgestoreError::Io(e)),
                }
                let stored_crc = u32::from_le_bytes(frame_hdr[0..4].try_into().unwrap());
                let entry_len = u32::from_le_bytes(frame_hdr[4..8].try_into().unwrap()) as usize;

                if entry_len > 1_000_000 {
                    eprintln!("manifest: entry_len {} too large, stopping replay", entry_len);
                    break;
                }

                let mut json_bytes = vec![0u8; entry_len];
                match file.read_exact(&mut json_bytes) {
                    Ok(_) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                    Err(e) => return Err(EdgestoreError::Io(e)),
                }

                let computed_crc = crc32c::crc32c(&json_bytes);
                if computed_crc != stored_crc {
                    eprintln!("manifest: CRC32C mismatch (stored={}, computed={}), skipping entry", stored_crc, computed_crc);
                    continue;
                }

                let entry: ManifestEntry = match serde_json::from_slice(&json_bytes) {
                    Ok(e) => e,
                    Err(e) => {
                        eprintln!("manifest: JSON parse error: {}, skipping entry", e);
                        continue;
                    }
                };

                match entry.entry_type {
                    ManifestEntryType::Add => {
                        if let Some(meta) = entry.meta {
                            segments.push(meta);
                        }
                    }
                    ManifestEntryType::Remove => {
                        segments.retain(|m| m.segment_id != entry.segment_id);
                    }
                }
            }
        }

        file.seek(SeekFrom::End(0))?;

        Ok(Manifest { path: path.to_path_buf(), file, segments })
    }

    pub fn add_segment(&mut self, meta: SegmentMeta) -> Result<(), EdgestoreError> {
        let entry = ManifestEntry {
            entry_type: ManifestEntryType::Add,
            segment_id: meta.segment_id,
            meta: Some(meta.clone()),
        };
        write_framed_entry(&mut self.file, &entry)?;
        self.file.sync_all()?;
        self.segments.push(meta);
        Ok(())
    }

    pub fn remove_segments(&mut self, ids: &[SegmentId]) -> Result<(), EdgestoreError> {
        for &id in ids {
            let entry = ManifestEntry {
                entry_type: ManifestEntryType::Remove,
                segment_id: id,
                meta: None,
            };
            write_framed_entry(&mut self.file, &entry)?;
        }
        self.file.sync_all()?;
        self.segments.retain(|m| !ids.contains(&m.segment_id));
        Ok(())
    }

    pub fn list_segments(&self) -> &[SegmentMeta] {
        &self.segments
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SegmentMeta;
    use tempfile::TempDir;

    fn make_meta(segment_id: u64) -> SegmentMeta {
        SegmentMeta {
            segment_id,
            segment_hash: vec![0u8; 32],
            min_key: b"aaa".to_vec(),
            max_key: b"zzz".to_vec(),
            min_lsn: 1,
            max_lsn: 100,
            record_count: 50,
            compressed_bytes: 1024,
            uncompressed_bytes: 4096,
            compression: "zstd:1".to_string(),
            cohort_bucket: 1,
            death_time: 9_999_999_999_999,
            merkle_root: vec![0u8; 32],
            created_at: 1_000_000_000_000,
        }
    }

    #[test]
    fn test_magic_constant() {
        let bytes = MANIFEST_MAGIC.to_le_bytes();
        assert_eq!(bytes, [0x46, 0x4E, 0x41, 0x4D]); // "MANF" little-endian
    }

    #[test]
    fn test_open_new_path_empty_segments() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("manifest.mf");
        let m = Manifest::open(&path).unwrap();
        assert!(m.list_segments().is_empty());
    }

    #[test]
    fn test_add_segment_then_replay() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("manifest.mf");

        {
            let mut m = Manifest::open(&path).unwrap();
            m.add_segment(make_meta(0)).unwrap();
            m.add_segment(make_meta(1)).unwrap();
        }

        let m2 = Manifest::open(&path).unwrap();
        assert_eq!(m2.list_segments().len(), 2);
        assert_eq!(m2.list_segments()[0].segment_id, 0);
        assert_eq!(m2.list_segments()[1].segment_id, 1);
    }

    #[test]
    fn test_remove_segment_then_replay() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("manifest.mf");

        {
            let mut m = Manifest::open(&path).unwrap();
            m.add_segment(make_meta(0)).unwrap();
            m.remove_segments(&[0]).unwrap();
        }

        let m2 = Manifest::open(&path).unwrap();
        assert!(m2.list_segments().is_empty());
    }

    #[test]
    fn test_corrupt_entry_skipped_prior_entries_recovered() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("manifest.mf");

        {
            let mut m = Manifest::open(&path).unwrap();
            m.add_segment(make_meta(0)).unwrap();
            m.add_segment(make_meta(1)).unwrap();
        }

        // Corrupt the last 4 bytes of the manifest file
        let file_len = std::fs::metadata(&path).unwrap().len();
        let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        use std::io::Seek;
        f.seek(SeekFrom::End(-4)).unwrap();
        f.write_all(&[0xFF, 0xFF, 0xFF, 0xFF]).unwrap();

        let m2 = Manifest::open(&path).unwrap();
        // First entry should still be recovered; second entry corrupt-skipped
        // (at minimum segment 0 should be present)
        let _ = file_len; // used above
        assert!(!m2.list_segments().is_empty());
        assert_eq!(m2.list_segments()[0].segment_id, 0);
    }

    #[test]
    fn test_entry_serialization_frame_format() {
        let dir = TempDir::new().unwrap();
        let mut f = std::fs::OpenOptions::new()
            .create(true).read(true).write(true).truncate(false)
            .open(dir.path().join("test.bin")).unwrap();

        let entry = ManifestEntry {
            entry_type: ManifestEntryType::Add,
            segment_id: 42,
            meta: Some(make_meta(42)),
        };
        write_framed_entry(&mut f, &entry).unwrap();

        f.seek(SeekFrom::Start(0)).unwrap();
        let mut crc_bytes = [0u8; 4];
        let mut len_bytes = [0u8; 4];
        f.read_exact(&mut crc_bytes).unwrap();
        f.read_exact(&mut len_bytes).unwrap();
        let len = u32::from_le_bytes(len_bytes) as usize;
        let mut json_bytes = vec![0u8; len];
        f.read_exact(&mut json_bytes).unwrap();

        let computed_crc = crc32c::crc32c(&json_bytes);
        let stored_crc = u32::from_le_bytes(crc_bytes);
        assert_eq!(computed_crc, stored_crc);

        let parsed: ManifestEntry = serde_json::from_slice(&json_bytes).unwrap();
        assert_eq!(parsed.segment_id, 42);
    }
}

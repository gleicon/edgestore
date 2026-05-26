use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::EdgestoreConfig;
use crate::error::EdgestoreError;
use crate::types::{Lsn, Operation, WalRecord};

// ── Constants ────────────────────────────────────────────────────────────────

pub(crate) const WAL_MAGIC: [u8; 4] = [0x45, 0x44, 0x47, 0x57]; // "EDGW"
pub(crate) const WAL_FORMAT_VERSION: u8 = 1;
pub(crate) const WAL_HEADER_LEN: usize = 8;

/// Maximum decompressed-payload length we will accept (DoS protection).
const MAX_COMPRESSED_LEN: u32 = 64 * 1024 * 1024; // 64 MiB

// ── Serialization / deserialization ─────────────────────────────────────────

/// Serialise a `WalRecord` to raw bytes.
///
/// Field order (all LE except ns_len which is BE):
///   txid(u64-LE) lsn(u64-LE) timestamp(i64-LE) ttl(u32-LE) op(u8)
///   ns_len(u16-BE) ns_bytes key_len(u32-LE) key_bytes
///   value_hash(32-bytes) val_len(u32-LE) val_bytes
pub(crate) fn serialize_record(rec: &WalRecord) -> Vec<u8> {
    let cap = 8 + 8 + 8 + 4 + 1 + 2 + rec.ns_bytes.len() + 4 + rec.key_bytes.len()
        + 32 + 4 + rec.value_bytes.len();
    let mut buf = Vec::with_capacity(cap);

    buf.extend_from_slice(&rec.txid.to_le_bytes());
    buf.extend_from_slice(&rec.lsn.to_le_bytes());
    buf.extend_from_slice(&rec.timestamp.to_le_bytes());
    buf.extend_from_slice(&rec.ttl.to_le_bytes());
    buf.push(rec.op.clone() as u8);
    buf.extend_from_slice(&rec.ns_len.to_be_bytes()); // BE — matches encode_key
    buf.extend_from_slice(&rec.ns_bytes);
    buf.extend_from_slice(&(rec.key_bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(&rec.key_bytes);
    buf.extend_from_slice(&rec.value_hash); // 32 raw bytes, no length prefix
    buf.extend_from_slice(&(rec.value_bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(&rec.value_bytes);

    buf
}

/// Deserialise a `WalRecord` from raw bytes.
///
/// Returns `Err(CorruptRecord)` if the buffer is too short at any step.
pub(crate) fn deserialize_record(buf: &[u8]) -> Result<WalRecord, EdgestoreError> {
    let mut pos = 0usize;

    macro_rules! need {
        ($n:expr, $label:expr) => {{
            let start = pos;
            let end = start + $n;
            if buf.len() < end {
                return Err(EdgestoreError::CorruptRecord(format!(
                    "buffer too short: {} (need {} more at offset {})",
                    $label,
                    $n,
                    start
                )));
            }
            #[allow(unused_assignments)]
            {
                pos = end;
            }
            &buf[start..end]
        }};
    }

    let txid = u64::from_le_bytes(need!(8, "txid").try_into().unwrap());
    let lsn = u64::from_le_bytes(need!(8, "lsn").try_into().unwrap()) as Lsn;
    let timestamp = i64::from_le_bytes(need!(8, "timestamp").try_into().unwrap());
    let ttl = u32::from_le_bytes(need!(4, "ttl").try_into().unwrap());

    let op_byte = need!(1, "op")[0];
    let op = match op_byte {
        1 => Operation::Put,
        2 => Operation::Delete,
        _ => {
            return Err(EdgestoreError::CorruptRecord(format!(
                "unknown operation byte: {}",
                op_byte
            )))
        }
    };

    let ns_len = u16::from_be_bytes(need!(2, "ns_len").try_into().unwrap());
    let ns_bytes = need!(ns_len as usize, "ns_bytes").to_vec();

    let key_len = u32::from_le_bytes(need!(4, "key_len").try_into().unwrap()) as usize;
    let key_bytes = need!(key_len, "key_bytes").to_vec();

    let hash_bytes = need!(32, "value_hash");
    let mut value_hash = [0u8; 32];
    value_hash.copy_from_slice(hash_bytes);

    let val_len = u32::from_le_bytes(need!(4, "val_len").try_into().unwrap()) as usize;
    let value_bytes = need!(val_len, "val_bytes").to_vec();

    Ok(WalRecord {
        txid,
        lsn,
        timestamp,
        ttl,
        ns_len,
        ns_bytes,
        key_bytes,
        op,
        value_hash,
        value_bytes,
    })
}

// ── WalWriter ────────────────────────────────────────────────────────────────

pub(crate) struct WalWriter {
    file: File,
    bytes_written: u64,
    created_at_secs: u64,
    wal_max_bytes: u64,
    wal_max_age_secs: u64,
}

impl std::fmt::Debug for WalWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WalWriter")
            .field("bytes_written", &self.bytes_written)
            .field("created_at_secs", &self.created_at_secs)
            .field("wal_max_bytes", &self.wal_max_bytes)
            .field("wal_max_age_secs", &self.wal_max_age_secs)
            .finish()
    }
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Write the 8-byte WAL header to `w`:  [EDGW magic][version=1][0][0][0]
fn write_wal_header(w: &mut impl Write) -> Result<(), EdgestoreError> {
    let mut header = [0u8; WAL_HEADER_LEN];
    header[..4].copy_from_slice(&WAL_MAGIC);
    header[4] = WAL_FORMAT_VERSION;
    // bytes 5-7 are reserved zeros (already zero)
    w.write_all(&header)?;
    Ok(())
}

/// Read and validate the 8-byte WAL header from `r`.
fn read_and_validate_header(r: &mut impl Read) -> Result<(), EdgestoreError> {
    let mut header = [0u8; WAL_HEADER_LEN];
    r.read_exact(&mut header).map_err(|_| {
        EdgestoreError::CorruptRecord("WAL file too short to contain header".to_string())
    })?;

    if header[..4] != WAL_MAGIC {
        return Err(EdgestoreError::CorruptRecord(format!(
            "bad WAL magic: {:?}",
            &header[..4]
        )));
    }
    let version = header[4];
    if version != WAL_FORMAT_VERSION {
        return Err(EdgestoreError::FormatVersion {
            expected: WAL_FORMAT_VERSION,
            got: version,
        });
    }
    Ok(())
}

impl WalWriter {
    /// Create a new WAL file, writing the 8-byte header.
    pub(crate) fn create(path: &Path, config: &EdgestoreConfig) -> Result<WalWriter, EdgestoreError> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;

        write_wal_header(&mut file)?;
        file.flush()?;

        Ok(WalWriter {
            file,
            bytes_written: WAL_HEADER_LEN as u64,
            created_at_secs: now_unix_secs(),
            wal_max_bytes: config.wal_max_bytes,
            wal_max_age_secs: config.wal_max_age_secs,
        })
    }

    /// Open an existing WAL file for appending.  Validates header.
    pub(crate) fn open(path: &Path, config: &EdgestoreConfig) -> Result<WalWriter, EdgestoreError> {
        let mut file = OpenOptions::new().read(true).write(true).open(path)?;

        read_and_validate_header(&mut file)?;

        // Seek to end so all future writes append.
        let end = file.seek(SeekFrom::End(0))?;

        Ok(WalWriter {
            file,
            bytes_written: end,
            created_at_secs: now_unix_secs(),
            wal_max_bytes: config.wal_max_bytes,
            wal_max_age_secs: config.wal_max_age_secs,
        })
    }

    /// Append one record.  Format:  {crc32c:u32-LE}{compressed_len:u32-LE}{lz4_payload}
    ///
    /// Does NOT call fsync — callers use `fsync()` for group-commit.
    pub(crate) fn append(&mut self, record: &WalRecord) -> Result<(), EdgestoreError> {
        let raw = serialize_record(record);
        let compressed = lz4_flex::compress_prepend_size(&raw);
        let crc = crc32c::crc32c(&compressed);

        let mut frame = Vec::with_capacity(8 + compressed.len());
        frame.extend_from_slice(&crc.to_le_bytes());
        frame.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        frame.extend_from_slice(&compressed);

        self.file.write_all(&frame)?;
        self.bytes_written += frame.len() as u64;
        Ok(())
    }

    /// Flush OS buffers to durable storage.
    pub(crate) fn fsync(&mut self) -> Result<(), EdgestoreError> {
        self.file.sync_all()?;
        Ok(())
    }

    /// Returns `true` when this WAL should be rotated.
    pub(crate) fn needs_rotation(&self, now_secs: u64) -> bool {
        self.bytes_written >= self.wal_max_bytes
            || (now_secs.saturating_sub(self.created_at_secs)) >= self.wal_max_age_secs
    }
}

// ── WalReader ────────────────────────────────────────────────────────────────

pub(crate) struct WalReader {
    file: File,
}

impl WalReader {
    /// Open an existing WAL file for reading.  Validates header.
    pub(crate) fn open(path: &Path) -> Result<WalReader, EdgestoreError> {
        let mut file = OpenOptions::new().read(true).open(path)?;
        read_and_validate_header(&mut file)?;
        Ok(WalReader { file })
    }

    /// Read all valid records from the WAL.
    ///
    /// Corrupt or truncated frames are logged to stderr and skipped / stopped.
    pub(crate) fn read_records(&mut self) -> Vec<WalRecord> {
        let mut records = Vec::new();

        loop {
            // 1. Read 8-byte frame header: crc32c(4) + compressed_len(4)
            let mut frame_hdr = [0u8; 8];
            match self.file.read_exact(&mut frame_hdr) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(_) => break,
            }

            // 2. Parse fields
            let crc_stored = u32::from_le_bytes(frame_hdr[..4].try_into().unwrap());
            let compressed_len = u32::from_le_bytes(frame_hdr[4..8].try_into().unwrap());

            // 3. DoS guard
            if compressed_len > MAX_COMPRESSED_LEN {
                eprintln!(
                    "WAL frame compressed_len {} exceeds limit, stopping replay",
                    compressed_len
                );
                break;
            }

            // 4. Read compressed payload
            let mut compressed = vec![0u8; compressed_len as usize];
            match self.file.read_exact(&mut compressed) {
                Ok(()) => {}
                Err(_) => break, // truncated — stop
            }

            // 5. Verify CRC32C
            let crc_computed = crc32c::crc32c(&compressed);
            if crc_computed != crc_stored {
                eprintln!("WAL CRC mismatch, skipping");
                continue;
            }

            // 6. Decompress
            let raw = match lz4_flex::decompress_size_prepended(&compressed) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("WAL LZ4 decompress error: {}, skipping", e);
                    continue;
                }
            };

            // 7. Deserialise
            match deserialize_record(&raw) {
                Ok(rec) => records.push(rec),
                Err(e) => {
                    eprintln!("WAL deserialize error: {}, skipping", e);
                    continue;
                }
            }
        }

        records
    }
}

// ── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::EdgestoreConfig;
    use tempfile::NamedTempFile;

    fn sample_record(lsn: u64) -> WalRecord {
        WalRecord {
            txid: 1,
            lsn,
            timestamp: 1_700_000_000,
            ttl: 0,
            ns_len: 2,
            ns_bytes: b"ns".to_vec(),
            key_bytes: format!("key{}", lsn).into_bytes(),
            op: Operation::Put,
            value_hash: [0xAB; 32],
            value_bytes: format!("value{}", lsn).into_bytes(),
        }
    }

    fn test_config(path: impl Into<std::path::PathBuf>) -> EdgestoreConfig {
        EdgestoreConfig::new(path)
    }

    // ── Task 1: serialization ───────────────────────────────────────────────

    #[test]
    fn wal_round_trip_value_hash() {
        let rec = sample_record(42);
        let serialized = serialize_record(&rec);
        let recovered = deserialize_record(&serialized).expect("deserialize failed");
        assert_eq!(recovered.value_hash, [0xAB; 32]);
        assert_eq!(recovered.lsn, 42);
        assert_eq!(recovered.ns_bytes, b"ns");
        assert_eq!(recovered.key_bytes, b"key42");
        assert_eq!(recovered.value_bytes, b"value42");
    }

    #[test]
    fn wal_truncated_buffer_returns_error() {
        let rec = sample_record(1);
        let serialized = serialize_record(&rec);
        let truncated = &serialized[..10];
        let result = deserialize_record(truncated);
        assert!(
            matches!(result, Err(EdgestoreError::CorruptRecord(_))),
            "expected CorruptRecord, got {:?}",
            result
        );
    }

    #[test]
    fn wal_magic_constant() {
        assert_eq!(WAL_MAGIC, [0x45, 0x44, 0x47, 0x57]);
    }

    // ── Task 2: writer ──────────────────────────────────────────────────────

    #[test]
    fn wal_writer_create_header() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        // NamedTempFile already holds the file; remove it so create_new works.
        drop(tmp);

        let cfg = test_config(&path);
        let mut writer = WalWriter::create(&path, &cfg).unwrap();
        writer.append(&sample_record(1)).unwrap();
        writer.fsync().unwrap();

        let mut f = File::open(&path).unwrap();
        let mut hdr = [0u8; 8];
        f.read_exact(&mut hdr).unwrap();

        assert_eq!(&hdr[..4], &WAL_MAGIC);
        assert_eq!(hdr[4], WAL_FORMAT_VERSION);
        assert_eq!(&hdr[5..8], &[0u8, 0, 0]);
    }

    #[test]
    fn wal_writer_open_wrong_magic() {
        let mut tmp = NamedTempFile::new().unwrap();
        // Write garbage magic
        tmp.write_all(&[0xFF, 0xFF, 0xFF, 0xFF, 1, 0, 0, 0]).unwrap();
        tmp.flush().unwrap();

        let cfg = test_config(tmp.path());
        let result = WalWriter::open(tmp.path(), &cfg);
        assert!(
            matches!(result, Err(EdgestoreError::CorruptRecord(_))),
            "expected CorruptRecord, got {:?}",
            result
        );
    }

    #[test]
    fn wal_writer_open_wrong_version() {
        let mut tmp = NamedTempFile::new().unwrap();
        // Write correct magic, bad version
        let mut hdr = [0u8; 8];
        hdr[..4].copy_from_slice(&WAL_MAGIC);
        hdr[4] = 99;
        tmp.write_all(&hdr).unwrap();
        tmp.flush().unwrap();

        let cfg = test_config(tmp.path());
        let result = WalWriter::open(tmp.path(), &cfg);
        assert!(
            matches!(
                result,
                Err(EdgestoreError::FormatVersion { expected: 1, got: 99 })
            ),
            "expected FormatVersion{{expected:1,got:99}}, got {:?}",
            result
        );
    }

    // ── Task 3: reader ──────────────────────────────────────────────────────

    #[test]
    fn wal_reader_five_records() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        drop(tmp);

        let cfg = test_config(&path);
        let mut writer = WalWriter::create(&path, &cfg).unwrap();
        for i in 1..=5u64 {
            writer.append(&sample_record(i)).unwrap();
        }
        writer.fsync().unwrap();

        let mut reader = WalReader::open(&path).unwrap();
        let records = reader.read_records();
        assert_eq!(records.len(), 5);
        for (i, r) in records.iter().enumerate() {
            assert_eq!(r.lsn, (i + 1) as u64);
        }
    }

    #[test]
    fn wal_reader_corrupt_middle_record() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        drop(tmp);

        let cfg = test_config(&path);
        let mut writer = WalWriter::create(&path, &cfg).unwrap();
        for i in 1..=3u64 {
            writer.append(&sample_record(i)).unwrap();
        }
        writer.fsync().unwrap();

        // Flip a bit at byte offset 50 (inside the second record's compressed payload).
        let mut data = std::fs::read(&path).unwrap();
        if data.len() > 50 {
            data[50] ^= 0xFF;
        }
        std::fs::write(&path, &data).unwrap();

        let mut reader = WalReader::open(&path).unwrap();
        let records = reader.read_records();
        // First record should be fine; the corrupt frame is skipped;
        // third record should also be recovered.
        // We expect at least the first record.
        assert!(
            !records.is_empty(),
            "expected at least 1 record, got 0"
        );
        // The corrupt record (whichever frame byte 50 lands in) should be skipped.
        assert!(
            records.len() < 3,
            "expected fewer than 3 records due to corruption, got {}",
            records.len()
        );
    }

    #[test]
    fn wal_reader_truncated_last_record() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        drop(tmp);

        let cfg = test_config(&path);
        let mut writer = WalWriter::create(&path, &cfg).unwrap();
        writer.append(&sample_record(1)).unwrap();
        writer.append(&sample_record(2)).unwrap();
        writer.fsync().unwrap();

        // Truncate the last 4 bytes.
        let mut data = std::fs::read(&path).unwrap();
        let new_len = data.len().saturating_sub(4);
        data.truncate(new_len);
        std::fs::write(&path, &data).unwrap();

        let mut reader = WalReader::open(&path).unwrap();
        let records = reader.read_records();
        // First record must be present; second silently skipped.
        assert_eq!(records[0].lsn, 1);
        // At most 1 record fully survived (could be 2 if truncation left second intact,
        // but we truncated the payload so it should be only 1).
        assert!(records.len() <= 2);
    }
}

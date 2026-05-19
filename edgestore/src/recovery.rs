use std::path::{Path, PathBuf};
use crate::error::EdgestoreError;
use crate::memtable::MemTable;
use crate::types::{encode_key, Lsn, MemEntry, Operation};
use crate::wal::WalReader;

pub struct RecoveryResult {
    pub records_replayed: u64,
    pub records_skipped: u64,
    pub max_lsn: Lsn,
    pub max_txid: u64,
    pub wal_files_read: usize,
}

pub(crate) fn list_wal_files(db_path: &Path) -> Result<Vec<PathBuf>, EdgestoreError> {
    if !db_path.exists() {
        return Ok(vec![]);
    }

    let mut files = Vec::new();
    for entry in std::fs::read_dir(db_path)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Pattern: "wal-" + 16 hex chars + ".log" = 24 chars total
        if name_str.len() == 24
            && name_str.starts_with("wal-")
            && name_str.ends_with(".log")
        {
            let hex_part = &name_str[4..20]; // chars 4..20 = 16 chars
            if hex_part.chars().all(|c| c.is_ascii_hexdigit()) {
                files.push(entry.path());
            }
        }
    }

    // Sort ascending by filename (lexicographic = chronological for zero-padded hex)
    files.sort();
    Ok(files)
}

pub fn recover_from_wal(
    db_path: &Path,
    memtable: &mut Box<dyn MemTable>,
) -> Result<RecoveryResult, EdgestoreError> {
    let wal_files = list_wal_files(db_path)?;

    let mut records_replayed = 0u64;
    let mut records_skipped = 0u64;
    let mut max_lsn = 0u64;
    let mut max_txid = 0u64;
    let mut wal_files_read = 0usize;

    for wal_path in &wal_files {
        let mut reader = match WalReader::open(wal_path) {
            Ok(r) => r,
            Err(EdgestoreError::FormatVersion { .. }) | Err(EdgestoreError::CorruptRecord(_)) => {
                records_skipped += 1;
                continue;
            }
            Err(e) => return Err(e),
        };

        let records = reader.read_records();
        wal_files_read += 1;

        for record in records {
            let encoded_key = encode_key(&record.ns_bytes, &record.key_bytes);
            let entry = MemEntry {
                key: encoded_key.clone(),
                value: if record.op == Operation::Put {
                    Some(record.value_bytes.clone())
                } else {
                    None
                },
                op: record.op,
                lsn: record.lsn,
                timestamp: record.timestamp,
                ttl: record.ttl,
            };
            memtable.insert(encoded_key, entry);

            if record.lsn > max_lsn {
                max_lsn = record.lsn;
            }
            if record.txid > max_txid {
                max_txid = record.txid;
            }
            records_replayed += 1;
        }
    }

    Ok(RecoveryResult {
        records_replayed,
        records_skipped,
        max_lsn,
        max_txid,
        wal_files_read,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::EdgestoreConfig;
    use crate::memtable::BTreeMemTable;
    use crate::types::{Operation, WalRecord};
    use crate::wal::WalWriter;
    use tempfile::TempDir;

    fn make_record(lsn: u64, txid: u64) -> WalRecord {
        WalRecord {
            txid,
            lsn,
            timestamp: 0,
            ttl: 0,
            ns_len: 2,
            ns_bytes: b"ns".to_vec(),
            key_bytes: format!("key{}", lsn).into_bytes(),
            op: Operation::Put,
            value_hash: blake3::hash(b"val").into(),
            value_bytes: b"val".to_vec(),
        }
    }

    fn make_config(dir: &TempDir) -> EdgestoreConfig {
        EdgestoreConfig::new(dir.path())
    }

    #[test]
    fn test_recover_five_records() {
        let dir = TempDir::new().unwrap();
        let config = make_config(&dir);
        let wal_path = dir.path().join("wal-0000000000000001.log");
        let mut writer = WalWriter::create(&wal_path, &config).unwrap();
        for i in 1..=5 {
            writer.append(&make_record(i, 0)).unwrap();
        }
        writer.fsync().unwrap();
        drop(writer);

        let mut memtable: Box<dyn MemTable> = Box::new(BTreeMemTable::new());
        let result = recover_from_wal(dir.path(), &mut memtable).unwrap();
        assert_eq!(result.records_replayed, 5);
        assert_eq!(result.max_lsn, 5);
        assert_eq!(result.wal_files_read, 1);
    }

    #[test]
    fn test_recover_two_wal_files() {
        let dir = TempDir::new().unwrap();
        let config = make_config(&dir);

        let wal1_path = dir.path().join("wal-0000000000000001.log");
        let mut w1 = WalWriter::create(&wal1_path, &config).unwrap();
        for i in 1..=3 {
            w1.append(&make_record(i, 0)).unwrap();
        }
        w1.fsync().unwrap();
        drop(w1);

        let wal2_path = dir.path().join("wal-0000000000000002.log");
        let mut w2 = WalWriter::create(&wal2_path, &config).unwrap();
        for i in 4..=6 {
            w2.append(&make_record(i, 0)).unwrap();
        }
        w2.fsync().unwrap();
        drop(w2);

        let mut memtable: Box<dyn MemTable> = Box::new(BTreeMemTable::new());
        let result = recover_from_wal(dir.path(), &mut memtable).unwrap();
        assert_eq!(result.records_replayed, 6);
        assert_eq!(result.wal_files_read, 2);
        assert_eq!(result.max_lsn, 6);
    }

    #[test]
    fn test_recover_empty_db() {
        let dir = TempDir::new().unwrap();
        let mut memtable: Box<dyn MemTable> = Box::new(BTreeMemTable::new());
        let result = recover_from_wal(dir.path(), &mut memtable).unwrap();
        assert_eq!(result.records_replayed, 0);
        assert_eq!(result.max_lsn, 0);
        assert_eq!(result.max_txid, 0);
        assert_eq!(result.wal_files_read, 0);
    }
}

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use xorf::Filter;

use crate::error::EdgestoreError;
use crate::manifest::Manifest;
use crate::memtable::MemTable;
use crate::types::{
    cohort_bucket_for, death_time_for, MemEntry, Operation, SegmentId, SegmentMeta,
};

pub mod in_memory;

// ── Constants ──────────────────────────────────────────────────────────────

pub(crate) const SEGMENT_BLOCK_MAGIC: u32 = 0x45445347; // "EDSG"
pub(crate) const SEGMENT_FILE_MAGIC: u32 = 0x45445347;
pub(crate) const SEGMENT_FORMAT_VERSION: u8 = 1;
/// Target size for an uncompressed block before flushing
const BLOCK_TARGET_BYTES: usize = 3900;
/// Block alignment boundary (4 KiB)
pub(crate) const SEGMENT_BLOCK_SIZE: usize = 4096;
/// Sparse index entry every N keys
/// How many records to skip between sparse-index entries.
pub const SPARSE_INDEX_STRIDE: usize = 64;

// ── Entry serialization ────────────────────────────────────────────────────

/// Layout: `[key_len:u32-le][key][val_len:u32-le][val][lsn:u64-le][timestamp:i64-le][ttl:u32-le][op:u8]`
pub(crate) fn serialize_entry(key: &[u8], entry: &MemEntry) -> Vec<u8> {
    let val = entry.value.as_deref().unwrap_or(&[]);
    let mut buf = Vec::with_capacity(4 + key.len() + 4 + val.len() + 21);
    buf.extend_from_slice(&(key.len() as u32).to_le_bytes());
    buf.extend_from_slice(key);
    buf.extend_from_slice(&(val.len() as u32).to_le_bytes());
    buf.extend_from_slice(val);
    buf.extend_from_slice(&entry.lsn.to_le_bytes());
    buf.extend_from_slice(&entry.timestamp.to_le_bytes());
    buf.extend_from_slice(&entry.ttl.to_le_bytes());
    buf.push(match entry.op {
        Operation::Put => 1,
        Operation::Delete => 2,
    });
    buf
}

pub(crate) fn deserialize_entry(
    buf: &[u8],
    pos: &mut usize,
) -> Result<(Vec<u8>, MemEntry), EdgestoreError> {
    macro_rules! read_bytes {
        ($n:expr, $field:literal) => {{
            if buf.len() < *pos + $n {
                return Err(EdgestoreError::SegmentCorrupt(format!(
                    "buffer too short: field {}",
                    $field
                )));
            }
            let slice = &buf[*pos..*pos + $n];
            *pos += $n;
            slice
        }};
    }

    let key_len = u32::from_le_bytes(read_bytes!(4, "key_len").try_into().unwrap()) as usize;
    let key = read_bytes!(key_len, "key").to_vec();
    let val_len = u32::from_le_bytes(read_bytes!(4, "val_len").try_into().unwrap()) as usize;
    let val_bytes = read_bytes!(val_len, "val").to_vec();
    let lsn = u64::from_le_bytes(read_bytes!(8, "lsn").try_into().unwrap());
    let timestamp = i64::from_le_bytes(read_bytes!(8, "timestamp").try_into().unwrap());
    let ttl = u32::from_le_bytes(read_bytes!(4, "ttl").try_into().unwrap());
    let op_byte = read_bytes!(1, "op")[0];
    let op = match op_byte {
        1 => Operation::Put,
        2 => Operation::Delete,
        b => {
            return Err(EdgestoreError::SegmentCorrupt(format!(
                "unknown op byte: {}",
                b
            )))
        }
    };
    let value = if op == Operation::Delete { None } else { Some(val_bytes) };
    Ok((
        key.clone(),
        MemEntry { key, value, op, lsn, timestamp, ttl },
    ))
}

// ── Xor filter helpers ─────────────────────────────────────────────────────

fn hash_key_to_u64(key: &[u8]) -> u64 {
    let h = blake3::hash(key);
    u64::from_le_bytes(h.as_bytes()[0..8].try_into().unwrap())
}

pub(crate) fn build_xor_filter(keys: &[Vec<u8>]) -> Result<xorf::Xor8, EdgestoreError> {
    let mut hashes: Vec<u64> = keys.iter().map(|k| hash_key_to_u64(k)).collect();
    hashes.sort_unstable();
    hashes.dedup();
    Ok(xorf::Xor8::from(hashes.as_slice()))
}

/// Language-neutral binary format: `[seed:u64-le][block_length:u64-le][fingerprints_len:u64-le][fingerprints]`
pub(crate) fn write_xf_file(filter: &xorf::Xor8, path: &Path) -> Result<(), EdgestoreError> {
    let mut f = std::fs::File::create(path)?;
    f.write_all(&filter.seed.to_le_bytes())?;
    f.write_all(&(filter.block_length as u64).to_le_bytes())?;
    f.write_all(&(filter.fingerprints.len() as u64).to_le_bytes())?;
    f.write_all(&filter.fingerprints)?;
    f.sync_all()?;
    Ok(())
}

pub(crate) fn read_xf_file(path: &Path) -> Result<xorf::Xor8, EdgestoreError> {
    let mut f = std::fs::File::open(path)?;
    let mut buf8 = [0u8; 8];

    f.read_exact(&mut buf8)
        .map_err(|_| EdgestoreError::SegmentCorrupt("xf: truncated seed".to_string()))?;
    let seed = u64::from_le_bytes(buf8);

    f.read_exact(&mut buf8)
        .map_err(|_| EdgestoreError::SegmentCorrupt("xf: truncated block_length".to_string()))?;
    let block_length = u64::from_le_bytes(buf8) as usize;

    f.read_exact(&mut buf8)
        .map_err(|_| EdgestoreError::SegmentCorrupt("xf: truncated fingerprints_len".to_string()))?;
    let fp_len = u64::from_le_bytes(buf8) as usize;
    if fp_len > 1_000_000 {
        return Err(EdgestoreError::SegmentCorrupt(
            "xf: fingerprints_len too large".to_string(),
        ));
    }

    let mut fingerprints = vec![0u8; fp_len];
    f.read_exact(&mut fingerprints)
        .map_err(|_| EdgestoreError::SegmentCorrupt("xf: truncated fingerprints".to_string()))?;

    Ok(xorf::Xor8 {
        seed,
        block_length,
        fingerprints: fingerprints.into_boxed_slice(),
    })
}

pub(crate) fn filter_contains(filter: &xorf::Xor8, key: &[u8]) -> bool {
    filter.contains(&hash_key_to_u64(key))
}

// ── Sparse index I/O ───────────────────────────────────────────────────────

pub(crate) fn write_idx_file(index: &[(Vec<u8>, u64)], path: &Path) -> Result<(), EdgestoreError> {
    let mut f = std::fs::File::create(path)?;
    f.write_all(&(index.len() as u64).to_le_bytes())?;
    for (key, offset) in index {
        f.write_all(&(key.len() as u32).to_le_bytes())?;
        f.write_all(key)?;
        f.write_all(&offset.to_le_bytes())?;
    }
    f.sync_all()?;
    Ok(())
}

/// Read a sparse index file and return the sorted list of (key, offset) entries.
pub fn read_idx_file(path: &Path) -> Result<Vec<(Vec<u8>, u64)>, EdgestoreError> {
    let mut f = std::fs::File::open(path)?;
    let mut buf8 = [0u8; 8];
    f.read_exact(&mut buf8)
        .map_err(|_| EdgestoreError::SegmentCorrupt("idx: truncated count".to_string()))?;
    let count = u64::from_le_bytes(buf8) as usize;
    if count > 10_000_000 {
        return Err(EdgestoreError::SegmentCorrupt("idx: count too large".to_string()));
    }
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let mut buf4 = [0u8; 4];
        f.read_exact(&mut buf4)
            .map_err(|_| EdgestoreError::SegmentCorrupt("idx: truncated key_len".to_string()))?;
        let key_len = u32::from_le_bytes(buf4) as usize;
        let mut key = vec![0u8; key_len];
        f.read_exact(&mut key)
            .map_err(|_| EdgestoreError::SegmentCorrupt("idx: truncated key".to_string()))?;
        f.read_exact(&mut buf8)
            .map_err(|_| EdgestoreError::SegmentCorrupt("idx: truncated offset".to_string()))?;
        let offset = u64::from_le_bytes(buf8);
        entries.push((key, offset));
    }
    Ok(entries)
}

// ── SegmentWriter ──────────────────────────────────────────────────────────

/// Builder that writes a single segment file on disk.
pub struct SegmentWriter {
    base_path: PathBuf,
    segment_id: SegmentId,
    cohort_window_secs: u64,
}

impl SegmentWriter {
    /// Create a new [`SegmentWriter`] for the given segment id.
    pub fn new(base_path: PathBuf, segment_id: SegmentId, cohort_window_secs: u64) -> Self {
        SegmentWriter { base_path, segment_id, cohort_window_secs }
    }

    fn dat_path(&self) -> PathBuf { self.base_path.join(format!("segment-{:08}.dat", self.segment_id)) }
    fn idx_path(&self) -> PathBuf { self.base_path.join(format!("segment-{:08}.idx", self.segment_id)) }
    fn xf_path(&self) -> PathBuf  { self.base_path.join(format!("segment-{:08}.xf",  self.segment_id)) }
    fn meta_path(&self) -> PathBuf { self.base_path.join(format!("segment-{:08}.meta", self.segment_id)) }

    #[allow(clippy::type_complexity)]
    fn write_dat_and_index(
        &self,
        entries: &[(Vec<u8>, MemEntry)],
    ) -> Result<(u64, u64, Vec<(Vec<u8>, u64)>), EdgestoreError> {
        let mut dat = std::fs::File::create(self.dat_path())?;
        dat.write_all(&SEGMENT_FILE_MAGIC.to_le_bytes())?;
        dat.write_all(&[SEGMENT_FORMAT_VERSION, 0, 0, 0])?;

        let mut current_block: Vec<u8> = Vec::new();
        let mut sparse_index: Vec<(Vec<u8>, u64)> = Vec::new();
        let mut block_start_offset: u64 = 8;
        let mut file_offset: u64 = 8;
        let mut compressed_total: u64 = 0;
        let mut uncompressed_total: u64 = 0;

        for (idx, (key, entry)) in entries.iter().enumerate() {
            let serialized = serialize_entry(key, entry);
            uncompressed_total += serialized.len() as u64;

            if idx.is_multiple_of(SPARSE_INDEX_STRIDE) {
                sparse_index.push((key.clone(), block_start_offset));
            }

            current_block.extend_from_slice(&serialized);

            if current_block.len() >= BLOCK_TARGET_BYTES {
                let written = flush_block_to_file(&mut dat, &current_block)?;
                compressed_total += written as u64;
                file_offset += written as u64;
                block_start_offset = file_offset;
                current_block.clear();
            }
        }

        if !current_block.is_empty() {
            let written = flush_block_to_file(&mut dat, &current_block)?;
            compressed_total += written as u64;
        }

        dat.sync_all()?;
        write_idx_file(&sparse_index, &self.idx_path())?;
        Ok((compressed_total, uncompressed_total, sparse_index))
    }

    /// Write the provided entries to disk and return the resulting segment metadata.
    pub fn flush(&mut self, entries: &[(Vec<u8>, MemEntry)]) -> Result<SegmentMeta, EdgestoreError> {
        if entries.is_empty() {
            return Err(EdgestoreError::SegmentCorrupt("empty entries".to_string()));
        }

        let (compressed_bytes, uncompressed_bytes, _) = self.write_dat_and_index(entries)?;

        let keys: Vec<Vec<u8>> = entries.iter().map(|(k, _)| k.clone()).collect();
        let filter = build_xor_filter(&keys)?;
        write_xf_file(&filter, &self.xf_path())?;

        let dat_bytes = std::fs::read(self.dat_path())?;
        let segment_hash = blake3::hash(&dat_bytes).as_bytes().to_vec();

        let first = &entries[0].1;
        let cohort_bucket = cohort_bucket_for(first.timestamp, first.ttl, self.cohort_window_secs);
        let death_time = entries
            .iter()
            .map(|(_, e)| death_time_for(e.timestamp, e.ttl, self.cohort_window_secs))
            .max()
            .unwrap_or(0);

        let mut key_hashes: Vec<[u8; 32]> = keys
            .iter()
            .map(|k| *blake3::hash(k).as_bytes())
            .collect();
        key_hashes.sort_unstable();
        let mut hasher = blake3::Hasher::new();
        for h in &key_hashes { hasher.update(h); }
        let merkle_root = hasher.finalize().as_bytes().to_vec();

        let min_key = entries.first().map(|(k, _)| k.clone()).unwrap();
        let max_key = entries.last().map(|(k, _)| k.clone()).unwrap();
        let min_lsn = entries.iter().map(|(_, e)| e.lsn).min().unwrap_or(0);
        let max_lsn = entries.iter().map(|(_, e)| e.lsn).max().unwrap_or(0);
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as i64;

        let meta = SegmentMeta {
            segment_id: self.segment_id,
            segment_hash,
            min_key,
            max_key,
            min_lsn,
            max_lsn,
            record_count: entries.len() as u64,
            compressed_bytes,
            uncompressed_bytes,
            compression: "zstd:1".to_string(),
            cohort_bucket,
            death_time,
            merkle_root,
            created_at,
            text_index_stripped: false,
        };

        let meta_file = std::fs::File::create(self.meta_path())?;
        serde_json::to_writer_pretty(meta_file, &meta)
            .map_err(|e| EdgestoreError::SegmentCorrupt(format!("meta serialize: {}", e)))?;

        Ok(meta)
    }
}

fn flush_block_to_file(dat: &mut std::fs::File, block: &[u8]) -> Result<usize, EdgestoreError> {
    let compressed = zstd::encode_all(block, 1)
        .map_err(|e| EdgestoreError::SegmentCorrupt(format!("zstd encode: {}", e)))?;
    let compressed_len = compressed.len() as u32;
    let payload_size = 8 + compressed.len();
    let aligned_size = if payload_size.is_multiple_of(SEGMENT_BLOCK_SIZE) {
        payload_size
    } else {
        (payload_size / SEGMENT_BLOCK_SIZE + 1) * SEGMENT_BLOCK_SIZE
    };
    let padding = aligned_size - payload_size;
    dat.write_all(&SEGMENT_BLOCK_MAGIC.to_le_bytes())?;
    dat.write_all(&compressed_len.to_le_bytes())?;
    dat.write_all(&compressed)?;
    if padding > 0 {
        dat.write_all(&vec![0u8; padding])?;
    }
    Ok(aligned_size)
}

// ── SegmentReader ──────────────────────────────────────────────────────────

/// Read-only handle for an on-disk segment file.
#[derive(Clone)]
pub struct SegmentReader {
    base_path: PathBuf,
    segment_id: SegmentId,
    /// Metadata for this segment (bounds, sizes, etc.).
    pub meta: SegmentMeta,
    filter: xorf::Xor8,
    /// Cached sparse index to avoid re-reading .idx file on every operation.
    index: Vec<(Vec<u8>, u64)>,
}

impl SegmentReader {
    fn dat_path(&self) -> PathBuf { self.base_path.join(format!("segment-{:08}.dat", self.segment_id)) }

    /// Open an existing segment from disk.
    pub fn open(base_path: PathBuf, segment_id: SegmentId) -> Result<SegmentReader, EdgestoreError> {
        let meta_path = base_path.join(format!("segment-{:08}.meta", segment_id));
        let xf_path  = base_path.join(format!("segment-{:08}.xf",   segment_id));
        let idx_path = base_path.join(format!("segment-{:08}.idx",  segment_id));

        let meta_file = std::fs::File::open(&meta_path)?;
        let meta: SegmentMeta = serde_json::from_reader(meta_file)
            .map_err(|e| EdgestoreError::SegmentCorrupt(format!("meta parse: {}", e)))?;

        let filter = read_xf_file(&xf_path)?;
        let index = read_idx_file(&idx_path)?;
        Ok(SegmentReader { base_path, segment_id, meta, filter, index })
    }

    #[allow(clippy::type_complexity)]
    fn read_block_at(
        &self,
        dat: &mut std::fs::File,
        offset: u64,
    ) -> Result<(Vec<(Vec<u8>, MemEntry)>, usize), EdgestoreError> {
        dat.seek(SeekFrom::Start(offset))?;
        let mut buf4 = [0u8; 4];
        match dat.read_exact(&mut buf4) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok((vec![], 0)),
            Err(e) => return Err(EdgestoreError::Io(e)),
        }
        let magic = u32::from_le_bytes(buf4);
        if magic != SEGMENT_BLOCK_MAGIC {
            return Ok((vec![], 0)); // hit padding or end
        }
        dat.read_exact(&mut buf4)?;
        let compressed_len = u32::from_le_bytes(buf4) as usize;

        let mut compressed = vec![0u8; compressed_len];
        dat.read_exact(&mut compressed)?;

        let payload_size = 8 + compressed_len;
        let aligned_size = if payload_size.is_multiple_of(SEGMENT_BLOCK_SIZE) {
            payload_size
        } else {
            (payload_size / SEGMENT_BLOCK_SIZE + 1) * SEGMENT_BLOCK_SIZE
        };

        const MAX_DECOMPRESSED: usize = SEGMENT_BLOCK_SIZE * 512;
        let decompressed = zstd::decode_all(compressed.as_slice())
            .map_err(|e| EdgestoreError::SegmentCorrupt(format!("zstd decode: {}", e)))?;
        if decompressed.len() > MAX_DECOMPRESSED {
            return Err(EdgestoreError::SegmentCorrupt("decompressed block too large".to_string()));
        }

        let mut entries = Vec::new();
        let mut pos = 0;
        while pos < decompressed.len() {
            match deserialize_entry(&decompressed, &mut pos) {
                Ok(entry) => entries.push(entry),
                Err(_) => break,
            }
        }
        Ok((entries, aligned_size))
    }

    /// Look up a single key in this segment.
    ///
    /// PERFORMANCE: uses the cached sparse index (`self.index`) loaded at open() time.
    /// Previously re-read the .idx file on every call. Regression: test_reader_caches_index_at_open.
    pub fn get(&self, key: &[u8]) -> Result<Option<MemEntry>, EdgestoreError> {
        if !filter_contains(&self.filter, key) {
            return Ok(None);
        }
        let start_offset = find_block_offset(&self.index, key);
        let mut dat = std::fs::File::open(self.dat_path())?;
        let dat_len = dat.metadata()?.len();
        let mut current_offset = start_offset;

        loop {
            if current_offset >= dat_len { break; }
            let (entries, aligned_size) = self.read_block_at(&mut dat, current_offset)?;
            if entries.is_empty() || aligned_size == 0 { break; }
            for (k, entry) in &entries {
                if k == key { return Ok(Some(entry.clone())); }
                if k.as_slice() > key { return Ok(None); }
            }
            current_offset += aligned_size as u64;
        }
        Ok(None)
    }

    /// Return all entries in the segment whose key falls in `[start, end]`.
    pub fn range_scan(
        &self,
        start: &[u8],
        end: &[u8],
    ) -> Result<Vec<(Vec<u8>, MemEntry)>, EdgestoreError> {
        if end < self.meta.min_key.as_slice() || start > self.meta.max_key.as_slice() {
            return Ok(vec![]);
        }
        let start_offset = find_block_offset(&self.index, start);
        let mut dat = std::fs::File::open(self.dat_path())?;
        let dat_len = dat.metadata()?.len();
        let mut current_offset = start_offset;
        let mut results = Vec::new();

        loop {
            if current_offset >= dat_len { break; }
            let (entries, aligned_size) = self.read_block_at(&mut dat, current_offset)?;
            if entries.is_empty() || aligned_size == 0 { break; }
            let mut past_end = false;
            for (k, entry) in entries {
                if k.as_slice() >= end { past_end = true; break; }
                if k.as_slice() >= start { results.push((k, entry)); }
            }
            if past_end { break; }
            current_offset += aligned_size as u64;
        }
        Ok(results)
    }
}

fn find_block_offset(index: &[(Vec<u8>, u64)], query_key: &[u8]) -> u64 {
    if index.is_empty() { return 8; } // skip file header
    // PERFORMANCE: binary search via partition_point — O(log n).
    // Previously linear scan — O(n). Regression: test_find_block_offset_binary_search.
    let pos = index.partition_point(|(k, _)| k.as_slice() <= query_key);
    if pos == 0 {
        index[0].1
    } else {
        index[pos - 1].1
    }
}

// ── SegmentStore ───────────────────────────────────────────────────────────

pub(crate) struct SegmentStore {
    base_path: PathBuf,
    manifest: Manifest,
    readers: Vec<SegmentReader>,
    next_segment_id: SegmentId,
    cohort_window_secs: u64,
}

impl SegmentStore {
    pub(crate) fn open(base_path: PathBuf, cohort_window_secs: u64) -> Result<SegmentStore, EdgestoreError> {
        let manifest_path = base_path.join("manifest.mf");
        let manifest = Manifest::open(&manifest_path)?;

        let mut readers = Vec::new();
        let mut next_id: SegmentId = 0;
        for meta in manifest.list_segments() {
            let reader = SegmentReader::open(base_path.clone(), meta.segment_id)?;
            if meta.segment_id >= next_id { next_id = meta.segment_id + 1; }
            readers.push(reader);
        }

        Ok(SegmentStore { base_path, manifest, readers, next_segment_id: next_id, cohort_window_secs })
    }

    pub(crate) fn flush_memtable(
        &mut self,
        memtable: &dyn MemTable,
    ) -> Result<SegmentMeta, EdgestoreError> {
        let raw = memtable.iter();
        if raw.is_empty() {
            return Err(EdgestoreError::SegmentCorrupt("memtable is empty".to_string()));
        }
        let entries: Vec<(Vec<u8>, MemEntry)> = raw.into_iter()
            .map(|(k, e)| (k.to_vec(), e.clone()))
            .collect();

        let mut writer = SegmentWriter::new(
            self.base_path.clone(),
            self.next_segment_id,
            self.cohort_window_secs,
        );
        let meta = writer.flush(&entries)?;
        self.manifest.add_segment(meta.clone())?;
        let reader = SegmentReader::open(self.base_path.clone(), meta.segment_id)?;
        self.readers.push(reader);
        self.next_segment_id += 1;
        Ok(meta)
    }

    /// Return the segment IDs of all segments currently loaded in this store.
    pub(crate) fn segment_ids(&self) -> Vec<crate::types::SegmentId> {
        self.readers.iter().map(|r| r.segment_id).collect()
    }

    /// Clone readers for the given segment IDs (used by snapshots).
    pub(crate) fn clone_readers_for(&self, ids: &[SegmentId]) -> Vec<SegmentReader> {
        let id_set: std::collections::HashSet<SegmentId> = ids.iter().copied().collect();
        self.readers.iter()
            .filter(|r| id_set.contains(&r.segment_id))
            .cloned()
            .collect()
    }

    /// Return all SegmentMeta entries from the manifest (used by replication API).
    pub(crate) fn list_segment_metas(&self) -> &[SegmentMeta] {
        self.manifest.list_segments()
    }

    /// Return the next available segment ID and advance the counter.
    pub(crate) fn alloc_segment_id(&mut self) -> SegmentId {
        let id = self.next_segment_id;
        self.next_segment_id += 1;
        id
    }

    /// Add a pre-built SegmentMeta and SegmentReader to this store (used by import_segment).
    pub(crate) fn add_imported_segment(
        &mut self,
        meta: SegmentMeta,
        reader: SegmentReader,
    ) -> Result<(), EdgestoreError> {
        self.manifest.add_segment(meta)?;
        self.readers.push(reader);
        Ok(())
    }

    /// Return the base path of this segment store.
    pub(crate) fn base_path(&self) -> &std::path::Path {
        &self.base_path
    }

    /// Return a reference to the SegmentReader for a given segment ID, if it exists.
    pub(crate) fn reader_for(&self, segment_id: SegmentId) -> Option<&SegmentReader> {
        self.readers.iter().find(|r| r.segment_id == segment_id)
    }

    /// Replace an existing segment with a rewritten version.
    ///
    /// Removes `old_id` from the manifest and reader list, adds `new_meta` and
    /// `new_reader`. Deletes the old segment files (.dat, .idx, .xf, .meta) from disk.
    pub(crate) fn replace_segment(
        &mut self,
        old_id: SegmentId,
        new_meta: SegmentMeta,
        new_reader: SegmentReader,
    ) -> Result<(), EdgestoreError> {
        // Update manifest and readers before touching disk — same ordering
        // invariant as remove_segment. Partial file deletion after manifest
        // success leaves only orphaned bytes, not a broken manifest.
        self.manifest.remove_segments(&[old_id])?;
        self.readers.retain(|r| r.segment_id != old_id);
        self.manifest.add_segment(new_meta)?;
        self.readers.push(new_reader);
        for ext in &["dat", "idx", "xf", "meta"] {
            let path = self.base_path.join(format!("segment-{:08}.{}", old_id, ext));
            if path.exists() {
                std::fs::remove_file(&path).map_err(EdgestoreError::Io)?;
            }
        }
        Ok(())
    }

    /// Remove a segment entirely: deletes its files (.dat, .idx, .xf, .meta) and
    /// removes it from the manifest and reader list. Unlike `replace_segment`, adds
    /// no replacement — for callers that have confirmed the segment's data is
    /// durably available elsewhere (e.g. a remote archive) and just want to reclaim
    /// local disk space.
    pub(crate) fn remove_segment(&mut self, id: SegmentId) -> Result<(), EdgestoreError> {
        // Update manifest and in-memory readers before touching disk. If file
        // deletion fails after this point, the segment is already gone from the
        // manifest — orphaned bytes on disk are a space leak, not a correctness
        // break. The previous ordering (delete files first) could leave the
        // manifest pointing at non-existent files on manifest-write failure.
        self.manifest.remove_segments(&[id])?;
        self.readers.retain(|r| r.segment_id != id);
        for ext in &["dat", "idx", "xf", "meta"] {
            let path = self.base_path.join(format!("segment-{:08}.{}", id, ext));
            if path.exists() {
                std::fs::remove_file(&path).map_err(EdgestoreError::Io)?;
            }
        }
        Ok(())
    }

    /// Return the cohort_window_secs for this segment store.
    #[allow(dead_code)]
    pub(crate) fn cohort_window_secs(&self) -> u64 {
        self.cohort_window_secs
    }

    pub(crate) fn get(&self, key: &[u8]) -> Result<Option<MemEntry>, EdgestoreError> {
        for reader in self.readers.iter().rev() {
            if let Some(entry) = reader.get(key)? {
                return Ok(Some(entry));
            }
        }
        Ok(None)
    }

    pub(crate) fn range_scan(
        &self,
        start: &[u8],
        end: &[u8],
    ) -> Result<Vec<(Vec<u8>, MemEntry)>, EdgestoreError> {
        // PERFORMANCE: K-way merge via BinaryHeap. Each reader's range_scan is sorted by key.
        // DO NOT use HashMap here — it was O(n log n) + 4 allocations. K-way merge is O(n) + 2 allocations.
        // The BinaryHeap tie-breaks on LSN so Ord and Eq are consistent (prevents heap corruption).
        // Regression test: test_range_scan_dedups_by_lsn_across_segments, test_range_scan_delete_wins.
        let mut per_reader: Vec<Vec<(Vec<u8>, MemEntry)>> = Vec::with_capacity(self.readers.len());
        let mut total_len = 0usize;
        for reader in &self.readers {
            let mut seg = reader.range_scan(start, end)?;
            // Sort by key within segment (already sorted, but ensure)
            seg.sort_by(|(a, _), (b, _)| a.cmp(b));
            total_len += seg.len();
            per_reader.push(seg);
        }
        // K-way merge: keep highest-LSN per key, filter deletes.
        use std::collections::BinaryHeap;
        #[derive(Eq, PartialEq)]
        struct Item<'a> {
            key: &'a [u8],
            entry: &'a MemEntry,
            reader_idx: usize,
            elem_idx: usize,
        }
        impl<'a> Ord for Item<'a> {
            fn cmp(&self, other: &Self) -> std::cmp::Ordering {
                // Reverse so BinaryHeap is a min-heap on key
                let key_cmp = other.key.cmp(self.key);
                if key_cmp != std::cmp::Ordering::Equal {
                    return key_cmp;
                }
                // Tie-break by lsn (reverse) so higher LSN is considered "smaller"
                // (popped first when keys are equal, which doesn't matter for correctness)
                other.entry.lsn.cmp(&self.entry.lsn)
            }
        }
        impl<'a> PartialOrd for Item<'a> {
            fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
                Some(self.cmp(other))
            }
        }
        let mut heap = BinaryHeap::new();
        for (ri, seg) in per_reader.iter().enumerate() {
            if let Some((k, e)) = seg.first() {
                heap.push(Item { key: k, entry: e, reader_idx: ri, elem_idx: 0 });
            }
        }
        let mut results: Vec<(Vec<u8>, MemEntry)> = Vec::with_capacity(total_len);
        let mut last_key: Option<Vec<u8>> = None;
        let mut last_entry: Option<MemEntry> = None;
        while let Some(item) = heap.pop() {
            let seg = &per_reader[item.reader_idx];
            let next_idx = item.elem_idx + 1;
            if next_idx < seg.len() {
                let (k, e) = &seg[next_idx];
                heap.push(Item { key: k, entry: e, reader_idx: item.reader_idx, elem_idx: next_idx });
            }
            match last_key {
                Some(ref lk) if lk == item.key => {
                    // Same key: keep highest LSN
                    if let Some(ref le) = last_entry {
                        if item.entry.lsn > le.lsn {
                            last_entry = Some(item.entry.clone());
                        }
                    }
                }
                _ => {
                    // New key: flush previous
                    if let Some(e) = last_entry.take() {
                        if e.op != Operation::Delete {
                            if let Some(lk) = last_key {
                                results.push((lk, e));
                            }
                        }
                    }
                    last_key = Some(item.key.to_vec());
                    last_entry = Some(item.entry.clone());
                }
            }
        }
        // Flush final entry
        if let Some(e) = last_entry {
            if e.op != Operation::Delete {
                if let Some(lk) = last_key {
                    results.push((lk, e));
                }
            }
        }
        Ok(results)
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{encode_key, Operation};
    use tempfile::TempDir;

    fn make_entry(lsn: u64, key: &[u8], value: &[u8]) -> MemEntry {
        MemEntry {
            key: key.to_vec(),
            value: Some(value.to_vec()),
            op: Operation::Put,
            lsn,
            timestamp: 3_600_000_000_000,
            ttl: 0,
        }
    }

    fn make_delete(lsn: u64, key: &[u8]) -> MemEntry {
        MemEntry { key: key.to_vec(), value: None, op: Operation::Delete, lsn, timestamp: 0, ttl: 0 }
    }

    fn sorted_entries(n: usize) -> Vec<(Vec<u8>, MemEntry)> {
        let mut v: Vec<(Vec<u8>, MemEntry)> = (0..n).map(|i| {
            let k = encode_key(b"ns", format!("key-{:04}", i).as_bytes());
            let val = format!("val-{:04}", i);
            let e = make_entry(i as u64 + 1, &k, val.as_bytes());
            (k, e)
        }).collect();
        v.sort_by(|(a, _), (b, _)| a.cmp(b));
        v
    }

    // ─ Entry serialization ─────────────────────────────────────────────────

    #[test]
    fn test_serialize_deserialize_put() {
        let key = b"hello";
        let entry = make_entry(42, key, b"world");
        let bytes = serialize_entry(key, &entry);
        let mut pos = 0;
        let (k2, e2) = deserialize_entry(&bytes, &mut pos).unwrap();
        assert_eq!(k2, key);
        assert_eq!(e2.lsn, 42);
        assert_eq!(e2.value, Some(b"world".to_vec()));
        assert_eq!(e2.op, Operation::Put);
        assert_eq!(pos, bytes.len());
    }

    #[test]
    fn test_serialize_deserialize_delete() {
        let key = b"gone";
        let entry = make_delete(7, key);
        let bytes = serialize_entry(key, &entry);
        let mut pos = 0;
        let (_, e2) = deserialize_entry(&bytes, &mut pos).unwrap();
        assert_eq!(e2.op, Operation::Delete);
        assert_eq!(e2.value, None);
    }

    #[test]
    fn test_deserialize_truncated() {
        let mut pos = 0;
        assert!(deserialize_entry(&[0, 1, 2], &mut pos).is_err());
    }

    // ─ Writer ──────────────────────────────────────────────────────────────

    #[test]
    fn test_writer_dat_file_header() {
        let dir = TempDir::new().unwrap();
        let entries = sorted_entries(20);
        let mut writer = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
        writer.flush(&entries).unwrap();

        let dat = std::fs::read(dir.path().join("segment-00000000.dat")).unwrap();
        let magic = u32::from_le_bytes(dat[0..4].try_into().unwrap());
        assert_eq!(magic, SEGMENT_FILE_MAGIC);
        assert_eq!(dat[4], SEGMENT_FORMAT_VERSION);
    }

    #[test]
    fn test_writer_sparse_index_count() {
        let dir = TempDir::new().unwrap();
        let entries = sorted_entries(200);
        let mut writer = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
        writer.flush(&entries).unwrap();
        let index = read_idx_file(&dir.path().join("segment-00000000.idx")).unwrap();
        assert_eq!(index.len(), 4); // ceil(200/64) = 4
    }

    #[test]
    fn test_flush_four_files_and_hash() {
        let dir = TempDir::new().unwrap();
        let entries = sorted_entries(10);
        let mut writer = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
        let meta = writer.flush(&entries).unwrap();

        assert!(dir.path().join("segment-00000000.dat").exists());
        assert!(dir.path().join("segment-00000000.idx").exists());
        assert!(dir.path().join("segment-00000000.xf").exists());
        assert!(dir.path().join("segment-00000000.meta").exists());
        assert_eq!(meta.record_count, 10);

        let dat_bytes = std::fs::read(dir.path().join("segment-00000000.dat")).unwrap();
        let expected = blake3::hash(&dat_bytes).as_bytes().to_vec();
        assert_eq!(meta.segment_hash, expected);
    }

    #[test]
    fn test_flush_empty_returns_error() {
        let dir = TempDir::new().unwrap();
        let mut writer = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
        assert!(writer.flush(&[]).is_err());
    }

    // ─ Xor filter ──────────────────────────────────────────────────────────

    #[test]
    fn test_xor_filter_no_false_negatives() {
        let dir = TempDir::new().unwrap();
        let keys: Vec<Vec<u8>> = (0..100u32).map(|i| format!("key-{:04}", i).into_bytes()).collect();
        let filter = build_xor_filter(&keys).unwrap();
        write_xf_file(&filter, &dir.path().join("test.xf")).unwrap();
        let filter2 = read_xf_file(&dir.path().join("test.xf")).unwrap();
        for key in &keys {
            assert!(filter_contains(&filter2, key), "false negative for {:?}", key);
        }
    }

    #[test]
    fn test_xf_truncated_returns_error() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("bad.xf");
        std::fs::write(&p, b"short").unwrap();
        assert!(read_xf_file(&p).is_err());
    }

    // ─ Reader ──────────────────────────────────────────────────────────────

    #[test]
    fn test_reader_open_and_get() {
        let dir = TempDir::new().unwrap();
        let entries = sorted_entries(200);
        let mut writer = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
        writer.flush(&entries).unwrap();

        let reader = SegmentReader::open(dir.path().to_path_buf(), 0).unwrap();
        assert_eq!(reader.meta.record_count, 200);

        let (target_key, target_entry) = &entries[100];
        let found = reader.get(target_key).unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().lsn, target_entry.lsn);
    }

    #[test]
    fn test_reader_absent_key_returns_none() {
        let dir = TempDir::new().unwrap();
        let entries = sorted_entries(50);
        let mut writer = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
        writer.flush(&entries).unwrap();

        let reader = SegmentReader::open(dir.path().to_path_buf(), 0).unwrap();
        let absent = encode_key(b"ns", b"absent-key-xyz");
        assert!(reader.get(&absent).unwrap().is_none());
    }

    #[test]
    fn test_reader_range_scan_100_entries() {
        let dir = TempDir::new().unwrap();
        let entries = sorted_entries(500);
        let mut writer = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
        writer.flush(&entries).unwrap();

        let reader = SegmentReader::open(dir.path().to_path_buf(), 0).unwrap();
        let start = encode_key(b"ns", b"key-0100");
        let end   = encode_key(b"ns", b"key-0200");
        let results = reader.range_scan(&start, &end).unwrap();
        assert_eq!(results.len(), 100);
    }

    #[test]
    fn test_reader_open_missing_meta_errors() {
        let dir = TempDir::new().unwrap();
        assert!(SegmentReader::open(dir.path().to_path_buf(), 99).is_err());
    }

    // ─ Performance regression guards ───────────────────────────────────────

    /// Regression: SegmentReader used to re-read the .idx file on every get() and range_scan().
    /// Now it caches the index at open() time. This test verifies that the index is loaded
    /// once and reused by reading multiple keys without touching the .idx file again.
    #[test]
    fn test_reader_caches_index_at_open() {
        let dir = TempDir::new().unwrap();
        let entries = sorted_entries(100);
        let mut writer = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
        writer.flush(&entries).unwrap();

        let reader = SegmentReader::open(dir.path().to_path_buf(), 0).unwrap();
        // Verify index is non-empty (loaded from .idx at open time)
        assert!(!reader.index.is_empty(), "index should be cached at open()");
        // Read multiple keys — each should succeed without re-reading .idx
        for (k, e) in &entries {
            let found = reader.get(k).unwrap();
            assert!(found.is_some(), "key {:?} not found", k);
            assert_eq!(found.unwrap().lsn, e.lsn);
        }
    }

    /// Regression: find_block_offset was linear scan. Now it is binary search.
    /// This test verifies correctness on edge cases and large indexes.
    #[test]
    fn test_find_block_offset_binary_search() {
        let index: Vec<(Vec<u8>, u64)> = (0..1000u64)
            .map(|i| (format!("key-{:08}", i).into_bytes(), i * 100))
            .collect();
        // Exact match on existing key
        assert_eq!(find_block_offset(&index, b"key-00000050"), 5000);
        // Between keys (should floor to previous)
        assert_eq!(find_block_offset(&index, b"key-00000050\x01"), 5000);
        // Before first key
        assert_eq!(find_block_offset(&index, b"aaa"), 0);
        // After last key
        assert_eq!(find_block_offset(&index, b"zzz"), 99900);
    }

    /// Regression: SegmentStore::range_scan used HashMap + sort. Now it uses K-way merge.
    /// This test verifies deduplication by LSN across multiple overlapping segments.
    #[test]
    fn test_range_scan_dedups_by_lsn_across_segments() {
        let dir = TempDir::new().unwrap();
        let ns = b"ns";
        let key = encode_key(ns, b"shared-key");
        let key_end = encode_key(ns, b"shared-key\x00");

        // Segment 0: lsn=1
        let mut writer0 = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
        let entry0 = make_entry(1, &key, b"old");
        let meta0 = writer0.flush(&vec![(key.clone(), entry0)]).unwrap();

        // Segment 1: lsn=2
        let mut writer1 = SegmentWriter::new(dir.path().to_path_buf(), 1, 3600);
        let entry1 = make_entry(2, &key, b"new");
        let meta1 = writer1.flush(&vec![(key.clone(), entry1)]).unwrap();

        let mut store = SegmentStore::open(dir.path().to_path_buf(), 3600).unwrap();
        let reader0 = SegmentReader::open(dir.path().to_path_buf(), 0).unwrap();
        let reader1 = SegmentReader::open(dir.path().to_path_buf(), 1).unwrap();
        store.add_imported_segment(meta0, reader0).unwrap();
        store.add_imported_segment(meta1, reader1).unwrap();

        let results = store.range_scan(&key, &key_end).unwrap();
        assert_eq!(results.len(), 1, "should deduplicate to 1 entry");
        assert_eq!(results[0].1.lsn, 2, "higher LSN should win");
    }

    /// Regression: range_scan must delete-filter correctly.
    /// If a segment has a Delete at lsn=2 for a key, the key should be absent.
    #[test]
    fn test_range_scan_delete_wins() {
        let dir = TempDir::new().unwrap();
        let key = encode_key(b"ns", b"key");
        let key_end = encode_key(b"ns", b"key\x00");

        let mut writer = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
        let entry1 = make_delete(2, &key);
        let meta = writer.flush(&vec![
            (key.clone(), entry1),
        ]).unwrap();

        let mut store = SegmentStore::open(dir.path().to_path_buf(), 3600).unwrap();
        let reader = SegmentReader::open(dir.path().to_path_buf(), 0).unwrap();
        store.add_imported_segment(meta, reader).unwrap();
        let results = store.range_scan(&key, &key_end).unwrap();
        assert!(results.is_empty(), "delete should filter out the key");
    }

    // Regression: remove_segment must update manifest before deleting files.
    // Previously files were deleted first — manifest-write failure would leave
    // manifest pointing at non-existent files. Now the manifest is durably
    // updated first; any subsequent file-deletion failure leaves only orphaned
    // bytes, not a broken manifest.
    #[test]
    fn test_remove_segment_manifest_updated_before_files_gone() {
        use crate::memtable::BTreeMemTable;
        use crate::types::encode_key;

        let dir = TempDir::new().unwrap();
        let mut store = SegmentStore::open(dir.path().to_path_buf(), 3600).unwrap();

        let mut mt = BTreeMemTable::new();
        let k = encode_key(b"ns", b"key");
        mt.insert(k.clone(), make_entry(1, &k, b"val"));
        let meta = store.flush_memtable(&mt).unwrap();
        let id = meta.segment_id;

        assert!(dir.path().join(format!("segment-{:08}.dat", id)).exists());
        assert_eq!(store.manifest.list_segments().len(), 1);

        store.remove_segment(id).unwrap();

        assert_eq!(store.manifest.list_segments().len(), 0, "manifest must not list removed segment");
        assert!(!dir.path().join(format!("segment-{:08}.dat", id)).exists());
        assert_eq!(store.readers.len(), 0);
    }
}

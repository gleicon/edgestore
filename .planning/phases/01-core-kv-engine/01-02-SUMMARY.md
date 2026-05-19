# Plan 01-02 Summary — WAL Implementation

**Status:** Complete
**Date:** 2026-05-18

## What was built

- `edgestore/src/wal.rs`: Full WAL implementation

## Interfaces exported

- `WAL_MAGIC: [u8; 4]` = [0x45, 0x44, 0x47, 0x57]
- `WAL_FORMAT_VERSION: u8` = 1
- `WAL_HEADER_LEN: usize` = 8
- `WalWriter::create(path, config) -> Result<WalWriter>`
- `WalWriter::open(path, config) -> Result<WalWriter>` (config required for rotation thresholds)
- `WalWriter::append(&mut self, record) -> Result<()>` (no fsync — caller's responsibility)
- `WalWriter::fsync(&mut self) -> Result<()>`
- `WalWriter::needs_rotation(&self, now_secs) -> bool`
- `WalReader::open(path) -> Result<WalReader>`
- `WalReader::read_records(&mut self) -> Vec<WalRecord>` (skips corrupt, stops at truncation)

## Key behaviors

- WAL file header: 8 bytes = [EDGW magic 4B][version=1][reserved 0,0,0]
- Frame format: {crc32c:u32-le}{compressed_len:u32-le}{lz4_payload}
- LZ4 via lz4_flex::compress_prepend_size (prepends uncompressed length)
- CRC32C computed over compressed bytes (not raw)
- append() does NOT fsync — group commit calls fsync explicitly
- WalWriter::open always takes config so needs_rotation() is always valid
- value_hash [u8; 32] written after key_bytes, before val_len — permanent format

## Verification

- All WAL unit tests pass (9/9)
- cargo clippy -D warnings clean

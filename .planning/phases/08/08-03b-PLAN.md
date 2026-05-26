---
plan_id: "08-03b"
phase: "08"
wave: 2
depends_on: ["08-01", "08-03a"]
requirements_addressed: ["POLISH-04"]
files_modified:
  - "edgestore-cli/src/main.rs"
  - ".cargo/config.toml"
  - "README.md"
autonomous: true
---

# Plan 08-03b: CLI Advanced Subcommands and Build Config

## Objective

Implement maintenance (compact), data exchange (export/import), search (vector-search, text-search), and build optimization for the CLI binary.

**Purpose:** Complete the administrative CLI with all remaining subcommands.

**Output:** Fully functional `edgestore-cli` with 10 subcommands and optimized release build.

## must_haves

- Subcommands: compact, export, import, vector-search, text-search
- Uses clap v4 with derive macro
- .cargo/config.toml with release optimizations
- All subcommands tested manually

## Tasks

<task>
  <id>08-03b-T1</id>
  <title>Implement maintenance subcommand (compact)</title>
  <action>
    Implement Compact subcommand: --path required, optional --write-budget-bytes for incremental compaction limit. Opens Engine, calls engine.compact_once() or engine.compact() with budget if provided. Prints compaction summary: segments collected, segments created, bytes relocated, cohorts processed. Use human-readable byte formatting (e.g., "1.2 MB relocated"). Handle errors gracefully: print user-friendly message if database is busy or locked.
  </action>
  <read_first>
    - edgestore/src/compaction.rs (for compaction API)
    - edgestore/src/engine.rs (for Engine methods)
  </read_first>
  <acceptance_criteria>
    - criterion 1: `compact --path /tmp/db` runs compaction successfully
    - criterion 2: Output shows segments before/after count
    - criterion 3: --write-budget-bytes option accepted and passed to engine
    - criterion 4: Human-readable byte formatting used
    - criterion 5: Error handling for locked/busy database
    - criterion 6: Exit code 0 on success, 1 on error
  </acceptance_criteria>
</task>

<task>
  <id>08-03b-T2</id>
  <title>Implement data exchange subcommands (export, import)</title>
  <action>
    Implement Export and Import subcommands for backup/restore. Export: --path (db), --output (file or directory), optional --format (json, binary). Dumps all key-value pairs from default or specified namespace. If directory output, creates manifest.json and data files. Import: --path (db), --input (file or directory), optional --format. Imports key-value pairs, handles conflicts with LWW (last write wins based on import order). Print progress every 1000 keys for large datasets. Export format: JSON array of {namespace, key, value, ttl?} objects, or binary format with length-prefixed records. Implement streaming for large datasets (don't hold all in memory).
  </action>
  <read_first>
    - edgestore/src/lib.rs (Engine::range for export)
    - edgestore/src/engine.rs (put methods for import)
  </read_first>
  <acceptance_criteria>
    - criterion 1: `export --path /tmp/db --output /tmp/backup.json` creates valid JSON
    - criterion 2: Exported JSON can be re-imported with identical results
    - criterion 3: `import --path /tmp/db --input /tmp/backup.json` restores data
    - criterion 4: Progress printed for large datasets
    - criterion 5: Binary export format also supported (--format binary)
    - criterion 6: Streaming implementation (constant memory usage regardless of DB size)
  </acceptance_criteria>
</task>

<task>
  <id>08-03b-T3</id>
  <title>Implement vector and text search subcommands</title>
  <action>
    Implement Vector subcommand group with sub-subcommands: vector-put, vector-get, vector-search. VectorPut: --path, --namespace, --key, --dims, --data (hex-encoded f32/f16 bytes), optional --dtype (f32, f16, i8). VectorGet: --path, --namespace, --key. Returns vector data (hex-encoded). VectorSearch: --path, --namespace, --query (hex-encoded query vector), --k (number of results), --metric (cosine, dot, euclidean). Returns top-k results: key=distance per line. Implement TextSearch subcommand: --path, --namespace, --query (search text), optional --k (max results). Returns BM25-ranked results with scores. All search commands validate inputs and produce clear errors. Use the vector and text APIs from edgestore crate.
  </action>
  <read_first>
    - edgestore/src/vector.rs (for vector API)
    - edgestore/src/text.rs or search module (for text search API)
    - edgestore/src/lib.rs (public API modules)
  </read_first>
  <acceptance_criteria>
    - criterion 1: `vector-put` stores vector with specified dims/dtype
    - criterion 2: `vector-get` retrieves stored vector correctly
    - criterion 3: `vector-search` returns top-k nearest vectors
    - criterion 4: Distance metric selection works (cosine, dot, euclidean)
    - criterion 5: `text-search` returns BM25-ranked results
    - criterion 6: Hex encoding/decoding works for binary vector data
  </acceptance_criteria>
</task>

<task>
  <id>08-03b-T4</id>
  <title>Add .cargo/config.toml and verify CLI build</title>
  <action>
    Create .cargo/config.toml in repo root with release profile settings: optimized for speed, LTO (link-time optimization) for release builds. After config is added, run full CLI verification: `cargo build --release -p edgestore-cli`, test all subcommands with a real database, verify help text is comprehensive, verify error handling works. Document CLI installation in README: `cargo install --path edgestore-cli` or `cargo install edgestore-cli` after crates.io publish.
  </action>
  <read_first>
    - Cargo.toml workspace structure
    - Existing .cargo/config.toml if present
  </read_first>
  <acceptance_criteria>
    - criterion 1: .cargo/config.toml exists with release optimizations
    - criterion 2: `cargo build --release -p edgestore-cli` produces optimized binary
    - criterion 3: All subcommands tested manually (create, put, get, delete, range, compact, stats, export, import, vector-search, text-search)
    - criterion 4: --help text comprehensive for all subcommands
    - criterion 5: README documents installation instructions
    - criterion 6: Binary size is reasonable (< 10MB stripped)
  </acceptance_criteria>
</task>

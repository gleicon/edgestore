---
plan_id: "08-02b"
phase: "08"
wave: 1
depends_on: []
requirements_addressed: ["POLISH-03", "POLISH-07"]
files_modified:
  - "BENCHMARKS.md"
  - "LICENSE-MIT"
  - "LICENSE-APACHE"
  - "examples/basic_kv.rs"
  - "examples/vector_search.rs"
  - "examples/replication.rs"
autonomous: true
---

# Plan 08-02b: Benchmarks, Examples, and Licenses

## Objective

Create benchmark documentation, runnable examples demonstrating all major features, and dual MIT/Apache-2.0 license files.

**Purpose:** Users can run examples to learn the API and understand performance characteristics.

**Output:** BENCHMARKS.md, examples/ directory, LICENSE files at repo root.

## must_haves

- BENCHMARKS.md with performance results and methodology
- LICENSE-MIT and LICENSE-APACHE files
- examples/ directory with 3+ runnable examples
- All examples compile with `cargo build --examples`

## Tasks

<task>
  <id>08-02b-T1</id>
  <title>Create BENCHMARKS.md with performance results</title>
  <action>
    Create BENCHMARKS.md documenting the benchmark suite. Include: 1) Overview of benchmark methodology (criterion.rs based, statistical rigor), 2) Hardware/environment details (CPU, RAM, SSD type), 3) Results tables for: a) Write throughput (ops/sec for sequential/random writes), b) Read throughput (point lookups, range scans), c) Vector search latency (p50, p99 for 10K/100K/500K vectors), d) HNSW recall vs latency tradeoff, e) Text search QPS, f) Compaction overhead (WAF measurements), 4) How to run benchmarks section with `cargo bench` commands, 5) Interpreting results guide. For v1.0, include placeholder tables with expected ranges; actual numbers can be filled after running benchmarks. Mark which benchmarks are in-repo vs need hardware.
  </action>
  <read_first>
    - edgestore/Cargo.toml (bench sections)
    - edgestore/benches/ directory (if exists)
    - .planning/REQUIREMENTS.md (VECTOR-05, SSD-05 for benchmark requirements)
  </read_first>
  <acceptance_criteria>
    - criterion 1: BENCHMARKS.md exists at repo root
    - criterion 2: Documents all benchmark binaries in Cargo.toml
    - criterion 3: Contains tables for all required measurements (per REQUIREMENTS.md)
    - criterion 4: Instructions for running each benchmark
    - criterion 5: Hardware/environment section for reproducibility
    - criterion 6: Placeholder results or actual results from recent run
  </acceptance_criteria>
</task>

<task>
  <id>08-02b-T2</id>
  <title>Create runnable examples</title>
  <action>
    Create examples/ directory at repo root with three standalone .rs files: 1) examples/basic_kv.rs: open DB, put 3 keys with different namespaces, get one key, range scan, delete, close; 2) examples/vector_search.rs: create vector collection, insert 1000 random f32 vectors, perform ANN search, print top-5 results; 3) examples/replication.rs: setup two engines, write to primary, export manifest, import to replica, verify sync. Each example should be a complete main.rs that compiles with `cargo run --example <name>`. Add doc comments at top explaining what the example demonstrates. Ensure examples reference published crate versions or use path = "../edgestore" for local development.
  </action>
  <read_first>
    - edgestore/src/lib.rs (for public API)
    - edgestore/examples/ (if exists, use as template)
  </read_first>
  <acceptance_criteria>
    - criterion 1: examples/ directory exists at repo root
    - criterion 2: examples/basic_kv.rs compiles and runs
    - criterion 3: examples/vector_search.rs compiles and runs
    - criterion 4: examples/replication.rs compiles and runs
    - criterion 5: All examples have doc comments explaining purpose
    - criterion 6: `cargo build --examples` passes
  </acceptance_criteria>
</task>

<task>
  <id>08-02b-T3</id>
  <title>Add dual MIT/Apache-2.0 license files</title>
  <action>
    Create LICENSE-MIT file with standard MIT license text (Copyright 2026 EdgeStore Contributors). Create LICENSE-APACHE file with standard Apache-2.0 license text. Both files at repo root. Use the standard license texts from https://opensource.org/licenses/MIT and https://www.apache.org/licenses/LICENSE-2.0. Ensure the Apache license is the full text version, not just the header.
  </action>
  <read_first>
    - Standard MIT license template
    - Standard Apache-2.0 license template
  </read_first>
  <acceptance_criteria>
    - criterion 1: LICENSE-MIT exists at repo root with MIT license text
    - criterion 2: LICENSE-APACHE exists at repo root with Apache-2.0 text
    - criterion 3: Copyright year is 2026
    - criterion 4: License files are complete (not truncated)
    - criterion 5: Cargo.toml references these licenses correctly
  </acceptance_criteria>
</task>

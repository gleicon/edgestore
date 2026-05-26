---
plan_id: "08-04a"
phase: "08"
wave: 3
depends_on: ["08-01", "08-02a", "08-02b", "08-03a", "08-03b"]
requirements_addressed: ["POLISH-05", "POLISH-06"]
files_modified:
  - "edgestore/tests/integration_v1.rs"
  - "Cargo.toml"
  - "edgestore/Cargo.toml"
  - "edgestore-tokio/Cargo.toml"
  - "edgestore-repl/Cargo.toml"
  - "edgestore-cli/Cargo.toml"
  - "RELEASE_CHECKLIST.md"
autonomous: true
---

# Plan 08-04a: Final Integration Test and Metadata

## Objective

Create the comprehensive cross-feature integration test and finalize all workspace Cargo.toml metadata and release checklist.

**Purpose:** Validate all v1.0 features work together and ensure release metadata is complete.

**Output:** Passing integration test and finalized release artifacts.

## must_haves

- Cross-feature integration test covering KV + vector + text + compaction + snapshot + replication + transactions
- All workspace crates versioned at 1.0.0 with complete metadata
- RELEASE_CHECKLIST.md documenting release steps

## Tasks

<task>
  <id>08-04a-T1</id>
  <title>Create comprehensive cross-feature integration test</title>
  <action>
    Create edgestore/tests/integration_v1.rs with a comprehensive end-to-end test that exercises: 1) KV operations: put, get, delete, range across multiple namespaces, 2) TTL: put_with_ttl, verify data expires correctly after cohort window, 3) Compaction: trigger compaction, verify expired data is collected, 4) Snapshots: create snapshot, verify data consistency, release snapshot, 5) Vector operations: vector_put, vector_get, vector_search with different metrics, 6) Text search: index_text, search_text, verify BM25 ranking, 7) Replication: setup two engines, write to primary, export manifest, compare merkle, import segments to replica, verify sync, 8) Transactions: begin, multiple puts, commit, rollback. The test should be one large #[test] function or multiple organized tests. Use temporary directories for isolation. Assert on expected behavior at each step. Test should take < 60 seconds to run. Document what each section tests with code comments.
  </action>
  <read_first>
    - edgestore/tests/ (existing test patterns)
    - edgestore/src/lib.rs (all public APIs)
    - edgestore/src/engine.rs (Engine methods)
    - edgestore/src/transaction.rs (Transaction API)
    - edgestore/src/vector.rs (Vector API)
  </read_first>
  <acceptance_criteria>
    - criterion 1: tests/integration_v1.rs exists with comprehensive test
    - criterion 2: Test exercises all major features: KV, TTL, compaction, snapshots, vectors, text search, replication, transactions
    - criterion 3: Test runs in < 60 seconds
    - criterion 4: Test passes with `cargo test --test integration_v1`
    - criterion 5: Each major feature section has explanatory comments
    - criterion 6: Test uses temporary directories and cleans up
  </acceptance_criteria>
</task>

<task>
  <id>08-04a-T2</id>
  <title>Finalize all workspace Cargo.toml metadata and release checklist</title>
  <action>
    Update all workspace crates to version "1.0.0" with final metadata. For each crate (edgestore, edgestore-tokio, edgestore-repl, edgestore-cli): verify version is "1.0.0", description is present and accurate, license = "MIT OR Apache-2.0", repository URL is correct, keywords are present (max 5), categories are correct. For edgestore (the main library), add: keywords = ["database", "kv-store", "embedded", "vector", "ssd"], categories = ["database-implementations", "data-structures", "caching"]. Ensure workspace root Cargo.toml has [workspace.package] section with shared metadata that child crates can inherit. Verify all dependency versions are pinned to compatible ranges (not "*" for published crates). Check that path dependencies are only in dev-dependencies or properly handled. Then create RELEASE_CHECKLIST.md documenting the release steps: verify all checks pass, tag commit with v1.0.0, push to GitHub, publish to crates.io in order (edgestore first, then dependents), create GitHub release with notes.
  </action>
  <read_first>
    - Cargo.toml (workspace root)
    - edgestore/Cargo.toml
    - edgestore-tokio/Cargo.toml
    - edgestore-repl/Cargo.toml
    - edgestore-cli/Cargo.toml
  </read_first>
  <acceptance_criteria>
    - criterion 1: All crates have version = "1.0.0"
    - criterion 2: All crates have complete metadata (description, license, repository)
    - criterion 3: edgestore has keywords and categories for crates.io discoverability
    - criterion 4: No "*" version dependencies in [dependencies] sections
    - criterion 5: Workspace uses [workspace.package] for shared fields
    - criterion 6: `cargo metadata --format-version 1` parses successfully
    - criterion 7: RELEASE_CHECKLIST.md created with step-by-step release process
  </acceptance_criteria>
</task>

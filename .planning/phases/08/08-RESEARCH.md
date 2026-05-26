# Phase 08 Research — v1.0 Polish & Release

**Date:** 2026-05-25
**Phase:** 08 — v1.0 Polish & Release

## Research Question
What do we need to know to plan the final release-engineering phase well?

## Key Findings

### 1. CLI Framework: `clap` v4
- Standard in Rust ecosystem; used by `cargo`, `ripgrep`, etc.
- Derive macro reduces boilerplate; supports subcommands, flags, positional args
- No runtime cost to library crate if CLI is a separate binary crate
- Decision: use `clap` with derive API in a new `edgestore-cli` crate

### 2. crates.io Readiness Requirements
- `Cargo.toml` must have: `name`, `version`, `description`, `license` (or `license-file`), `repository`, `authors` (optional but recommended)
- `README.md` is displayed on crates.io page
- All dependencies must be published to crates.io (no git/path-only deps for publish)
- `cargo publish --dry-run` validates packaging without uploading
- `Cargo.toml` can use `workspace.package` inheritance for shared metadata

### 3. Rustdoc Best Practices
- `#![warn(missing_docs)]` in `lib.rs` to enforce coverage
- Module-level docs with `//!` at top of each file
- Public trait methods and structs need doc comments
- `cargo doc --workspace --no-deps` to check warnings
- Decision: add `#![warn(missing_docs)]` to `edgestore/src/lib.rs`

### 4. Documentation Structure
- README: quick-start + feature matrix + links
- ARCHITECTURE.md: component overview, data flow diagrams, file format summary
- CHANGELOG.md: Keep a Changelog format, versions + dates
- examples/: standalone `.rs` files that compile with `cargo build --examples`

### 5. License
- Rust ecosystem standard: dual MIT/Apache-2.0
- Requires `LICENSE-MIT` and `LICENSE-APACHE` files in repo root
- Crates.io accepts SPDX expression: `MIT OR Apache-2.0`

### 6. Feature Flags Review
- Current workspace has many features across phases (vector, text, replication, HNSW, etc.)
- Need to ensure optional deps are behind feature flags where appropriate
- `default` feature should include core KV + segment store
- `full` feature could enable everything for convenience
- Decision: audit each `Cargo.toml` and gate non-core deps

### 7. Cross-Feature Integration Test
- Must exercise KV put/get, vector search, text search, compaction, snapshot, and replication in one test
- Serves as a "smoke test" for the full v1.0 surface
- Should run in CI and be documented in README

## Validation Architecture
- No Nyquist validation needed (no AI system, no complex algorithm)
- Quality gates: clippy, rustdoc, tests, dry-run publish
- Acceptance: manual review of README and docs

## Risks
- Adding `#![warn(missing_docs)]` may surface hundreds of warnings — budget time
- CLI binary design may reveal API gaps (e.g. missing `Engine` methods needed for CLI) — fix inline
- crates.io dry-run may fail due to unpublished path deps — verify workspace dependency types

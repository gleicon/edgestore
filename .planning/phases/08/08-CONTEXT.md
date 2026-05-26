# Phase 08: v1.0 Polish & Release - Context

**Gathered:** 2026-05-25
**Status:** Ready for planning
**Source:** Phase definition from ROADMAP.md + project state review

<domain>
## Phase Boundary

Phase 8 is the final release-engineering phase before v1.0. All feature work (Phases 1–7) is complete. This phase does NOT add new features. It polishes the existing codebase for production use and crates.io publication.

What this phase delivers:
- Public API audit and rustdoc coverage
- Comprehensive README, architecture guide, and CHANGELOG
- `edgestore-cli` administrative binary
- Workspace Cargo.toml metadata and LICENSE
- Final cross-feature integration validation
- crates.io publish readiness

What this phase does NOT deliver:
- New database features
- New indexing algorithms
- Breaking API changes (only polish, cleanup, and documentation)
</domain>

<decisions>
## Implementation Decisions

### API Polish
- All `pub` items in `edgestore` crate must have rustdoc comments
- Remove any `pub` that should be `pub(crate)`
- Feature flags: review optional dependencies; ensure they are gated correctly
- Keep `edgestore-tokio` as separate crate; do not merge into main crate

### Documentation
- README at repo root: quick-start, architecture diagram (ASCII or linked), feature matrix
- `ARCHITECTURE.md` at repo root or in `docs/`: component overview, data flow, file formats
- `CHANGELOG.md`: summarize all phases 1–7 with date and commit range
- API examples in `examples/` directory

### CLI Binary
- New crate `edgestore-cli` in workspace (or binary in `edgestore` crate if preferred)
- Use `clap` for argument parsing
- Subcommands: `create`, `put`, `get`, `delete`, `range`, `compact`, `stats`, `export`, `import`, `vector-search`, `text-search`
- CLI must NOT add mandatory dependencies to `edgestore` library crate

### Release Engineering
- Version all workspace crates to `1.0.0`
- License: MIT OR Apache-2.0 (dual license, Rust standard)
- `cargo publish --dry-run` must pass
- `.cargo/config.toml` if needed for build profiles

### the agent's Discretion
- Exact CLI subcommand naming and flags
- Whether `edgestore-cli` is a separate crate or a `[[bin]]` in `edgestore`
- Specific rustdoc style (detailed vs concise)
- Choice of diagram tool for architecture diagram
</decisions>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Project Structure
- `Cargo.toml` (workspace root) — crate definitions, dependencies, versions
- `edgestore/Cargo.toml` — main crate metadata
- `edgestore-tokio/Cargo.toml` — async wrapper metadata
- `.planning/PROJECT.md` — project context and decisions
- `.planning/REQUIREMENTS.md` — all requirements with REQ-IDs
- `.planning/ROADMAP.md` — 8 phases, success criteria, risks
- `.planning/STATE.md` — current progress and stats
- `prod.md` — full design spec (source of truth for architecture)

### Prior Phase Summaries (for cross-feature context)
- `.planning/phases/01/01-SUMMARY.md` through `.planning/phases/07/07-SUMMARY.md` (if they exist)
- Phase integration tests in `edgestore/tests/`

### Code Quality Baseline
- `cargo clippy --workspace -D warnings` (must pass)
- `cargo test --workspace` (421 tests pass currently)
- `cargo bench --no-run` (4 benchmarks compile)
</canonical_refs>

<specifics>
## Specific Ideas

- README quick-start should show: open DB, put, get, close — 10 lines max
- Architecture diagram should show: App → Transaction → Memtable → SegmentStore → SSD
- CLI `stats` subcommand should print: segment count, memtable size, WAL size, compaction stats
- CHANGELOG should follow Keep a Changelog format (https://keepachangelog.com/)
- Examples: `examples/basic_kv.rs`, `examples/vector_search.rs`, `examples/replication.rs`
</specifics>

<deferred>
## Deferred Ideas

- Website / GitHub Pages documentation (can be done post-release)
- Book-style documentation with mdBook (v1.1+)
- Binary releases via GitHub Actions (CI/CD pipeline, not part of this phase)
- Docker image (not a library concern)
</deferred>

---

*Phase: 08-v1.0-polish-release*
*Context gathered: 2026-05-25 via inline definition*

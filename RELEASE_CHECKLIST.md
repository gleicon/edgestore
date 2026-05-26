# EdgeStore v1.0.0 Release Checklist

## Pre-Release Verification

- [ ] All tests pass: `cargo test --all`
- [ ] Integration tests pass: `cargo test --test integration_v1`
- [ ] Benchmarks compile: `cargo bench --no-run`
- [ ] Documentation builds: `cargo doc --no-deps`
- [ ] No clippy warnings: `cargo clippy --all-targets --all-features`
- [ ] No `*` version dependencies in published crate `[dependencies]`
- [ ] All crates have version `1.0.0` and complete metadata
- [ ] `cargo metadata --format-version 1` parses successfully
- [ ] README.md is up to date with v1.0 features

## Tagging

1. Ensure the commit you want to tag is on the main branch.
2. Tag the release commit:
   ```bash
   git tag -a v1.0.0 -m "EdgeStore v1.0.0"
   ```
3. Push the tag to GitHub:
   ```bash
   git push origin v1.0.0
   ```

## Publishing to crates.io

**Important:** Publish crates in dependency order. `edgestore` must be published first because the other crates depend on it via path dependency.

1. Log in to crates.io:
   ```bash
   cargo login
   ```

2. Publish the core library:
   ```bash
   cd edgestore
   cargo publish
   ```

3. Wait for `edgestore` to appear on crates.io (may take a few minutes).

4. Publish the async wrapper:
   ```bash
   cd ../edgestore-tokio
   cargo publish
   ```

5. Publish the REPL / HTTP server:
   ```bash
   cd ../edgestore-repl
   cargo publish
   ```

6. Publish the CLI:
   ```bash
   cd ../edgestore-cli
   cargo publish
   ```

## GitHub Release

1. Go to https://github.com/gleicon/edgestore/releases/new
2. Choose tag `v1.0.0`
3. Title: "EdgeStore v1.0.0"
4. Write release notes covering:
   - Core KV engine with WAL and segments
   - Deathtime-cohort compaction with TTL support
   - Vector storage and search (flat SIMD + HNSW)
   - Full-text search with BM25 ranking
   - Pull-only replication with Merkle anti-entropy
   - Point-in-time snapshots
   - Multi-record transactions
   - Async Tokio wrapper, REPL, and CLI tools
5. Attach release binaries (optional):
   ```bash
   cargo build --release --bin edgestore-cli
   ```
6. Publish the release.

## Post-Release

- [ ] Verify all crates appear on crates.io with correct metadata
- [ ] Verify docs.rs builds successfully for `edgestore`
- [ ] Announce on relevant channels (if applicable)

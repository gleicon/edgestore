---
plan_id: "08-04b"
phase: "08"
wave: 3
depends_on: ["08-04a"]
requirements_addressed: ["POLISH-07", "POLISH-08"]
files_modified:
  - "BENCHMARKS.md"
autonomous: true
---

# Plan 08-04b: Publish Dry-Run, Benchmarks, and Final Validation

## Objective

Run crates.io publish dry-run, execute benchmark suite, and run the final validation sequence before v1.0 release.

**Purpose:** Ensure the release is ready for crates.io and all quality gates pass.

**Output:** Validated v1.0.0 release with benchmark results and clean validation.

## must_haves

- `cargo publish --dry-run` passes for all publishable crates
- Benchmarks run and results captured in BENCHMARKS.md
- Final validation sequence passes

## Tasks

<task>
  <id>08-04b-T1</id>
  <title>Run crates.io publish dry-run for all crates</title>
  <action>
    Run `cargo publish --dry-run -p edgestore` to validate the main library crate can be published. This checks: all files included in package, no path dependencies that can't be resolved, metadata is valid, dependencies exist on crates.io, build succeeds in clean environment. Fix any errors reported. Repeat for edgestore-tokio and edgestore-cli if they are to be published independently (they will be published after edgestore since they depend on it). Note: edgestore-repl may not be published if it's internal. Document any issues found and their resolutions. Verify that the package includes README.md, LICENSE files, and all source files.
  </action>
  <read_first>
    - Cargo.toml (for package metadata)
    - edgestore/Cargo.toml (for include/exclude patterns)
  </read_first>
  <acceptance_criteria>
    - criterion 1: `cargo publish --dry-run -p edgestore` exits with code 0
    - criterion 2: No unpublished path dependencies blocking publish
    - criterion 3: Package includes README.md and LICENSE files
    - criterion 4: All source files are included in the package
    - criterion 5: No warnings about missing metadata
    - criterion 6: Documentation builds successfully
  </acceptance_criteria>
</task>

<task>
  <id>08-04b-T2</id>
  <title>Execute benchmark suite and capture results</title>
  <action>
    Run the full benchmark suite: `cargo bench --workspace`. This will execute all benchmarks defined in edgestore/Cargo.toml: vector_search, hnsw_recall, throughput, text_search. Capture the output and update BENCHMARKS.md with actual results. For each benchmark, document: command used, hardware specs, results (throughput in ops/sec, latency in ms/μs, recall percentage for HNSW). If benchmarks require specific hardware (SSD WAF measurement), note that results are placeholder or run on available hardware. Ensure criterion.rs generates HTML reports in target/criterion/. Verify benchmarks compile and run without errors. If any benchmark is too slow for regular CI, mark it as "manual only" in BENCHMARKS.md.
  </action>
  <read_first>
    - BENCHMARKS.md (created in plan 08-02b)
    - edgestore/Cargo.toml (bench sections)
    - edgestore/benches/ (benchmark source files)
  </read_first>
  <acceptance_criteria>
    - criterion 1: `cargo bench --workspace` completes successfully
    - criterion 2: BENCHMARKS.md updated with actual results
    - criterion 3: HTML reports generated in target/criterion/
    - criterion 4: All benchmark binaries run without errors
    - criterion 5: Results include hardware/environment details
    - criterion 6: Slow benchmarks marked as "manual only" if needed
  </acceptance_criteria>
</task>

<task>
  <id>08-04b-T3</id>
  <title>Run final validation sequence</title>
  <action>
    Run the full validation sequence: 1) `cargo test --workspace` - all tests pass, 2) `cargo clippy --workspace -D warnings` - clean, 3) `cargo doc --workspace --no-deps` - no warnings, 4) `cargo build --release --workspace` - release builds succeed, 5) `cargo publish --dry-run -p edgestore` - passes. Verify the edgestore-cli binary can be installed and works: `cargo install --path edgestore-cli && edgestore-cli --version`. Document any failures and their resolutions.
  </action>
  <read_first>
    - All prior plan outputs
    - .planning/ROADMAP.md (success criteria for phase 8)
  </read_first>
  <acceptance_criteria>
    - criterion 1: All validation commands pass
    - criterion 2: `edgestore-cli --version` reports v1.0.0
    - criterion 3: Git tag v1.0.0 is ready to create
    - criterion 4: All Phase 8 success criteria met (per ROADMAP.md)
    - criterion 5: Ready for v1.0.0 release announcement
  </acceptance_criteria>
</task>

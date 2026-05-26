---
plan_id: "08-03a"
phase: "08"
wave: 2
depends_on: ["08-01"]
requirements_addressed: ["POLISH-04"]
files_modified:
  - "edgestore-cli/Cargo.toml"
  - "edgestore-cli/src/main.rs"
  - "Cargo.toml"
autonomous: true
---

# Plan 08-03a: CLI Scaffold and Core Subcommands

## Objective

Create the `edgestore-cli` crate structure and implement core database management and data operation subcommands.

**Purpose:** Administrative tool for managing EdgeStore databases without writing code.

**Output:** Working `edgestore-cli` binary crate handling create, stats, put, get, delete, and range.

## must_haves

- New crate `edgestore-cli` in workspace
- Subcommands: create, stats, put, get, delete, range
- Uses clap v4 with derive macro for clean argument definitions
- CLI depends on `edgestore` crate but adds no mandatory deps to library
- `cargo build -p edgestore-cli` succeeds

## Tasks

<task>
  <id>08-03a-T1</id>
  <title>Create edgestore-cli crate structure</title>
  <action>
    Create new crate directory `edgestore-cli/` in workspace root. Create edgestore-cli/Cargo.toml with: name = "edgestore-cli", version = "1.0.0", edition = "2021", license = "MIT OR Apache-2.0", dependencies including `clap = { version = "4", features = ["derive"] }` and `edgestore = { path = "../edgestore" }`. Add `[[bin]]` section with name = "edgestore-cli", path = "src/main.rs". Create src/main.rs with basic clap derive struct and main function that prints "EdgeStore CLI" and exits. Add "edgestore-cli" to workspace members in root Cargo.toml. Verify `cargo build -p edgestore-cli` compiles.
  </action>
  <read_first>
    - Cargo.toml (workspace root)
    - edgestore/Cargo.toml (for dependency reference)
    - .planning/phases/08/08-CONTEXT.md (CLI requirements)
  </read_first>
  <acceptance_criteria>
    - criterion 1: edgestore-cli/Cargo.toml exists with proper metadata
    - criterion 2: edgestore-cli/src/main.rs exists with basic structure
    - criterion 3: Workspace root Cargo.toml lists edgestore-cli in members
    - criterion 4: `cargo build -p edgestore-cli` succeeds
    - criterion 5: Binary outputs "EdgeStore CLI" when run
    - criterion 6: `edgestore-cli --help` shows help text
  </acceptance_criteria>
</task>

<task>
  <id>08-03a-T2</id>
  <title>Implement database management subcommands (create, stats)</title>
  <action>
    Add clap derive structs for `Create` and `Stats` subcommands. Create command: takes `--path <PATH>` required argument, optional `--namespace <NS>` for default namespace, creates Engine at path, prints "Created database at {path}". Stats command: takes `--path <PATH>`, opens Engine, collects statistics: segment count, memtable entry count, WAL file count, total size on disk, cohort distribution if available, prints formatted table or JSON (add `--json` flag). Use clap subcommand derive pattern with enum Cli { Create(Create), Stats(Stats), ... }. Implement match arms in main() to dispatch to handler functions. Handler functions return Result<(), Box<dyn Error>> for proper error propagation.
  </action>
  <read_first>
    - edgestore/src/lib.rs (for Engine public API)
    - edgestore/src/engine.rs (for Engine::open, Engine::stats methods)
  </read_first>
  <acceptance_criteria>
    - criterion 1: `edgestore-cli create --path /tmp/testdb` creates database directory
    - criterion 2: `edgestore-cli stats --path /tmp/testdb` shows segment/memtable/WAL counts
    - criterion 3: `edgestore-cli stats --path /tmp/testdb --json` outputs valid JSON
    - criterion 4: Both commands have --help showing all options
    - criterion 5: Error messages are user-friendly on invalid path
    - criterion 6: Stats reflect actual database state after put operations
  </acceptance_criteria>
</task>

<task>
  <id>08-03a-T3</id>
  <title>Implement data operations subcommands (put, get, delete, range)</title>
  <action>
    Implement Put, Get, Delete, Range subcommands. Put: --path, --namespace, --key, --value, optional --ttl-seconds. Opens Engine, calls engine.put() or engine.put_with_ttl(), prints confirmation. Get: --path, --namespace, --key. Opens Engine, calls engine.get(), prints value (hex if binary) or "Key not found". Delete: --path, --namespace, --key. Opens Engine, calls engine.delete(), prints confirmation. Range: --path, --namespace, --start, --end, optional --limit. Opens Engine, calls engine.range(), prints key=value pairs one per line, respects limit. All commands support --namespace with default "default". Handle binary data safely: accept hex-encoded input with --hex flag, output hex for non-UTF8 values. Return exit code 0 on success, 1 on error/not found.
  </action>
  <read_first>
    - edgestore/src/lib.rs (Engine API: put, get, delete, range)
    - edgestore/src/engine.rs (for method signatures)
  </read_first>
  <acceptance_criteria>
    - criterion 1: `put --path /tmp/db --key foo --value bar` stores key
    - criterion 2: `get --path /tmp/db --key foo` returns "bar"
    - criterion 3: `get --path /tmp/db --key missing` returns exit code 1
    - criterion 4: `delete --path /tmp/db --key foo` removes key
    - criterion 5: `range --path /tmp/db --start a --end z` lists keys in range
    - criterion 6: `--hex` flag works for binary data round-trip
  </acceptance_criteria>
</task>

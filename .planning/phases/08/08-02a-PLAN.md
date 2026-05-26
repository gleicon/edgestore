---
plan_id: "08-02a"
phase: "08"
wave: 1
depends_on: []
requirements_addressed: ["POLISH-03"]
files_modified:
  - "README.md"
  - "ARCHITECTURE.md"
  - "CHANGELOG.md"
autonomous: true
---

# Plan 08-02a: Core Documentation

## Objective

Create the three core documentation files: README with quick-start and feature matrix, ARCHITECTURE.md with component overview, and CHANGELOG.md summarizing all v1.0 features.

**Purpose:** Users can understand, install, and use EdgeStore from documentation alone.

**Output:** README.md, ARCHITECTURE.md, CHANGELOG.md at repo root.

## must_haves

- README.md with quick-start, feature matrix, architecture overview
- ARCHITECTURE.md with component diagram and data flow
- CHANGELOG.md following Keep a Changelog format

## Tasks

<task>
  <id>08-02a-T1</id>
  <title>Create README.md with quick-start and feature matrix</title>
  <action>
    Create README.md at repo root with sections: 1) Badges (CI, crates.io, docs.rs), 2) One-line description: "Local-first embedded KV + vector database in Rust", 3) Quick start (10 lines max): open DB, put, get, close with code example, 4) Feature matrix showing which features work (KV, TTL, snapshots, vector search, text search, replication, S3, HNSW, SSD optimization), 5) Installation section with Cargo.toml snippet, 6) Architecture overview with ASCII diagram or link to ARCHITECTURE.md, 7) Links to docs.rs, repo, issues. Use markdown code blocks for all examples. Style: concise, professional, welcoming.
  </action>
  <read_first>
    - AGENTS.md (for project description)
    - .planning/PROJECT.md (for feature list)
    - .planning/ROADMAP.md (for completed phases)
  </read_first>
  <acceptance_criteria>
    - criterion 1: README.md exists at repo root
    - criterion 2: Quick-start example is 10 lines or fewer
    - criterion 3: Feature matrix shows all 8 phases' features
    - criterion 4: Installation shows correct cargo add command
    - criterion 5: Contains links to docs.rs API documentation
    - criterion 6: ASCII architecture diagram or link to ARCHITECTURE.md present
  </acceptance_criteria>
</task>

<task>
  <id>08-02a-T2</id>
  <title>Create ARCHITECTURE.md with component overview</title>
  <action>
    Create ARCHITECTURE.md with: 1) Overview paragraph describing architecture philosophy (local-first, append-only, SSD-aware), 2) Component diagram (ASCII art) showing: App → Transaction → Memtable → SegmentStore → SSD, with WAL on the side, 3) Component descriptions: Engine (coordination), WAL (durability), Memtable (in-memory BTreeMap), SegmentStore (immutable SSTables with xor filters), Compactor (deathtime-cohort GC), VectorIndex (SIMD/HNSW), TextIndex (BM25 inverted index), 4) Data flow section: write path (put → WAL → memtable → flush → segment), read path (get → memtable → segments → xor filter), compaction path, 5) File formats section: WAL format, segment format (.dat, .idx, .xf, .meta), manifest format, 6) Namespace encoding diagram, 7) References to prod.md for deep technical details.
  </action>
  <read_first>
    - prod.md (for technical architecture details)
    - AGENTS.md (for key design decisions)
    - edgestore/src/lib.rs (for module structure)
  </read_first>
  <acceptance_criteria>
    - criterion 1: ARCHITECTURE.md exists at repo root
    - criterion 2: Contains ASCII or text-based component diagram
    - criterion 3: All major components documented with responsibilities
    - criterion 4: Write/read/compaction data flows described
    - criterion 5: File format summary included
    - criterion 6: Links to prod.md for full specification
  </acceptance_criteria>
</task>

<task>
  <id>08-02a-T3</id>
  <title>Create CHANGELOG.md following Keep a Changelog format</title>
  <action>
    Create CHANGELOG.md following https://keepachangelog.com/ format with sections: 1) Header explaining the format, 2) [Unreleased] section with placeholder, 3) [1.0.0] - 2026-05-25 with sections: Added (all features from phases 1-7), Changed (nothing for v1.0), Deprecated (nothing), Removed (nothing), Fixed (nothing), Security (nothing). For Added section, list major features grouped by phase: Phase 1 (WAL, memtable, transactions), Phase 2 (segments, xor filters), Phase 3 (deathtime-cohort compaction, snapshots), Phase 4 (replication, S3), Phase 5 (vector search), Phase 6 (HNSW, tokio wrapper), Phase 7 (full-text search). Include git commit range placeholder for v1.0.
  </action>
  <read_first>
    - .planning/ROADMAP.md (for phase summaries)
  </read_first>
  <acceptance_criteria>
    - criterion 1: CHANGELOG.md exists at repo root
    - criterion 2: Follows Keep a Changelog format
    - criterion 3: [1.0.0] section includes all phase features
    - criterion 4: Date in ISO format (YYYY-MM-DD)
    - criterion 5: [Unreleased] section present for future work
    - criterion 6: No placeholder text remaining
  </acceptance_criteria>
</task>

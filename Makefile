# EdgeStore Release Makefile
# Usage: make <target>

.PHONY: all help clean test build clippy doc lint
.PHONY: tag tags-push publish publish-dryrun release

# ── Configuration ─────────────────────────────────────────────────────────

SHELL := /bin/bash

# Read version from workspace Cargo.toml
VERSION := $(shell grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)
GIT_TAG := v$(VERSION)

# Crates in dependency order (publish from root to leaves)
CRATES := edgestore edgestore-tokio edgestore-repl edgestore-cli

# ── Default ─────────────────────────────────────────────────────────────────

all: test build

help:
	@echo "EdgeStore $(VERSION) — Release workflow"
	@echo ""
	@echo "  make clean          — Remove build artifacts"
	@echo "  make test           — Run all workspace tests"
	@echo "  make build          — Build all workspace members"
	@echo "  make clippy         — Run clippy with warnings-as-errors"
	@echo "  make lint           — Run clippy + fmt check"
	@echo "  make doc            — Build and open docs"
	@echo "  make tag            — Create git tag $(GIT_TAG)"
	@echo "  make tags-push      — Push tag to origin"
	@echo "  make publish-dryrun — Verify all crates are publishable"
	@echo "  make publish        — Publish all crates to crates.io (in order)"
	@echo "  make release        — Full workflow: test → tag → publish"
	@echo ""

# ── Development ─────────────────────────────────────────────────────────────

clean:
	cargo clean

test:
	cargo test --workspace

build:
	cargo build --workspace

clippy:
	cargo clippy --workspace -- -D warnings

fmt-check:
	cargo fmt -- --check

lint: clippy fmt-check

doc:
	cargo doc --workspace --no-deps

bench:
	cargo bench --workspace

# ── Release ─────────────────────────────────────────────────────────────────

tag:
	@if git rev-parse $(GIT_TAG) >/dev/null 2>&1; then \
		echo "Tag $(GIT_TAG) already exists"; \
	else \
		echo "Creating tag $(GIT_TAG)..."; \
		git tag -a $(GIT_TAG) -m "EdgeStore $(VERSION)"; \
		echo "Tag $(GIT_TAG) created. Run 'make tags-push' to push."; \
	fi

tags-push:
	@echo "Pushing $(GIT_TAG) to origin..."
	git push origin $(GIT_TAG)

publish-dryrun:
	@echo "Dry-run publish for EdgeStore $(VERSION)"
	@for crate in $(CRATES); do \
		echo "  → Checking $$crate..."; \
		cargo publish -p $$crate --dry-run || exit 1; \
	done
	@echo "All crates ready for publish."

publish:
	@echo "Publishing EdgeStore $(VERSION) to crates.io"
	@echo "Order: $(CRATES)"
	@for crate in $(CRATES); do \
		echo ""; \
		echo "Publishing $$crate..."; \
		cargo publish -p $$crate; \
	done
	@echo ""
	@echo "All crates published. Verify at:"
	@echo "  https://crates.io/crates/edgestore/$(VERSION)"

release: test tag publish
	@echo ""
	@echo "EdgeStore $(VERSION) released!"
	@echo "Tag: $(GIT_TAG)"
	@echo "Crates: $(CRATES)"

# EdgeStore Release Makefile
# Usage: make <target>

.PHONY: all help clean test build clippy doc lint
.PHONY: tag tag-force tags-push publish publish-dryrun release
.PHONY: bump-patch bump-minor bump-major
.PHONY: s3-test s3-up s3-down

# ── Configuration ─────────────────────────────────────────────────────────

SHELL := /bin/bash

# VERSION: env var overrides Cargo.toml. E.g. VERSION=1.0.3 make tag
VERSION ?= $(shell grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)
GIT_TAG := v$(VERSION)

# Crates in dependency order (publish from root to leaves)
CRATES := edgestore edgestore-tokio edgestore-repl edgestore-cli

# ── Docker detection ──────────────────────────────────────────────────────
# Some environments (e.g. Colima) run a newer Docker daemon than the host
# Docker client supports. We detect this and fall back to `colima ssh -- docker`
# when the local client is too old.
DOCKER_OK := $(shell docker ps >/dev/null 2>&1 && echo 1 || echo 0)
ifeq ($(DOCKER_OK),1)
  DOCKER := docker
else
  DOCKER := colima ssh -- docker
endif

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
	@echo ""
	@echo "  make tag            — Create git tag $(GIT_TAG)"
	@echo "  make tag-force      — Delete + recreate tag $(GIT_TAG) if it exists"
	@echo "  VERSION=1.0.3 make tag  — Override version from env var"
	@echo ""
	@echo "  make bump-patch     — Bump patch, update all files, commit"
	@echo "  make bump-minor     — Bump minor, update all files, commit"
	@echo "  make bump-major     — Bump major, update all files, commit"
	@echo ""
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

# ── S3 / LocalStack integration tests ─────────────────────────────────────

s3-up: ## Start LocalStack container for S3 testing
	@echo "Starting LocalStack..."
	@echo "Using docker command: $(DOCKER)"
	@$(DOCKER) stop edgestore-localstack >/dev/null 2>&1 || true
	@$(DOCKER) rm edgestore-localstack >/dev/null 2>&1 || true
	$(DOCKER) run -d \
		--name edgestore-localstack \
		-p 4566:4566 \
		-e SERVICES=s3 \
		-e AWS_ACCESS_KEY_ID=test \
		-e AWS_SECRET_ACCESS_KEY=test \
		-e AWS_DEFAULT_REGION=us-east-1 \
		-v "$(PWD)/scripts/localstack-init.sh:/etc/localstack/init/ready.d/init-aws.sh:ro" \
		localstack/localstack:3
	@echo "Waiting for LocalStack to be ready..."
	@sleep 3
	@until curl -sf http://localhost:4566/_localstack/health >/dev/null 2>&1; do \
		echo "  ...waiting"; sleep 2; \
	done
	@echo "LocalStack ready."

s3-down: ## Stop and remove LocalStack container
	@echo "Stopping LocalStack..."
	$(DOCKER) stop edgestore-localstack >/dev/null 2>&1 || true
	$(DOCKER) rm edgestore-localstack >/dev/null 2>&1 || true

s3-test: s3-up ## Run S3 integration tests against LocalStack
	@echo "Running S3 integration tests..."
	EDGESTORE_S3_ENDPOINT_URL=http://localhost:4566 \
	EDGESTORE_S3_BUCKET=edgestore-test \
	AWS_ACCESS_KEY_ID=test \
	AWS_SECRET_ACCESS_KEY=test \
	AWS_DEFAULT_REGION=us-east-1 \
	cargo test --package edgestore-repl --features s3 -- --nocapture
	$(MAKE) s3-down

# ── Version bump helpers ──────────────────────────────────────────────────

# Read current version parts
VERSION_MAJOR := $(shell echo $(VERSION) | cut -d. -f1)
VERSION_MINOR := $(shell echo $(VERSION) | cut -d. -f2)
VERSION_PATCH := $(shell echo $(VERSION) | cut -d. -f3)

NEW_PATCH := $(VERSION_MAJOR).$(VERSION_MINOR).$(shell echo $$(($(VERSION_PATCH)+1)))
NEW_MINOR := $(VERSION_MAJOR).$(shell echo $$(($(VERSION_MINOR)+1))).0
NEW_MAJOR := $(shell echo $$(($(VERSION_MAJOR)+1))).0.0

# Helper: update all version references across the repo
# Usage: $(call update_all_versions,OLD,NEW)
define update_all_versions
	@echo "Bumping $(1) → $(2) across all files..."
	@# Workspace Cargo.toml
	@sed -i '' "s/^version = \"$(1)\"/version = \"$(2)\"/" Cargo.toml
	@# Downstream crate Cargo.toml: edgestore dep version pin
	@for f in edgestore-cli/Cargo.toml edgestore-repl/Cargo.toml edgestore-tokio/Cargo.toml; do \
		OLD_DEP=$$(grep -o 'edgestore = { path = "../edgestore", version = "[^"]*"' $$f | grep -o '"[^"]*"$$' | tr -d '"'); \
		if [ -n "$$OLD_DEP" ]; then \
			sed -i '' "s|edgestore = { path = \"../edgestore\", version = \"$$OLD_DEP\"|edgestore = { path = \"../edgestore\", version = \"$(2)\"|" $$f; \
		fi; \
	done
	@# CLI binary version
	@sed -i '' 's/#\[command(version = "[^"]*")/#\[command(version = "$(2)")/' edgestore-cli/src/main.rs
	@# Website hero badge
	@sed -i '' "s/v$(1) Released/v$(2) Released/" website/index.html
	@# Website release section badge
	@sed -i '' "s/>v$(1)</>v$(2)</" website/index.html
	@# AGENTS.md status line
	@sed -i '' "s/v$(1)/v$(2)/" AGENTS.md
	@# Cargo.lock
	@cargo update -w >/dev/null 2>&1
	@echo "Done. Files touched: Cargo.toml, Cargo.lock, edgestore-cli/Cargo.toml,"
	@echo "  edgestore-repl/Cargo.toml, edgestore-tokio/Cargo.toml,"
	@echo "  edgestore-cli/src/main.rs, website/index.html, AGENTS.md"
endef

define add_changelog_section
	@head -n 9 CHANGELOG.md > CHANGELOG.md.new
	@echo "## [$(1)] - $$(date +%Y-%m-%d)" >> CHANGELOG.md.new
	@echo "" >> CHANGELOG.md.new
	@echo "### Added" >> CHANGELOG.md.new
	@echo "" >> CHANGELOG.md.new
	@echo "### Changed" >> CHANGELOG.md.new
	@echo "" >> CHANGELOG.md.new
	@echo "### Deprecated" >> CHANGELOG.md.new
	@echo "" >> CHANGELOG.md.new
	@echo "### Removed" >> CHANGELOG.md.new
	@echo "" >> CHANGELOG.md.new
	@echo "### Fixed" >> CHANGELOG.md.new
	@echo "" >> CHANGELOG.md.new
	@echo "### Security" >> CHANGELOG.md.new
	@echo "" >> CHANGELOG.md.new
	@tail -n +10 CHANGELOG.md >> CHANGELOG.md.new
	@mv CHANGELOG.md.new CHANGELOG.md
	@echo "CHANGELOG.md updated with [$(1)] section."
endef

bump-patch:
	$(call update_all_versions,$(VERSION),$(NEW_PATCH))
	$(call add_changelog_section,$(NEW_PATCH))
	@git add Cargo.toml Cargo.lock \
		edgestore-cli/Cargo.toml edgestore-repl/Cargo.toml edgestore-tokio/Cargo.toml \
		edgestore-cli/src/main.rs website/index.html AGENTS.md CHANGELOG.md
	@git diff --cached --quiet || git commit -m "chore: bump version to $(NEW_PATCH)"
	@echo ""
	@echo "Version bumped to $(NEW_PATCH) and committed."
	@echo "Run 'make tag' to create tag v$(NEW_PATCH)"

bump-minor:
	$(call update_all_versions,$(VERSION),$(NEW_MINOR))
	$(call add_changelog_section,$(NEW_MINOR))
	@git add Cargo.toml Cargo.lock \
		edgestore-cli/Cargo.toml edgestore-repl/Cargo.toml edgestore-tokio/Cargo.toml \
		edgestore-cli/src/main.rs website/index.html AGENTS.md CHANGELOG.md
	@git diff --cached --quiet || git commit -m "chore: bump version to $(NEW_MINOR)"
	@echo ""
	@echo "Version bumped to $(NEW_MINOR) and committed."
	@echo "Run 'make tag' to create tag v$(NEW_MINOR)"

bump-major:
	$(call update_all_versions,$(VERSION),$(NEW_MAJOR))
	$(call add_changelog_section,$(NEW_MAJOR))
	@git add Cargo.toml Cargo.lock \
		edgestore-cli/Cargo.toml edgestore-repl/Cargo.toml edgestore-tokio/Cargo.toml \
		edgestore-cli/src/main.rs website/index.html AGENTS.md CHANGELOG.md
	@git diff --cached --quiet || git commit -m "chore: bump version to $(NEW_MAJOR)"
	@echo ""
	@echo "Version bumped to $(NEW_MAJOR) and committed."
	@echo "Run 'make tag' to create tag v$(NEW_MAJOR)"

# ── Release ─────────────────────────────────────────────────────────────────

tag:
	@if git rev-parse $(GIT_TAG) >/dev/null 2>&1; then \
		echo "Tag $(GIT_TAG) already exists."; \
		echo "  Run 'make tag-force' to delete and recreate it,"; \
		echo "  or 'make bump-patch' (or minor/major) to increment version first."; \
		exit 1; \
	else \
		echo "Creating tag $(GIT_TAG)..."; \
		git tag -a $(GIT_TAG) -m "EdgeStore $(VERSION)"; \
		echo "Tag $(GIT_TAG) created. Run 'make tags-push' to push."; \
	fi

tag-force:
	@echo "Force-recreating tag $(GIT_TAG)..."
	@git tag -d $(GIT_TAG) 2>/dev/null || true
	@git tag -a $(GIT_TAG) -m "EdgeStore $(VERSION)"
	@echo "Tag $(GIT_TAG) recreated. Run 'make tags-push' to push."

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

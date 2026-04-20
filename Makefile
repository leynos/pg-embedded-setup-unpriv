.PHONY: help all clean test build release release-archive lint fmt check-fmt markdownlint nixie typecheck

APP ?= pg_embedded_setup_unpriv
CARGO ?= cargo
BUILD_JOBS ?=
DIST_DIR ?= dist
RELEASE_BINARIES ?= pg_embedded_setup_unpriv pg_worker
TARGET ?=
MANIFEST_VERSION := $(strip $(shell awk '\
	/^\[package\]$$/ { in_package = 1; next } \
	/^\[/ { if (in_package) exit; next } \
	in_package && /^version[[:space:]]*=/ { \
		if (match($$0, /"([^"]+)"/)) { \
			print substr($$0, RSTART + 1, RLENGTH - 2); \
			exit; \
		} \
	}' Cargo.toml))
VERSION ?= $(MANIFEST_VERSION)
ifeq ($(strip $(VERSION)),)
$(error VERSION is empty; set [package].version in Cargo.toml or pass VERSION explicitly)
endif
RELEASE_ARCHIVE_STEM = pg-embed-setup-unpriv-$(TARGET)-v$(VERSION)
RELEASE_ARCHIVE_DIR = $(DIST_DIR)/$(RELEASE_ARCHIVE_STEM)
RELEASE_ARCHIVE_FILE = $(RELEASE_ARCHIVE_DIR).tgz
CLIPPY_FLAGS ?= --all-targets --all-features -- -D warnings
RUSTDOC_FLAGS ?= --cfg docsrs -D warnings
MDLINT ?= markdownlint-cli2
NIXIE ?= nixie

build: ## Build debug binary
	$(CARGO) build $(BUILD_JOBS) --bin "$(APP)"

release: ## Build release binaries
	$(CARGO) build $(BUILD_JOBS) --release $(foreach bin,$(RELEASE_BINARIES),--bin $(bin))

all: check-fmt lint test ## Perform all commit gate checks

clean: ## Remove build artifacts
	$(CARGO) clean

test: ## Run tests with warnings treated as errors
	RUSTFLAGS="-D warnings" $(CARGO) nextest run --all-targets --all-features $(BUILD_JOBS)
	RUSTFLAGS="-D warnings" $(CARGO) nextest run --tests --workspace --no-default-features --features dev-worker $(BUILD_JOBS)

release-archive: ## Package release binaries for cargo-binstall
	@test -n "$(TARGET)" || (echo "TARGET is required" >&2; exit 1)
	@test "$(MANIFEST_VERSION)" = "$(VERSION)" || \
		(echo "VERSION ($(VERSION)) must match Cargo.toml package version ($(MANIFEST_VERSION))" >&2; exit 1)
	$(CARGO) build $(BUILD_JOBS) --release --target "$(TARGET)" $(foreach bin,$(RELEASE_BINARIES),--bin $(bin))
	rm -rf "$(RELEASE_ARCHIVE_DIR)" "$(RELEASE_ARCHIVE_FILE)"
	mkdir -p "$(RELEASE_ARCHIVE_DIR)"
	@for bin in $(RELEASE_BINARIES); do \
		cp "target/$(TARGET)/release/$$bin" "$(RELEASE_ARCHIVE_DIR)/$$bin"; \
	done
	tar -C "$(DIST_DIR)" -czf "$(RELEASE_ARCHIVE_FILE)" "$(RELEASE_ARCHIVE_STEM)"
	rm -rf "$(RELEASE_ARCHIVE_DIR)"

lint: ## Run Clippy with warnings denied
	RUSTDOCFLAGS="$(RUSTDOC_FLAGS)" $(CARGO) doc --workspace --no-deps $(BUILD_JOBS)
	$(CARGO) clippy $(CLIPPY_FLAGS)

typecheck: ## Typecheck the workspace
	$(CARGO) check --workspace --all-targets --all-features $(BUILD_JOBS)

fmt: ## Format Rust and Markdown sources
	$(CARGO) fmt --all
	mdformat-all

check-fmt: ## Verify formatting
	$(CARGO) fmt --all -- --check

markdownlint: ## Lint Markdown files
	$(MDLINT) "**/*.md"

nixie: ## Validate Mermaid diagrams
	nixie --no-sandbox

help: ## Show available targets
	@grep -E '^[a-zA-Z_-]+:.*?##' $(MAKEFILE_LIST) | \
	awk 'BEGIN {FS=":"; printf "Available targets:\n"} {printf "  %-20s %s\n", $$1, $$2}'

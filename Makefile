.PHONY: help all clean test build release release-archive lint fmt check-fmt markdownlint nixie typecheck

APP ?= pg_embedded_setup_unpriv
CARGO ?= cargo
BUILD_JOBS ?=
DIST_DIR ?= dist
RELEASE_BINARIES ?= pg_embedded_setup_unpriv pg_worker
TARGET ?=
VERSION ?= $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
CLIPPY_FLAGS ?= --all-targets --all-features -- -D warnings
RUSTDOC_FLAGS ?= --cfg docsrs -D warnings
MDLINT ?= markdownlint-cli2
NIXIE ?= nixie

build: target/debug/$(APP) ## Build debug binary
release: $(addprefix target/release/,$(RELEASE_BINARIES)) ## Build release binaries

all: check-fmt lint test ## Perform all commit gate checks

clean: ## Remove build artifacts
	$(CARGO) clean

test: ## Run tests with warnings treated as errors
	RUSTFLAGS="-D warnings" $(CARGO) nextest run --all-targets --all-features $(BUILD_JOBS)
	RUSTFLAGS="-D warnings" $(CARGO) nextest run --tests --workspace --no-default-features --features dev-worker $(BUILD_JOBS)

target/%/pg_embedded_setup_unpriv target/%/pg_worker: ## Build binary in debug or release mode
	$(CARGO) build $(BUILD_JOBS) $(if $(findstring release,$(@)),--release) --bin $(@F)

release-archive: ## Package release binaries for cargo-binstall
	@test -n "$(TARGET)" || (echo "TARGET is required" >&2; exit 1)
	@manifest_version="$$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)"; \
		test "$$manifest_version" = "$(VERSION)" || \
		(echo "VERSION ($(VERSION)) must match Cargo.toml package version ($$manifest_version)" >&2; exit 1)
	$(CARGO) build $(BUILD_JOBS) --release --target "$(TARGET)" \
		--bin pg_embedded_setup_unpriv \
		--bin pg_worker
	rm -rf "$(DIST_DIR)/pg-embed-setup-unpriv-$(TARGET)-v$(VERSION)" \
		"$(DIST_DIR)/pg-embed-setup-unpriv-$(TARGET)-v$(VERSION).tgz"
	mkdir -p "$(DIST_DIR)/pg-embed-setup-unpriv-$(TARGET)-v$(VERSION)"
	cp "target/$(TARGET)/release/pg_embedded_setup_unpriv" \
		"$(DIST_DIR)/pg-embed-setup-unpriv-$(TARGET)-v$(VERSION)/pg_embedded_setup_unpriv"
	cp "target/$(TARGET)/release/pg_worker" \
		"$(DIST_DIR)/pg-embed-setup-unpriv-$(TARGET)-v$(VERSION)/pg_worker"
	tar -C "$(DIST_DIR)" \
		-czf "$(DIST_DIR)/pg-embed-setup-unpriv-$(TARGET)-v$(VERSION).tgz" \
		"pg-embed-setup-unpriv-$(TARGET)-v$(VERSION)"
	rm -rf "$(DIST_DIR)/pg-embed-setup-unpriv-$(TARGET)-v$(VERSION)"

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

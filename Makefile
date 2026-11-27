# Shortcuts for the commands in ci/README.md. `make check` is what CI runs; `make` lists the targets.
# segment-log's Makefile has the same shape and shared targets; keep the two alike.
.DEFAULT_GOAL := help
KILLS ?= 10000
.PHONY: help check fmt lint test doc clean release release-dry-run embed soak deny public-api bless install-tools

help: ## Show the available targets and how to use them
	@echo 'Usage: make [target] [VERSION=x.y.z] [KILLS=n]'
	@echo
	@awk 'BEGIN {FS = ":.*## "} /^[a-zA-Z_-]+:.*## / {printf "  %-16s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

check: ## Run every CI job locally (ci/check.sh); run it before you push
	ci/check.sh

fmt: ## Format all crates in place, the embed example too
	cargo fmt --all
	cd examples/embed && cargo fmt

lint: ## Check formatting and run clippy with warnings as errors
	cargo fmt --all --check
	cargo clippy --workspace --all-targets --locked -- -D warnings

test: ## Run the workspace tests, with CI's crash-test budget
	env -u CRASH_SEED CRASH_KILLS=100 cargo test --workspace --locked

doc: ## Build the API docs and open them
	cargo doc --workspace --no-deps --open

clean: ## Remove build output, the embed example's too
	cargo clean
	cd examples/embed && cargo clean

release: ## Publish the crates to crates.io and push the tag (no archives): make release VERSION=0.1.0
	ci/release.sh $(VERSION)

release-dry-run: ## Verify the crates a release would publish, publishing nothing: make release-dry-run VERSION=0.1.0
	ci/release.sh $(VERSION) --dry-run

embed: ## Run examples/embed against this checkout
	cd examples/embed && cargo run --locked

soak: ## Crash-test soak on this machine: make soak KILLS=10000
	env -u CRASH_SEED CRASH_KILLS=$(KILLS) cargo test -p ironflowx-crash-test --locked --test crash seeded_random_kills

deny: ## Check licences and advisories (cargo-deny)
	cargo deny check

public-api: ## Compare the public API with the snapshots (commit first)
	ci/public-api.sh

bless: ## Rewrite the public-API snapshots after a reviewed change
	ci/public-api.sh --bless

install-tools: ## Install the pinned nightly, cargo-public-api and cargo-deny
	ci/install-tools.sh

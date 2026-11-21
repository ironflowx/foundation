# Shortcuts for the commands in ci/README.md. `make` alone lists them.
.DEFAULT_GOAL := help
SHELL := bash

VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
KILLS ?= 10000

.PHONY: help check fmt fmt-check lint test embed soak deny public-api bless doc install-tools publish-dry-run publish tag clean

help: ## List the targets
	@awk 'BEGIN {FS = ":.*## "} /^[a-z-]+:.*## / {printf "  \033[36m%-16s\033[0m %s\n", $$1, $$2}' $(MAKEFILE_LIST)

check: ## Run every CI job locally (ci/check.sh); run before you push
	ci/check.sh

fmt: ## Format the workspace and the embed example
	cargo fmt --all
	cd examples/embed && cargo fmt

fmt-check: ## Check formatting
	cargo fmt --all --check

lint: ## Clippy with warnings denied, as CI runs it
	cargo clippy --workspace --all-targets --locked -- -D warnings

test: ## Run the tests, with CI's crash-test budget
	env -u CRASH_SEED CRASH_KILLS=100 cargo test --workspace --locked

embed: ## Run examples/embed against this checkout
	cd examples/embed && cargo run --locked

soak: ## Crash-test soak on this machine: KILLS seeded kills (default 10000)
	env -u CRASH_SEED CRASH_KILLS=$(KILLS) cargo test -p ironflowx-crash-test --locked --test crash seeded_random_kills

deny: ## Licences and advisories (cargo-deny)
	cargo deny check

public-api: ## Compare the public API with the snapshots (commit first)
	ci/public-api.sh

bless: ## Rewrite the public-API snapshots after a reviewed change
	ci/public-api.sh --bless

doc: ## Build the API docs and open them
	cargo doc --workspace --no-deps --open

install-tools: ## Install the pinned nightly, cargo-public-api and cargo-deny
	ci/install-tools.sh

publish-dry-run: ## Package and verify every published crate; uploads nothing
	cargo publish --workspace --dry-run --locked

# Publishing cannot be undone: only from a clean main that matches origin, after a dry run.
publish: publish-dry-run ## Publish every crate at the workspace version to crates.io
	@[ "$$(git branch --show-current)" = main ] || { echo "publish from main"; exit 1; }
	@[ -z "$$(git status --porcelain)" ] || { echo "the tree is not clean"; exit 1; }
	@git fetch -q origin main && [ "$$(git rev-parse HEAD)" = "$$(git rev-parse origin/main)" ] || { echo "main does not match origin/main"; exit 1; }
	@read -r -p "Publish $(VERSION) to crates.io? Type the version to confirm: " v && [ "$$v" = "$(VERSION)" ] || { echo "not published"; exit 1; }
	cargo publish --workspace --locked

tag: ## Tag the workspace version on HEAD and push the tag
	git tag -a v$(VERSION) -m "$(VERSION)"
	git push origin v$(VERSION)

clean: ## Remove build output, the embed example's too
	cargo clean
	cd examples/embed && cargo clean

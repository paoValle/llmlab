# `cargo` is the native tool and that is enough: on Windows `make` often does not exist,
# on GitHub Actions on Linux it does. The Makefile is here for whoever prefers it.
.DEFAULT_GOAL := help
.PHONY: help setup lab agent test lint fmt fmt-check ci doc

help: ## show this help
	@grep -E '^[a-z-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN{FS=":.*?## "}{printf "  \033[36m%-10s\033[0m %s\n", $$1, $$2}'

setup: ## fetch dependencies
	cargo fetch

lab: ## run every scenario and write reports/latest.{md,json}
	cargo run --release --quiet

agent: ## only the vertical: a real agent, over a socket, through the gateway
	@echo "scenario 7 needs a sibling agentloop checkout and npm ci; it is part of \`make lab\`"

test: ## assert the invariants again
	cargo test --all-targets

lint: ## clippy, warnings are errors
	cargo clippy --all-targets --all-features -- -D warnings

fmt: ## writes the files
	cargo fmt

fmt-check: ## checks formatting without writing
	cargo fmt --check

doc: ## opens the documentation
	cargo doc --open

ci: fmt-check lint test ## exactly what runs in CI

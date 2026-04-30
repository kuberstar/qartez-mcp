CARGO       := $(shell command -v cargo 2>/dev/null || echo "$(HOME)/.cargo/bin/cargo")
INSTALL_DIR  = $(HOME)/.local/bin
TARGET_DIR   = $(or $(CARGO_TARGET_DIR),$(CURDIR)/target)
BENCH_RUN    = $(CARGO) run --quiet --release --features benchmark --bin benchmark --
BENCH_LANGS := rust typescript python go java

.PHONY: help test test-install build install deploy setup uninstall clean \
        deploy-windows install-windows setup-windows \
        bench bench-all bench-fixtures \
        ci ci-fmt ci-clippy ci-test ci-build ci-deny ci-doc

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-20s\033[0m %s\n", $$1, $$2}'

deploy: ## Full deploy: install deps, build, install, configure IDEs
	@./install.sh

deploy-windows: ## Full deploy on Windows PowerShell
	@powershell -ExecutionPolicy Bypass -File .\install.ps1

test: ## Run all tests
	$(CARGO) test

test-install: ## Test install.sh portability (no build needed)
	@./tests/test-install.sh

build: ## Build release binaries
	@command -v cargo >/dev/null 2>&1 || [ -x $(HOME)/.cargo/bin/cargo ] || { \
		echo "\033[1;31m[!]\033[0m Rust not found. Run './install.sh' or 'make deploy' for first-time setup."; \
		exit 1; \
	}
	$(CARGO) build --release

install: build ## Build and install binaries to ~/.local/bin
	@mkdir -p $(INSTALL_DIR)
	@for bin in qartez qartez-guard qartez-setup; do \
		cp $(TARGET_DIR)/release/$$bin $(INSTALL_DIR)/$$bin; \
		if [ "$$(uname)" = "Darwin" ]; then \
			codesign -s - -f $(INSTALL_DIR)/$$bin 2>/dev/null || true; \
		fi; \
		echo "\033[0;32m[+]\033[0m Binary: $(INSTALL_DIR)/$$bin ($$(wc -c < $(TARGET_DIR)/release/$$bin | awk '{printf "%.1f MB", $$1/1048576}'))"; \
	done
	@ln -sf qartez $(INSTALL_DIR)/qartez-mcp
	@echo "\033[0;32m[+]\033[0m Symlink: $(INSTALL_DIR)/qartez-mcp -> qartez"

setup: ## Interactive IDE setup wizard (install deps + build + choose IDEs)
	@./install.sh --interactive

setup-windows: ## Interactive setup on Windows PowerShell
	@powershell -ExecutionPolicy Bypass -File .\install.ps1 -Interactive

install-windows: ## Build/install only on Windows PowerShell (skip IDE setup)
	@powershell -ExecutionPolicy Bypass -File .\install.ps1 -SkipSetup

uninstall: ## Remove qartez from all IDEs and uninstall binaries
	@$(INSTALL_DIR)/qartez-setup --uninstall 2>/dev/null || true
	@rm -f $(INSTALL_DIR)/qartez $(INSTALL_DIR)/qartez-mcp $(INSTALL_DIR)/qartez-guard $(INSTALL_DIR)/qartez-setup
	@echo "\033[0;32m[+]\033[0m Binaries removed: $(INSTALL_DIR)/qartez{,-mcp,-guard,-setup}"

clean: ## Remove build artifacts
	$(CARGO) clean

# --- Pre-release CI parity ---
# Run locally before tagging. Mirrors .github/workflows/{ci,deny}.yml so a
# release branch never ships with a check that only red-flags after the tag
# is already pushed (see v0.9.8: cargo-deny failed because deny.yml did not
# trigger on release/* branches and the dashboard subcrate had a wildcard
# path dep + missing license clarify).

ci: ci-fmt ci-clippy ci-deny ci-build ci-test ci-doc ## Run the full CI suite locally (matches GitHub Actions)
	@echo "\033[0;32m[+]\033[0m All CI checks passed locally"

ci-fmt: ## cargo fmt --check (matches ci.yml)
	$(CARGO) fmt --all -- --check

ci-clippy: ## cargo clippy with -D warnings (matches ci.yml default-features gate)
	$(CARGO) clippy --locked --all-targets -- -D warnings

ci-deny: ## cargo-deny advisories/bans/licenses/sources (matches deny.yml)
	@command -v cargo-deny >/dev/null 2>&1 || { \
		echo "\033[1;31m[!]\033[0m cargo-deny not installed. Run: cargo install --locked cargo-deny"; \
		exit 1; \
	}
	cargo-deny --log-level warn --manifest-path ./Cargo.toml --all-features check advisories bans licenses sources

ci-build: ## cargo build --locked --release (matches ci.yml)
	$(CARGO) build --locked --release

ci-test: ## cargo test --locked --release --no-fail-fast (matches ci.yml)
	$(CARGO) test --locked --release --no-fail-fast

ci-doc: ## cargo doc with -D warnings (matches ci.yml)
	RUSTDOCFLAGS="-D warnings" $(CARGO) doc --locked --no-deps

# --- Benchmarks ---

bench: ## Run full Rust benchmark (fresh measurements, no caching)
	@echo "\033[1;34m==>\033[0m Running full Rust benchmark..."
	@$(BENCH_RUN) --project-root . \
		--out-json reports/benchmark.json \
		--out-md reports/benchmark.md
	@echo "\033[0;32m[+]\033[0m Report: reports/benchmark.md"

bench-all: bench-fixtures ## Run all languages + cross-language summary
	@for lang in $(BENCH_LANGS); do \
		echo "\033[1;34m==>\033[0m Benchmarking $$lang..."; \
		$(BENCH_RUN) --project-root . --lang $$lang \
			--out-json reports/benchmark-$$lang.json \
			--out-md reports/benchmark-$$lang.md || { \
				echo "\033[1;33m[!]\033[0m $$lang failed, skipping"; \
				continue; \
			}; \
	done
	@echo "\033[1;34m==>\033[0m Generating cross-language summary..."
	@$(BENCH_RUN) --cross-lang-summary-only
	@echo "\033[0;32m[+]\033[0m Full benchmark complete. See reports/"

bench-fixtures: ## Clone and index fixture repos for non-Rust benchmarks
	@./scripts/setup-benchmark-fixtures.sh all

TARGET_DIR  = $(shell cargo metadata --no-deps --format-version 1 2>/dev/null | jq -r .target_directory)
BINARY      = $(TARGET_DIR)/release/qartez-mcp
GUARD_BIN   = $(TARGET_DIR)/release/qartez-guard
SETUP_BIN   = $(TARGET_DIR)/release/qartez-setup
INSTALL_DIR = $(HOME)/.local/bin
BENCH_RUN    = cargo run --quiet --release --features benchmark --bin benchmark --
BENCH_LANGS := rust typescript python go java

.PHONY: help test build install deploy setup uninstall clean \
        bench bench-all bench-fixtures

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-20s\033[0m %s\n", $$1, $$2}'

test: ## Run all tests
	cargo test

build: ## Build release binaries
	cargo build --release

install: build ## Build and install binaries to ~/.local/bin
	@mkdir -p $(INSTALL_DIR)
	@cp $(BINARY) $(INSTALL_DIR)/qartez-mcp
	@cp $(GUARD_BIN) $(INSTALL_DIR)/qartez-guard
	@cp $(SETUP_BIN) $(INSTALL_DIR)/qartez-setup
	@echo "\033[0;32m[+]\033[0m Binary: $(INSTALL_DIR)/qartez-mcp ($$(wc -c < $(BINARY) | awk '{printf "%.1f MB", $$1/1048576}'))"
	@echo "\033[0;32m[+]\033[0m Binary: $(INSTALL_DIR)/qartez-guard ($$(wc -c < $(GUARD_BIN) | awk '{printf "%.1f MB", $$1/1048576}'))"
	@echo "\033[0;32m[+]\033[0m Binary: $(INSTALL_DIR)/qartez-setup ($$(wc -c < $(SETUP_BIN) | awk '{printf "%.1f MB", $$1/1048576}'))"

deploy: install ## Full deploy: test, build, install, configure all detected IDEs
	@echo "\033[1;34m==>\033[0m Running tests..."
	@bash -c 'set -o pipefail; cargo test --quiet 2>&1 | grep -E "^(running|test result)"'
	@echo "\033[1;34m==>\033[0m Configuring all detected IDEs..."
	@$(INSTALL_DIR)/qartez-setup --yes
	@echo "\033[0;32m[+]\033[0m Deploy complete. Restart IDEs to pick up MCP changes."

setup: install ## Interactive IDE setup wizard (build + auto-detect)
	@$(INSTALL_DIR)/qartez-setup

uninstall: ## Remove qartez from all IDEs and uninstall binaries
	@$(INSTALL_DIR)/qartez-setup --uninstall 2>/dev/null || true
	@rm -f $(INSTALL_DIR)/qartez-mcp $(INSTALL_DIR)/qartez-guard $(INSTALL_DIR)/qartez-setup
	@echo "\033[0;32m[+]\033[0m Binaries removed: $(INSTALL_DIR)/qartez-{mcp,guard,setup}"

clean: ## Remove build artifacts
	cargo clean

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


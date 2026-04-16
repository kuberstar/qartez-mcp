# Contributing to Qartez MCP

Thanks for your interest in contributing to Qartez. This guide covers everything you need to get started.

## Development Setup

**Prerequisites:** Rust 2024 edition (1.85+), Git.

```bash
git clone https://github.com/kuberstar/qartez-mcp.git
cd qartez-mcp
cargo build
cargo test
```

The project compiles to four binaries:
- `qartez-mcp` - the MCP server (main entry point)
- `qartez-guard` - PreToolUse hook for Claude Code
- `qartez-setup` - interactive IDE setup wizard
- `benchmark` - per-tool benchmark harness (requires `--features benchmark`)

## Project Structure

```
src/
  main.rs          # CLI entry point and MCP server startup
  lib.rs           # Library root, re-exports all modules
  server/
    mod.rs         # All 24 MCP tool handlers (~4k lines)
    params.rs      # Tool parameter structs (serde + JSON Schema)
    helpers.rs     # Shared helper functions
    treesitter.rs  # AST walking utilities
    cache.rs       # Per-file parse cache
  storage/
    schema.rs      # SQLite schema (CREATE TABLE statements)
    read.rs        # All SELECT queries
    write.rs       # All INSERT/UPDATE/DELETE queries
    models.rs      # Row types (FileRow, SymbolRow, etc.)
  index/
    mod.rs         # Full and incremental indexing pipelines
    walker.rs      # File discovery (respects .gitignore)
    symbols.rs     # Tree-sitter symbol extraction
    languages/     # Per-language parsing rules (37 languages)
  graph/
    pagerank.rs    # File and symbol PageRank computation
    blast.rs       # Transitive blast radius analysis
    leiden.rs      # Community detection (Louvain + Leiden refinement)
    wiki.rs        # Architecture document generation
    boundaries.rs  # Architecture boundary enforcement
  git/
    cochange.rs    # Co-change pair analysis from git history
```

## Code Style

- **Formatting:** `cargo fmt` before every commit. The project uses default `rustfmt` settings.
- **Linting:** `cargo clippy` must pass with zero warnings.
- **Language:** All code comments and documentation must be in English.
- **Visibility:** The crate enables `unreachable_pub` as a warning. Use `pub(crate)` or `pub(super)` for items that are not part of the external API.

## Running Tests

```bash
# Full test suite (1000+ tests, takes ~2 seconds)
cargo test

# Run a specific test
cargo test rename_apply

# Run tests for a specific module
cargo test -- server::quality_tests

# Run with output visible
cargo test -- --nocapture
```

The test suite includes:
- **Unit tests** - in `#[cfg(test)]` modules within source files
- **Quality tests** - in `src/server/quality_tests.rs`, covering all 24 tools
- **Business logic tests** - in `tests/business_logic.rs`
- **Integration tests** - in `tests/tools.rs`, end-to-end indexing scenarios

## Making Changes

### Adding a New Language

1. Add the tree-sitter grammar dependency to `Cargo.toml`
2. Create a parser module in `src/index/languages/`
3. Register it in `src/index/parser.rs`
4. Add integration tests in `tests/tools.rs`

### Adding a New Tool

1. Add the parameter struct in `src/server/params.rs`
2. Add the handler method in `src/server/mod.rs`
3. Add quality tests in `src/server/quality_tests.rs`

### Modifying Destructive Tools

The three destructive tools (`qartez_rename`, `qartez_move`, `qartez_rename_file`) modify user files on disk. Changes to these tools must include tests for both preview mode and apply mode. See the "Destructive Tools" section in `quality_tests.rs` for the test pattern.

## Contributor License Agreement (CLA)

All contributors must sign the [Contributor License Agreement](CLA.md) before
their first pull request can be merged. The CLA is handled automatically by the
CLA Assistant bot integrated into the repository.

**Why a CLA?** Qartez is distributed under a dual-license model (open-source
and commercial). The CLA ensures that we have the legal rights to distribute
contributions under both licenses, while you retain full copyright ownership of
your work. Your contributions will always remain available under the
open-source terms.

**Individual contributors:** You will be prompted to sign the CLA electronically
the first time you open a pull request. This is a one-time process.

**Corporate contributors:** If you are contributing on behalf of a company,
please contact hello@qartez.dev with the subject "Corporate CLA" before
opening your first pull request.

## Pull Request Process

1. Fork the repository
2. Create a feature branch from `main`
3. Make your changes
4. Run the full test suite: `cargo test`
5. Run the linter: `cargo clippy`
6. Run the formatter: `cargo fmt`
7. Open a PR with a clear description of what changed and why
8. Sign the CLA when prompted (first-time contributors only)

Keep PRs focused on a single concern. If you are fixing a bug and also want to refactor nearby code, please split them into separate PRs.

## Reporting Issues

Use the [GitHub issue tracker](https://github.com/kuberstar/qartez-mcp/issues). For bug reports, include:
- Qartez version (`qartez-mcp --version`)
- Operating system
- Steps to reproduce
- Expected vs actual behavior

## License

By contributing, you agree to the terms of the [Contributor License Agreement](CLA.md). Your contributions will be licensed under the same terms as the project (see [LICENSE](LICENSE)), and you retain full copyright ownership of your work.

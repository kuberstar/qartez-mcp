# Configuration

Qartez is designed to work with zero configuration. It auto-detects project
roots, respects gitignore, and places its database automatically. This
document covers the configuration surfaces that exist for when defaults
aren't enough.

## Project root detection

Qartez finds your project root by walking up from cwd looking for these markers:

- `.git`
- `Cargo.toml`
- `package.json`
- `go.mod`
- `pyproject.toml`

If cwd has no markers but its children do (the "meta-directory" pattern where a
folder groups multiple repos), each child with a marker becomes a separate root.

The home directory is explicitly excluded to prevent accidentally indexing `~/`.

Override with `--root <path>` (repeatable for multi-root).

## Workspace expansion

When a root contains a workspace config, qartez expands it automatically:

- **Cargo** — reads `[workspace] members` from `Cargo.toml`, expands globs
- **npm/yarn/pnpm** — reads `"workspaces"` from `package.json` (array or `{packages: [...]}`)
- **Go** — reads `use` directives from `go.work`

Each workspace member becomes an additional root. The original root is kept
(it often contains shared config, scripts, etc.).

## Database location

- **Single-root** — `.qartez/index.db` inside the project root
- **Multi-root** — `.qartez/index.db` in cwd (the parent of the roots)
- **Override** — `--db-path <path>`

The `.qartez/` directory is created automatically. Add it to `.gitignore` if
you don't want to track it (the database is a build artifact, not source).

## File exclusion

### .gitignore

Qartez respects `.gitignore` (local, global, and `.git/info/exclude`)
automatically via the `ignore` crate. If your dependency directories
(`node_modules/`, `venv/`, `vendor/`, etc.) are already in `.gitignore`,
they won't be indexed.

### .qartezignore

For qartez-specific exclusions that shouldn't go in `.gitignore`, create a
`.qartezignore` file in your project root. Same glob syntax as `.gitignore`:

```gitignore
# Exclude generated code
generated/
*.gen.go

# Exclude vendored dependencies
vendor/

# Exclude large test fixtures
testdata/fixtures/
```

### File size limit

Files larger than 1 MB are skipped by default. Override with:

```sh
export QARTEZ_MAX_FILE_BYTES=5000000  # 5 MB
```

## CLI flags

```
qartez [OPTIONS] [COMMAND]
```

| Flag | Default | Purpose |
|------|---------|---------|
| `--root <path>` | auto-detected | Project root (repeatable) |
| `--reindex` | false | Force full re-index, ignoring mtime cache |
| `--git-depth <n>` | 300 | Max commits for co-change and knowledge analysis |
| `--db-path <path>` | auto | Override database location |
| `--no-watch` | false | Disable file watcher |
| `--log-level <level>` | info | Tracing log level |
| `--wiki <path>` | none | Generate architecture wiki and exit |
| `--leiden-resolution <f>` | 1.0 | Leiden clustering resolution for wiki |
| `--format <fmt>` | human | Output format: `human`, `json`, `compact` |

When run without a subcommand and stdin is a terminal, qartez prints help.
When stdin is piped (by an MCP client), it starts the MCP server.

## Security rules (`.qartez/security.toml`)

The `qartez_security` tool ships with 13 built-in rules. You can disable
rules or add custom ones via `.qartez/security.toml` in your project root:

```toml
# Disable rules by ID
disable = ["SEC007", "SEC009"]

# Add custom rules
[[rules]]
id = "CUSTOM001"
name = "No println in production"
severity = "low"
category = "quality"
pattern = { type = "body_regex", regex = "println!" }
description = "Use tracing instead of println! in production code"
language = "rust"  # optional: restrict to a language
```

Severity levels: `low`, `medium`, `high`, `critical`.

Pattern types:
- `body_regex` — match against the full source body of each symbol
- `symbol_name` — match against the symbol's name
- `signature_regex` — match against the symbol's signature

## Architecture boundaries (`.qartez/boundaries.toml`)

The `qartez_boundaries` tool checks import rules between architectural layers.
Define rules in `.qartez/boundaries.toml`:

```toml
[[rules]]
name = "storage isolation"
from = "src/server/"
deny = ["src/index/"]
reason = "Server tools should use storage queries, not the indexer directly"

[[rules]]
name = "no circular deps"
from = "src/graph/"
deny = ["src/server/"]
```

To generate a starter config from the current Leiden clustering:

```
qartez_boundaries suggest=true write_to=".qartez/boundaries.toml"
```

## Environment variables

| Variable | Default | Purpose |
|----------|---------|---------|
| `QARTEZ_PROGRESSIVE` | unset | Set to `1` for progressive tool disclosure |
| `QARTEZ_MAX_FILE_BYTES` | 1000000 | Max file size for indexing (bytes) |
| `QARTEZ_NO_AUTO_UPDATE` | unset | Disable background update checks |

## MCP client configuration

### Claude Code

Add to `.claude/settings.json` or the global settings:

```json
{
  "mcpServers": {
    "qartez": {
      "command": "qartez",
      "args": []
    }
  }
}
```

For multi-root or custom options:

```json
{
  "mcpServers": {
    "qartez": {
      "command": "qartez",
      "args": ["--root", "/path/to/repo-a", "--root", "/path/to/repo-b"]
    }
  }
}
```

### Cursor / other MCP clients

The configuration structure is the same — qartez communicates over
stdin/stdout using standard MCP JSON-RPC. Consult your client's documentation
for where to place the MCP server configuration.

#!/bin/bash
# qartez-session-start.sh - SessionStart hook for Claude Code
# Auto-indexes a project on session start if it looks like a code repo
# and no .qartez/ index exists yet. Fire-and-forget: the hook returns
# immediately and the indexer runs detached in the background.

PROJECT_DIR="${CLAUDE_PROJECT_DIR:-$PWD}"

# No project dir → nothing to do
[ -z "$PROJECT_DIR" ] && exit 0
[ ! -d "$PROJECT_DIR" ] && exit 0

# Skip dangerous roots where auto-indexing would explode
case "$PROJECT_DIR" in
    "$HOME"|"$HOME/"|"/"|"") exit 0 ;;
esac

# Already indexed → nothing to do
[ -d "$PROJECT_DIR/.qartez" ] && exit 0

# Require at least one repo marker so we don't index random folders
HAS_MARKER=0
for marker in .git Cargo.toml package.json pyproject.toml go.mod; do
    if [ -e "$PROJECT_DIR/$marker" ]; then
        HAS_MARKER=1
        break
    fi
done
[ "$HAS_MARKER" -eq 0 ] && exit 0

# Locate the qartez-mcp binary (respects QARTEZ_BINARY override)
BINARY="${QARTEZ_BINARY:-}"
if [ -z "$BINARY" ]; then
    for candidate in \
        "$HOME/.local/bin/qartez-mcp" \
        "$(command -v qartez-mcp 2>/dev/null || true)"; do
        if [ -n "$candidate" ] && [ -x "$candidate" ]; then
            BINARY="$candidate"
            break
        fi
    done
fi
[ -z "$BINARY" ] && exit 0

# Fire-and-forget background reindex. nohup + setsid (when available) detaches
# the process so it survives hook completion. Output is routed to a log so the
# user can inspect failures.
LOG_DIR="$HOME/.cache/qartez-mcp"
mkdir -p "$LOG_DIR" 2>/dev/null || true
LOG_FILE="$LOG_DIR/session-index.log"

if command -v setsid >/dev/null 2>&1; then
    setsid nohup "$BINARY" --root "$PROJECT_DIR" --reindex >>"$LOG_FILE" 2>&1 < /dev/null &
else
    nohup "$BINARY" --root "$PROJECT_DIR" --reindex >>"$LOG_FILE" 2>&1 < /dev/null &
fi
disown 2>/dev/null || true

exit 0

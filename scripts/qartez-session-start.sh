#!/bin/bash
# qartez-session-start.sh - SessionStart hook for Claude Code
# Auto-indexes a project on session start if it looks like a code repo
# and no .qartez/ index exists yet. Fire-and-forget: the hook returns
# immediately and the indexer runs detached in the background.

set -u

PROJECT_DIR="${CLAUDE_PROJECT_DIR:-$PWD}"
INDEX_PATH="${QARTEZ_INDEX_PATH:-$PROJECT_DIR/.qartez}"
if [[ "$INDEX_PATH" != /* ]]; then
    INDEX_PATH="$PROJECT_DIR/$INDEX_PATH"
fi
STATUS_PATH="${QARTEZ_STATUS_PATH:-$INDEX_PATH/status.json}"
STALE_THRESHOLD_SECS="${QARTEZ_STALE_THRESHOLD_SECS:-300}"

# No project dir → nothing to do
[ -z "$PROJECT_DIR" ] && exit 0
[ ! -d "$PROJECT_DIR" ] && exit 0

# Skip dangerous roots where auto-indexing would explode
case "$PROJECT_DIR" in
    "$HOME"|"$HOME/"|"/"|"") exit 0 ;;
esac

# Already indexed → nothing to do
[ -d "$INDEX_PATH" ] && exit 0

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

mkdir -p "$INDEX_PATH" 2>/dev/null || true

STARTED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
PROJECT_ROOT_REAL="$(cd "$PROJECT_DIR" 2>/dev/null && pwd)"

write_status_indexing() {
    cat > "$STATUS_PATH" <<JSON
{
  "schema_version": "1",
  "state": "INDEXING",
  "project_root": "$PROJECT_ROOT_REAL",
  "index_version": null,
  "languages": [],
  "file_count": 0,
  "is_stale": false,
  "stale_threshold_secs": $STALE_THRESHOLD_SECS,
  "last_error": null,
  "started_at": "$STARTED_AT",
  "ready_at": null,
  "progress_hint": "starting index build"
}
JSON
}

write_status_ready() {
    local ready_at file_count
    ready_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    file_count="$(find "$PROJECT_DIR" -type f \( -name '*.ts' -o -name '*.tsx' -o -name '*.js' -o -name '*.jsx' -o -name '*.rs' -o -name '*.go' -o -name '*.py' -o -name '*.java' -o -name '*.kt' -o -name '*.swift' -o -name '*.c' -o -name '*.cpp' -o -name '*.rb' -o -name '*.php' \) 2>/dev/null | wc -l | tr -d ' ')"
    [ -z "$file_count" ] && file_count=0

    cat > "$STATUS_PATH" <<JSON
{
  "schema_version": "1",
  "state": "READY",
  "project_root": "$PROJECT_ROOT_REAL",
  "index_version": "$ready_at",
  "languages": [],
  "file_count": $file_count,
  "is_stale": false,
  "stale_threshold_secs": $STALE_THRESHOLD_SECS,
  "last_error": null,
  "started_at": "$STARTED_AT",
  "ready_at": "$ready_at",
  "progress_hint": null
}
JSON
}

write_status_unavailable() {
    local error_message="$1"
    cat > "$STATUS_PATH" <<JSON
{
  "schema_version": "1",
  "state": "UNAVAILABLE",
  "project_root": "$PROJECT_ROOT_REAL",
  "index_version": null,
  "languages": [],
  "file_count": 0,
  "is_stale": false,
  "stale_threshold_secs": $STALE_THRESHOLD_SECS,
  "last_error": "$error_message",
  "started_at": "$STARTED_AT",
  "ready_at": null,
  "progress_hint": null
}
JSON
}

write_status_indexing

# Fire-and-forget background reindex. nohup + setsid (when available) detaches
# the process so it survives hook completion. Output is routed to a log so the
# user can inspect failures.
LOG_DIR="$HOME/.cache/qartez-mcp"
mkdir -p "$LOG_DIR" 2>/dev/null || true
LOG_FILE="$LOG_DIR/session-index.log"

(
    if command -v setsid >/dev/null 2>&1; then
        setsid "$BINARY" --root "$PROJECT_DIR" --reindex >>"$LOG_FILE" 2>&1 < /dev/null
    else
        "$BINARY" --root "$PROJECT_DIR" --reindex >>"$LOG_FILE" 2>&1 < /dev/null
    fi
    exit_code=$?
    if [ "$exit_code" -eq 0 ]; then
        write_status_ready
    else
        write_status_unavailable "index build failed (exit $exit_code)"
    fi
) >/dev/null 2>&1 &

disown 2>/dev/null || true

exit 0

#!/bin/bash
# qartez-guard.sh - PreToolUse hook for Claude Code
# Denies Glob/Grep when qartez MCP is indexed for the current project.
# Forces Claude to use qartez_map/qartez_find/qartez_grep instead.
# Falls through (allows) when .qartez/ does not exist.

QARTEZ_DIR="${CLAUDE_PROJECT_DIR:-.}/.qartez"

# No qartez index = allow built-in tools
[ ! -d "$QARTEZ_DIR" ] && exit 0

# Qartez is available - read tool input and redirect
INPUT=$(cat)
TOOL_NAME=$(echo "$INPUT" | jq -r '.tool_name // empty')

case "$TOOL_NAME" in
  Glob)
    printf '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"STOP: qartez MCP is available. Use `qartez_map` for project structure or `qartez_find` to locate symbols. Use Glob ONLY for non-code file patterns (e.g., *.toml, *.json) - if so, use Bash find/ls instead."}}'
    ;;
  Grep)
    printf '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"STOP: qartez MCP is available. Use `qartez_grep` for symbol search or `qartez_find` for definitions. Use Grep ONLY for non-symbol text search (e.g., TODO comments, string literals) - if so, use Bash grep instead."}}'
    ;;
  *)
    exit 0
    ;;
esac

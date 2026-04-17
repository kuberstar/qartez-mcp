#!/bin/bash
# qartez-guard.sh - PreToolUse hook for Claude Code
# Emits machine-readable denial payloads for built-in code tools.

set -u

PROJECT_DIR="${CLAUDE_PROJECT_DIR:-$PWD}"

INDEX_PATH="${QARTEZ_INDEX_PATH:-$PROJECT_DIR/.qartez}"
if [[ "$INDEX_PATH" != /* ]]; then
  INDEX_PATH="$PROJECT_DIR/$INDEX_PATH"
fi

STATUS_PATH="${QARTEZ_STATUS_PATH:-$INDEX_PATH/status.json}"
ACK_TTL_SECS="${QARTEZ_ACK_TTL_SECS:-600}"

# Explicit global off-switch.
if [ "${QARTEZ_GUARD_DISABLE:-0}" = "1" ]; then
  exit 0
fi

# No qartez index = allow built-in tools
[ ! -d "$INDEX_PATH" ] && exit 0

INPUT="$(cat)"
TOOL_NAME="$(printf '%s' "$INPUT" | jq -r '.tool_name // empty')"
FILE_PATH="$(printf '%s' "$INPUT" | jq -r '.tool_input.file_path // .tool_input.filePath // .file_path // .filePath // empty')"

read_status_state() {
  if [ -f "$STATUS_PATH" ]; then
    jq -r '.state // "READY"' "$STATUS_PATH" 2>/dev/null || printf 'READY'
  else
    printf 'READY'
  fi
}

is_source_file() {
  case "${1##*.}" in
    ts|tsx|js|jsx|mjs|cjs|rs|go|py|rb|java|kt|swift|c|cc|cpp|h|hpp|cs|fs|elm|hs|vue|svelte|sh|bash)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

deny_with_payload() {
  local human_message="$1"
  local payload_json="$2"
  local reason

  reason="${human_message}\n\nQartez denial payload:\n${payload_json}"
  jq -cn --arg reason "$reason" '{hookSpecificOutput:{hookEventName:"PreToolUse",permissionDecision:"deny",permissionDecisionReason:$reason}}'
  exit 0
}

emit_builtin_denial() {
  local tool_attempted="$1"
  local replacement_json="$2"
  local reason_code="$3"
  local retryable="$4"
  local state="$5"
  local human_message="$6"

  local payload
  if [ -n "$FILE_PATH" ]; then
    payload="$(jq -cn --arg t "$tool_attempted" --arg fp "$FILE_PATH" --argjson replacement "$replacement_json" --arg reason "$reason_code" --argjson retryable "$retryable" --arg state "$state" '{qartez:{type:"DENIED_BUILTIN_CODE_TOOL",tool_attempted:$t,file_path:$fp,replacement:$replacement,reason_code:$reason,retryable:$retryable,state:$state}}')"
  else
    payload="$(jq -cn --arg t "$tool_attempted" --argjson replacement "$replacement_json" --arg reason "$reason_code" --argjson retryable "$retryable" --arg state "$state" '{qartez:{type:"DENIED_BUILTIN_CODE_TOOL",tool_attempted:$t,replacement:$replacement,reason_code:$reason,retryable:$retryable,state:$state}}')"
  fi

  deny_with_payload "$human_message" "$payload"
}

emit_risk_ack_required() {
  local human_message="$1"
  local payload
  payload="$(jq -cn --arg fp "$FILE_PATH" --argjson ttl "$ACK_TTL_SECS" '{qartez:{type:"RISK_ACK_REQUIRED",tool_attempted:"edit",file_path:$fp,replacement:["qartez_impact"],reason_code:"LOAD_BEARING_FILE",pagerank:null,blast_radius:null,retryable:true,ack_ttl_secs:$ttl}}')"
  deny_with_payload "$human_message" "$payload"
}

STATE="$(read_status_state)"

if [ "$STATE" = "INDEXING" ]; then
  case "$TOOL_NAME" in
    Glob|Grep|Read|View|cat)
      emit_builtin_denial "$TOOL_NAME" '[]' 'INDEX_NOT_READY' 'false' 'INDEXING' 'Qartez index is currently building. Wait for READY before using code search/read tools.'
      ;;
  esac
fi

case "$TOOL_NAME" in
  Glob)
    emit_builtin_denial "Glob" '["qartez_map"]' 'SOURCE_CODE_REQUIRES_QARTEZ' 'true' "$STATE" 'STOP: qartez MCP is available. Use qartez_map for project structure and file discovery.'
    ;;
  Grep)
    emit_builtin_denial "Grep" '["qartez_grep","qartez_find"]' 'SOURCE_CODE_REQUIRES_QARTEZ' 'true' "$STATE" 'STOP: qartez MCP is available. Use qartez_grep for symbol/body search or qartez_find for exact symbol definitions.'
    ;;
  Read|View|cat)
    if [ -n "$FILE_PATH" ] && is_source_file "$FILE_PATH"; then
      emit_builtin_denial "$TOOL_NAME" '["qartez_read","qartez_outline"]' 'SOURCE_CODE_REQUIRES_QARTEZ' 'true' "$STATE" 'STOP: source-code reads must use qartez_read or qartez_outline.'
    fi
    ;;
  Edit|Write|MultiEdit)
    if [ -n "$FILE_PATH" ] && is_source_file "$FILE_PATH"; then
      emit_risk_ack_required 'Load-bearing edit requires risk acknowledgment. Run qartez_impact on the target file, then retry the edit.'
    fi
    ;;
  *)
    exit 0
    ;;
esac

exit 0

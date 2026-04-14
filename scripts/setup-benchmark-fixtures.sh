#!/bin/bash
# setup-benchmark-fixtures.sh
#
# Reads benchmarks/fixtures.toml and clones each pinned OSS repository into
# target/benchmark-fixtures/<lang>/. Each fixture is then indexed with
# qartez-mcp so the benchmark harness can point --project-root at it.
#
# Usage:
#   ./scripts/setup-benchmark-fixtures.sh               # all languages
#   ./scripts/setup-benchmark-fixtures.sh all           # all languages
#   ./scripts/setup-benchmark-fixtures.sh typescript    # only TypeScript
#   ./scripts/setup-benchmark-fixtures.sh python go     # Python and Go
#
# The clone is shallow-ish (blobless, no-checkout) so we can pin an arbitrary
# commit without downloading full history for every pinned SHA.
#
# Idempotent: if the target already points at the pinned commit, nothing
# happens. If the commit differs, the directory is removed and recloned.
#
# If the qartez-mcp release binary is not yet built, the clone step still
# runs and the script exits 0 with a message asking the user to build the
# binary and re-run. Clone work is never thrown away on binary-missing.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
MANIFEST="$REPO_DIR/benchmarks/fixtures.toml"
FIXTURE_ROOT="$REPO_DIR/target/benchmark-fixtures"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

info()  { printf '%b[+]%b %s\n' "$GREEN" "$NC" "$1"; }
warn()  { printf '%b[!]%b %s\n' "$YELLOW" "$NC" "$1"; }
note()  { printf '%b[.]%b %s\n' "$BLUE" "$NC" "$1"; }
error() { printf '%b[x]%b %s\n' "$RED" "$NC" "$1" >&2; }

check_deps() {
    command -v git >/dev/null 2>&1 || {
        error "git is required but not on PATH."
        exit 1
    }
}

# Extract a field for a given section from the TOML manifest.
# Usage: toml_field <section> <key>
# Implementation is intentionally simple — we control the TOML format.
toml_field() {
    local section="$1"
    local key="$2"
    awk -v section="[$section]" -v key="$key" '
        $0 == section { in_section = 1; next }
        /^\[/         { in_section = 0 }
        in_section && $1 == key {
            # Strip "key = " and surrounding quotes.
            sub(/^[^=]*= *"?/, "", $0)
            sub(/"? *$/, "", $0)
            print
            exit
        }
    ' "$MANIFEST"
}

# List every [section] in the manifest, in order.
toml_sections() {
    awk '
        /^\[[a-z][a-z0-9_-]*\]$/ {
            s = $0
            gsub(/[\[\]]/, "", s)
            print s
        }
    ' "$MANIFEST"
}

# Return 0 if the fixture directory already points at the pinned commit.
is_up_to_date() {
    local dir="$1"
    local want_commit="$2"
    [ -d "$dir/.git" ] || return 1
    local have_commit
    have_commit=$(git -C "$dir" rev-parse HEAD 2>/dev/null || echo "")
    [ "$have_commit" = "$want_commit" ]
}

clone_fixture() {
    local lang="$1"
    local repo
    local commit
    local desc
    repo=$(toml_field "$lang" "repo")
    commit=$(toml_field "$lang" "commit")
    desc=$(toml_field "$lang" "description")

    if [ -z "$repo" ] || [ -z "$commit" ]; then
        error "$lang: missing repo or commit in $MANIFEST"
        return 1
    fi

    local dir="$FIXTURE_ROOT/$lang"

    if is_up_to_date "$dir" "$commit"; then
        info "$lang: already at $commit"
        return 0
    fi

    if [ -d "$dir" ]; then
        warn "$lang: directory exists but is on wrong commit, removing"
        rm -rf "$dir"
    fi

    note "$lang: cloning $repo"
    note "$lang: $desc"
    mkdir -p "$FIXTURE_ROOT"

    # Blobless clone without checkout, then check out the pinned commit.
    # --depth=1 would not let us pin an arbitrary older SHA, and a full
    # clone wastes bandwidth on binary blobs we never look at.
    if ! git clone \
        --filter=blob:none \
        --no-checkout \
        --quiet \
        "$repo" "$dir"
    then
        error "$lang: git clone failed for $repo"
        return 1
    fi

    # If the default fetch refspec didn't include the pinned commit
    # (common for very old SHAs on busy repos), fetch it explicitly.
    if ! git -C "$dir" cat-file -e "$commit" 2>/dev/null; then
        note "$lang: pinned commit $commit not in default fetch, fetching directly"
        if ! git -C "$dir" fetch --quiet --filter=blob:none origin "$commit"; then
            error "$lang: unable to fetch pinned commit $commit from $repo"
            return 1
        fi
    fi

    if ! git -C "$dir" -c advice.detachedHead=false checkout --quiet "$commit"; then
        error "$lang: failed to check out $commit"
        return 1
    fi

    info "$lang: checked out $commit"
    return 0
}

# Locate the qartez-mcp release binary. Searched in this order:
#   $QARTEZ_BINARY (if set)
#   $REPO_DIR/target/release/qartez-mcp
#   qartez-mcp on $PATH
find_qartez_binary() {
    if [ -n "${QARTEZ_BINARY:-}" ] && [ -x "$QARTEZ_BINARY" ]; then
        printf '%s\n' "$QARTEZ_BINARY"
        return 0
    fi
    local local_bin="$REPO_DIR/target/release/qartez-mcp"
    if [ -x "$local_bin" ]; then
        printf '%s\n' "$local_bin"
        return 0
    fi
    if command -v qartez-mcp >/dev/null 2>&1; then
        command -v qartez-mcp
        return 0
    fi
    return 1
}

# Drive qartez-mcp into indexing the fixture, then exit. The binary auto-
# indexes when started with --root and then serves MCP on stdin; closing
# stdin makes it exit with a non-zero "connection closed: initialize request"
# error from the MCP handshake. That exit status is unrelated to whether
# indexing itself succeeded, so we verify success by checking for the
# `Index complete` log line and the resulting index.db on disk.
index_fixture() {
    local lang="$1"
    local binary="$2"
    local dir="$FIXTURE_ROOT/$lang"
    local db="$dir/.qartez/index.db"

    if [ ! -d "$dir" ]; then
        warn "$lang: skipping index (directory missing)"
        return 1
    fi

    note "$lang: indexing with $binary"
    local log
    log=$(mktemp -t qartez-index.XXXXXX)
    # Ignore exit status intentionally — MCP stdin EOF always produces
    # a non-zero exit and we detect indexing success separately below.
    "$binary" --root "$dir" --log-level info </dev/null >/dev/null 2>"$log" || true

    if ! grep -q "Index complete" "$log"; then
        error "$lang: indexing did not complete, log follows:"
        sed 's/^/    /' "$log" >&2
        rm -f "$log"
        return 1
    fi
    rm -f "$log"

    if [ ! -f "$db" ]; then
        error "$lang: index database not found at $db after indexing"
        return 1
    fi
    info "$lang: index db at $db"
    return 0
}

sanity_check() {
    printf '\n'
    info "Fixture sizes:"
    if [ ! -d "$FIXTURE_ROOT" ]; then
        note "  (no fixtures cloned yet)"
        return 0
    fi
    local any=0
    local d
    for d in "$FIXTURE_ROOT"/*/; do
        [ -d "$d" ] || continue
        any=1
        local size
        size=$(du -sh "$d" 2>/dev/null | awk '{print $1}')
        local commit
        commit=$(git -C "$d" rev-parse --short HEAD 2>/dev/null || echo "unknown")
        printf '    %-14s %-8s %s\n' "$(basename "$d")" "$size" "$commit"
    done
    if [ "$any" = "0" ]; then
        note "  (no fixtures cloned yet)"
    fi
}

print_usage() {
    cat <<'EOF'
Usage: setup-benchmark-fixtures.sh [all | <lang>...]

Clones the pinned fixtures listed in benchmarks/fixtures.toml into
target/benchmark-fixtures/<lang>/ and indexes each with qartez-mcp.

Arguments:
  all                (default) set up every fixture
  <lang> [<lang>...] set up only the given languages

Languages are whatever [sections] exist in benchmarks/fixtures.toml.
Currently: typescript, python, go, java.

Environment:
  QARTEZ_BINARY     override the path to the qartez-mcp release binary
EOF
}

main() {
    check_deps

    if [ ! -f "$MANIFEST" ]; then
        error "manifest not found at $MANIFEST"
        exit 1
    fi

    local -a wanted=()
    if [ "$#" -eq 0 ] || [ "$1" = "all" ]; then
        while IFS= read -r s; do
            wanted+=("$s")
        done < <(toml_sections)
    else
        for arg in "$@"; do
            case "$arg" in
                -h|--help|help)
                    print_usage
                    exit 0
                    ;;
                *)
                    wanted+=("$arg")
                    ;;
            esac
        done
    fi

    if [ "${#wanted[@]}" -eq 0 ]; then
        error "no fixtures selected"
        print_usage
        exit 1
    fi

    # Validate requested languages exist in the manifest.
    local -a known=()
    while IFS= read -r s; do
        known+=("$s")
    done < <(toml_sections)

    local l
    local k
    local found
    for l in "${wanted[@]}"; do
        found=0
        for k in "${known[@]}"; do
            if [ "$l" = "$k" ]; then
                found=1
                break
            fi
        done
        if [ "$found" = "0" ]; then
            error "unknown fixture '$l'. Known: ${known[*]}"
            exit 1
        fi
    done

    local binary=""
    if binary=$(find_qartez_binary); then
        info "Using qartez-mcp binary: $binary"
    else
        warn "qartez-mcp release binary not found."
        warn "Clones will run, but fixtures will not be indexed."
        warn "Build the binary with: cargo build --release --bin qartez-mcp"
        warn "Then re-run this script to index."
    fi

    local clone_failures=0
    local index_failures=0
    for l in "${wanted[@]}"; do
        printf '\n'
        note "=== $l ==="
        if ! clone_fixture "$l"; then
            clone_failures=$((clone_failures + 1))
            continue
        fi
        if [ -n "$binary" ]; then
            if ! index_fixture "$l" "$binary"; then
                index_failures=$((index_failures + 1))
            fi
        fi
    done

    sanity_check

    printf '\n'
    if [ "$clone_failures" = "0" ] && [ "$index_failures" = "0" ]; then
        info "Done. All ${#wanted[@]} fixture(s) ready."
        exit 0
    fi

    if [ "$clone_failures" != "0" ]; then
        error "$clone_failures fixture(s) failed to clone"
    fi
    if [ "$index_failures" != "0" ]; then
        error "$index_failures fixture(s) failed to index"
    fi
    # Non-zero exit so CI can notice, but we already ran through every lang.
    exit 1
}

main "$@"

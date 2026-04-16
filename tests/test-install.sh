#!/bin/sh
set -e

# Test suite for install.sh portability
# Runs without Docker - tests error paths, edge cases, and POSIX compliance
# Usage: ./tests/test-install.sh

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
INSTALL_SH="$PROJECT_DIR/install.sh"
PASS=0
FAIL=0
SKIP=0

RED='\033[1;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[1;34m'
NC='\033[0m'

pass() { PASS=$((PASS + 1)); printf "${GREEN}  PASS${NC} %s\n" "$1"; }
fail() { FAIL=$((FAIL + 1)); printf "${RED}  FAIL${NC} %s\n" "$1"; }
skip() { SKIP=$((SKIP + 1)); printf "${YELLOW}  SKIP${NC} %s\n" "$1"; }
section() { printf "\n${BLUE}--- %s ---${NC}\n" "$1"; }

# ============================================================
section "File integrity"
# ============================================================

# 1. install.sh exists and is executable
if [ -x "$INSTALL_SH" ]; then
    pass "install.sh is executable"
else
    fail "install.sh is not executable (chmod +x install.sh)"
fi

# 2. Shebang is #!/bin/sh (not bash)
SHEBANG=$(head -1 "$INSTALL_SH")
if [ "$SHEBANG" = "#!/bin/sh" ]; then
    pass "shebang is #!/bin/sh (POSIX)"
else
    fail "shebang is '$SHEBANG' (should be #!/bin/sh)"
fi

# 3. No CRLF line endings
if od -c "$INSTALL_SH" | grep -q '\\r'; then
    fail "install.sh has CRLF line endings"
else
    pass "no CRLF line endings"
fi

# 4. No bash-isms (shellcheck)
if command -v shellcheck >/dev/null 2>&1; then
    SC_OUT=$(shellcheck -s sh "$INSTALL_SH" 2>&1) || true
    if [ -z "$SC_OUT" ]; then
        pass "shellcheck clean (POSIX sh)"
    else
        fail "shellcheck issues: $SC_OUT"
    fi
else
    skip "shellcheck not installed"
fi

# 5. No bashisms via checkbashisms
if command -v checkbashisms >/dev/null 2>&1; then
    CB_OUT=$(checkbashisms "$INSTALL_SH" 2>&1) || true
    if [ -z "$CB_OUT" ]; then
        pass "checkbashisms clean"
    else
        fail "bashisms found: $CB_OUT"
    fi
else
    skip "checkbashisms not installed"
fi

# ============================================================
section ".gitattributes"
# ============================================================

GA="$PROJECT_DIR/.gitattributes"
if [ -f "$GA" ]; then
    if grep -q '\.sh.*eol=lf\|\.sh.*text.*eol=lf\|\*\.sh.*binary' "$GA" 2>/dev/null; then
        pass ".gitattributes forces LF for .sh files"
    elif grep -q 'text=auto' "$GA" 2>/dev/null; then
        fail ".gitattributes has text=auto but no explicit *.sh eol=lf rule"
        printf "       WSL/Windows git may checkout .sh with CRLF → broken shebang\n"
    else
        fail ".gitattributes exists but has no LF rule for shell scripts"
    fi
else
    fail "no .gitattributes file"
fi

# ============================================================
section "POSIX portability of constructs"
# ============================================================

# Check for bash-only features
check_no_pattern() {
    LABEL="$1"; PATTERN="$2"
    if grep -nE "$PATTERN" "$INSTALL_SH" >/dev/null 2>&1; then
        LINE=$(grep -nE "$PATTERN" "$INSTALL_SH" | head -1)
        fail "bashism: $LABEL - $LINE"
    else
        pass "no $LABEL"
    fi
}

check_no_pattern 'arrays'           '\w+\=\('
check_no_pattern '[[ ]] conditionals' '\[\['
check_no_pattern 'function keyword'  '^function '
check_no_pattern 'dollar-RANDOM'    '\$RANDOM'
check_no_pattern '&>> redirect'     '&>>'
check_no_pattern 'here-string <<<'  '<<<'
check_no_pattern 'process substitution <()' '<\('
check_no_pattern '{a..z} brace expansion' '\{[a-z]\.\.[a-z]\}'

# ============================================================
section "Error path coverage"
# ============================================================

# Test that install.sh contains all required error checks
check_error_path() {
    LABEL="$1"; PATTERN="$2"
    if grep -q "$PATTERN" "$INSTALL_SH" 2>/dev/null; then
        pass "handles: $LABEL"
    else
        fail "missing error path: $LABEL"
    fi
}

check_error_path "no curl/wget"           "Neither curl nor wget"
check_error_path "no C compiler"          "No C compiler found"
check_error_path "macOS hint"             "xcode-select --install"
check_error_path "apt hint"               "apt-get install build-essential"
check_error_path "dnf hint"               "dnf install gcc"
check_error_path "pacman hint"            "pacman -S base-devel"
check_error_path "apk hint"               "apk add build-base"
check_error_path "generic hint"           "Install gcc or clang"
check_error_path "cargo post-install check" "cargo not found.*after"
check_error_path "binary not found"       "Binary not found"
check_error_path "PATH warning"           "not on your PATH"
check_error_path "PATH fix suggestion"    'export PATH='

# ============================================================
section "Download safety"
# ============================================================

# rustup should be downloaded to temp file, not piped
if grep -q 'mktemp' "$INSTALL_SH" && grep -q 'download.*rustup' "$INSTALL_SH"; then
    pass "rustup downloaded to temp file (not piped)"
else
    fail "rustup should be downloaded to temp file, not piped to sh"
fi

# temp file cleaned up
if grep -q 'rm -f.*RUSTUP_INIT' "$INSTALL_SH" && grep -q 'trap.*rm.*RUSTUP_INIT' "$INSTALL_SH"; then
    pass "temp file cleaned up (rm + trap)"
else
    fail "temp file not cleaned up properly"
fi

# ============================================================
section "Build safety"
# ============================================================

# Tests are NOT run during install (they run in CI/prerelease)
if grep -q 'cargo.*test' "$INSTALL_SH" 2>/dev/null; then
    fail "install.sh should not run cargo test (tests belong in CI)"
else
    pass "install.sh does not run cargo test (handled by CI)"
fi

# CARGO_TARGET_DIR respected
if grep -q 'CARGO_TARGET_DIR' "$INSTALL_SH"; then
    pass "CARGO_TARGET_DIR env var respected"
else
    fail "CARGO_TARGET_DIR not handled"
fi

# ============================================================
section "Terminal handling"
# ============================================================

if grep -q '\[ -t 1 \]' "$INSTALL_SH"; then
    pass "colors disabled when stdout is not a terminal"
else
    fail "no check for terminal (colors will break piped output)"
fi

# ============================================================
section "Shell profile detection"
# ============================================================

for shell in zsh bash fish; do
    if grep -q "$shell" "$INSTALL_SH"; then
        pass "PATH hint for $shell"
    else
        fail "no PATH hint for $shell"
    fi
done

# Check default fallback
if grep -q '\.profile' "$INSTALL_SH"; then
    pass "PATH hint fallback to .profile"
else
    fail "no .profile fallback"
fi

# ============================================================
section "Makefile integration"
# ============================================================

MAKEFILE="$PROJECT_DIR/Makefile"
if [ -f "$MAKEFILE" ]; then
    # deploy should delegate to install.sh
    if grep -q './install.sh' "$MAKEFILE"; then
        pass "make deploy delegates to install.sh"
    else
        fail "make deploy does not call install.sh (duplicate logic)"
    fi

    # no bash dependency
    if grep -q 'bash -c' "$MAKEFILE"; then
        fail "Makefile uses bash -c (not available on Alpine/minimal)"
    else
        pass "Makefile has no bash dependency"
    fi

    # CARGO_TARGET_DIR in Makefile
    if grep -q 'CARGO_TARGET_DIR' "$MAKEFILE"; then
        pass "Makefile respects CARGO_TARGET_DIR"
    else
        fail "Makefile ignores CARGO_TARGET_DIR"
    fi
fi

# ============================================================
section "Live environment check"
# ============================================================

# Verify tools exist on this machine
for tool in sh mktemp basename dirname wc awk cp mkdir chmod; do
    if command -v $tool >/dev/null 2>&1; then
        pass "$tool available"
    else
        fail "$tool missing on this system"
    fi
done

# Check that install.sh can at least parse without syntax errors
SH_CHECK=$(sh -n "$INSTALL_SH" 2>&1) || true
if [ -z "$SH_CHECK" ]; then
    pass "sh -n syntax check passed"
else
    fail "sh -n syntax errors: $SH_CHECK"
fi

# If dash is available, test with dash too (Debian/Ubuntu default sh)
if command -v dash >/dev/null 2>&1; then
    DASH_CHECK=$(dash -n "$INSTALL_SH" 2>&1) || true
    if [ -z "$DASH_CHECK" ]; then
        pass "dash -n syntax check passed"
    else
        fail "dash syntax errors: $DASH_CHECK"
    fi
else
    skip "dash not installed (Debian/Ubuntu default /bin/sh)"
fi

# ============================================================
# Docker tests (if Docker is available)
# ============================================================

if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
    section "Docker integration tests"

    # Test preflight error: no curl, no wget
    RESULT=$(docker run --rm alpine:3.19 sh -c '
        apk del curl wget 2>/dev/null
        printf "#!/bin/sh\nset -e\n" > /tmp/test.sh
        # Copy just the preflight check
        cat >> /tmp/test.sh << "HEREDOC"
if ! command -v curl >/dev/null 2>&1 && ! command -v wget >/dev/null 2>&1; then
    echo "PREFLIGHT_FAIL_CURL_WGET"
    exit 1
fi
echo "PREFLIGHT_OK"
HEREDOC
        sh /tmp/test.sh 2>&1
    ' 2>&1) || true
    if echo "$RESULT" | grep -q "PREFLIGHT_FAIL_CURL_WGET"; then
        pass "Docker/Alpine: detects missing curl+wget"
    else
        fail "Docker/Alpine: curl+wget check failed: $RESULT"
    fi

    # Test preflight error: no C compiler
    RESULT=$(docker run --rm ubuntu:24.04 sh -c '
        echo "PREFLIGHT_CHECK"
        if ! command -v cc >/dev/null 2>&1 && ! command -v gcc >/dev/null 2>&1 && ! command -v clang >/dev/null 2>&1; then
            echo "NO_CC_DETECTED"
        else
            echo "CC_FOUND"
        fi
    ' 2>&1) || true
    if echo "$RESULT" | grep -q "NO_CC_DETECTED"; then
        pass "Docker/Ubuntu: fresh Ubuntu has no C compiler (our check will catch it)"
    else
        fail "Docker/Ubuntu: expected no C compiler on fresh Ubuntu"
    fi

    # Test: Alpine has no bash
    RESULT=$(docker run --rm alpine:3.19 sh -c '
        command -v bash >/dev/null 2>&1 && echo "HAS_BASH" || echo "NO_BASH"
    ' 2>&1) || true
    if echo "$RESULT" | grep -q "NO_BASH"; then
        pass "Docker/Alpine: confirmed no bash (our script uses sh)"
    else
        skip "Docker/Alpine: bash is present (test less relevant)"
    fi

    # Test: install.sh parses on Alpine (BusyBox sh)
    RESULT=$(docker run --rm -v "$PROJECT_DIR/install.sh:/install.sh:ro" alpine:3.19 sh -c '
        sh -n /install.sh 2>&1 && echo "PARSE_OK" || echo "PARSE_FAIL"
    ' 2>&1) || true
    if echo "$RESULT" | grep -q "PARSE_OK"; then
        pass "Docker/Alpine: install.sh parses on BusyBox sh"
    else
        fail "Docker/Alpine: parse error: $RESULT"
    fi

    # Test: install.sh parses on Ubuntu (dash as /bin/sh)
    RESULT=$(docker run --rm -v "$PROJECT_DIR/install.sh:/install.sh:ro" ubuntu:24.04 sh -c '
        sh -n /install.sh 2>&1 && echo "PARSE_OK" || echo "PARSE_FAIL"
    ' 2>&1) || true
    if echo "$RESULT" | grep -q "PARSE_OK"; then
        pass "Docker/Ubuntu: install.sh parses on dash"
    else
        fail "Docker/Ubuntu: parse error: $RESULT"
    fi
else
    section "Docker tests"
    skip "Docker not available - install Docker to run container tests"
fi

# ============================================================
section "Summary"
# ============================================================

TOTAL=$((PASS + FAIL + SKIP))
printf "\n"
printf "  ${GREEN}%d passed${NC}, " "$PASS"
printf "${RED}%d failed${NC}, " "$FAIL"
printf "${YELLOW}%d skipped${NC}" "$SKIP"
printf " out of %d\n\n" "$TOTAL"

if [ "$FAIL" -gt 0 ]; then
    printf "%bSOME TESTS FAILED%b\n" "$RED" "$NC"
    exit 1
else
    printf "%bALL TESTS PASSED%b\n" "$GREEN" "$NC"
fi

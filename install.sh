#!/bin/sh
set -e

# Qartez MCP - zero-dependency installer
# Works on macOS (arm64/x86_64) and Linux (amd64/arm64/riscv64)
# Only needs: curl or wget
#
# Usage:
#   curl -sSfL https://qartez.dev/install | sh
#
# Or from a checked-out repo:
#   ./install.sh
#
# In curl|sh mode, the script downloads the latest source tarball into a
# temp directory and builds from there.

QARTEZ_REPO="kuberstar/qartez-mcp"
QARTEZ_BRANCH="main"
INSTALL_DIR="${HOME}/.local/bin"
SCRIPT_DIR="$(cd "$(dirname "$0")" 2>/dev/null && pwd || true)"

if [ -t 1 ]; then
    GREEN='\033[0;32m'; BLUE='\033[1;34m'; RED='\033[1;31m'; YELLOW='\033[1;33m'; NC='\033[0m'
else
    GREEN=''; BLUE=''; RED=''; YELLOW=''; NC=''
fi

info()  { printf "${BLUE}==>${NC} %s\n" "$1"; }
ok()    { printf "${GREEN}[+]${NC} %s\n" "$1"; }
warn()  { printf "${YELLOW}[!]${NC} %s\n" "$1"; }
err()   { printf "${RED}[!]${NC} %s\n" "$1" >&2; }

# --- Preflight checks ---
if ! command -v curl >/dev/null 2>&1 && ! command -v wget >/dev/null 2>&1; then
    err "Neither curl nor wget found. Install one of them first."
    exit 1
fi

if ! command -v cc >/dev/null 2>&1 && ! command -v gcc >/dev/null 2>&1 && ! command -v clang >/dev/null 2>&1; then
    err "No C compiler found (cc, gcc, or clang)."
    err "Rust needs a linker to build. Install one first:"
    case "$(uname)" in
        Darwin) err "  xcode-select --install" ;;
        *)
            if command -v apt-get >/dev/null 2>&1; then
                err "  sudo apt-get install build-essential"
            elif command -v dnf >/dev/null 2>&1; then
                err "  sudo dnf install gcc"
            elif command -v pacman >/dev/null 2>&1; then
                err "  sudo pacman -S base-devel"
            elif command -v apk >/dev/null 2>&1; then
                err "  sudo apk add build-base"
            else
                err "  Install gcc or clang via your package manager"
            fi
            ;;
    esac
    exit 1
fi

download() {
    if command -v curl >/dev/null 2>&1; then
        curl -sSfL -o "$2" "$1"
    else
        wget -qO "$2" "$1"
    fi
}

# --- Source acquisition (curl|sh mode) ---
# When invoked via `curl ... | sh`, $0 is "sh" and SCRIPT_DIR has no Cargo.toml.
# Download the source tarball into a temp dir and build from there.
if [ -z "$SCRIPT_DIR" ] || [ ! -f "${SCRIPT_DIR}/Cargo.toml" ]; then
    if ! command -v tar >/dev/null 2>&1; then
        err "tar not found - required to extract source tarball."
        exit 1
    fi
    info "Source not found locally - downloading from github.com/${QARTEZ_REPO}..."
    QARTEZ_TMPDIR="$(mktemp -d)"
    trap 'rm -rf "$QARTEZ_TMPDIR"' EXIT INT TERM
    download "https://codeload.github.com/${QARTEZ_REPO}/tar.gz/refs/heads/${QARTEZ_BRANCH}" "${QARTEZ_TMPDIR}/qartez.tar.gz"
    tar -xzf "${QARTEZ_TMPDIR}/qartez.tar.gz" -C "$QARTEZ_TMPDIR"
    SCRIPT_DIR="${QARTEZ_TMPDIR}/qartez-mcp-${QARTEZ_BRANCH}"
    if [ ! -f "${SCRIPT_DIR}/Cargo.toml" ]; then
        err "Tarball layout unexpected: ${SCRIPT_DIR}/Cargo.toml not found"
        exit 1
    fi
    ok "Source extracted to ${SCRIPT_DIR}"
fi

# --- Rust ---
if command -v cargo >/dev/null 2>&1; then
    CARGO="$(command -v cargo)"
elif [ -x "${HOME}/.cargo/bin/cargo" ]; then
    CARGO="${HOME}/.cargo/bin/cargo"
else
    info "Rust not found. Installing via rustup..."
    RUSTUP_INIT="$(mktemp)"
    trap 'rm -f "$RUSTUP_INIT"' EXIT
    download https://sh.rustup.rs "$RUSTUP_INIT"
    sh "$RUSTUP_INIT" -y
    rm -f "$RUSTUP_INIT"
    trap - EXIT
    CARGO="${HOME}/.cargo/bin/cargo"
    if ! [ -x "$CARGO" ]; then
        err "cargo not found at $CARGO after rustup install."
        exit 1
    fi
    ok "Rust installed."
fi

# --- Build ---
cd "$SCRIPT_DIR"
info "Building release binaries (this may take a few minutes on first run)..."
"$CARGO" build --release

# --- Install ---
TARGET_DIR="${CARGO_TARGET_DIR:-${SCRIPT_DIR}/target}"
mkdir -p "$INSTALL_DIR"
for bin in qartez qartez-guard qartez-setup; do
    if ! [ -f "${TARGET_DIR}/release/${bin}" ]; then
        err "Binary not found: ${TARGET_DIR}/release/${bin}"
        exit 1
    fi
    # Atomic install: copy to .new, then rename. mv replaces the inode so a
    # running process keeps the old binary mapped via its open fd while new
    # invocations get the fresh one - avoids ETXTBSY and corrupted overwrites.
    cp "${TARGET_DIR}/release/${bin}" "${INSTALL_DIR}/${bin}.new"
    if [ "$(uname)" = "Darwin" ]; then
        codesign -s - -f "${INSTALL_DIR}/${bin}.new" 2>/dev/null || true
    fi
    mv -f "${INSTALL_DIR}/${bin}.new" "${INSTALL_DIR}/${bin}"
    SIZE=$(wc -c < "${TARGET_DIR}/release/${bin}" | awk '{printf "%.1f MB", $1/1048576}')
    ok "Installed: ${INSTALL_DIR}/${bin} (${SIZE})"
done
ln -sf qartez "${INSTALL_DIR}/qartez-mcp"
ok "Symlink: ${INSTALL_DIR}/qartez-mcp -> qartez"

# --- Configure IDEs ---
case "${1:-}" in
    --interactive)
        info "Launching interactive IDE setup..."
        "${INSTALL_DIR}/qartez-setup"
        ;;
    --skip-setup)
        info "Skipping IDE setup (--skip-setup)."
        ;;
    *)
        info "Configuring all detected IDEs..."
        "${INSTALL_DIR}/qartez-setup" --yes
        ;;
esac

ok "Deploy complete. Restart your IDEs to pick up MCP changes."

# --- PATH check ---
case ":${PATH}:" in
    *":${INSTALL_DIR}:"*) ;;
    *)
        warn "${INSTALL_DIR} is not on your PATH."
        SHELL_NAME="$(basename "${SHELL:-/bin/sh}")"
        case "$SHELL_NAME" in
            zsh)  PROFILE="\$HOME/.zshrc" ;;
            bash) PROFILE="\$HOME/.bashrc" ;;
            fish) PROFILE="\$HOME/.config/fish/config.fish" ;;
            *)    PROFILE="\$HOME/.profile" ;;
        esac
        warn "Add to ${PROFILE}:"
        warn "  export PATH=\"\$HOME/.local/bin:\$PATH\""
        ;;
esac

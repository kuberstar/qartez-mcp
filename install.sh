#!/bin/sh
set -e

# Qartez MCP - zero-dependency installer
# Works on macOS (arm64/x86_64) and Linux (x86_64/aarch64, gnu or musl).
# Only needs: curl or wget, plus tar.
#
# Usage:
#   curl -sSfL https://qartez.dev/install | sh
#
# Or from a checked-out repo:
#   ./install.sh
#
# By default this installer downloads a pre-built release binary for the
# current platform. If no matching asset exists, or --from-source is passed,
# it falls back to building from source with cargo.

QARTEZ_REPO="kuberstar/qartez-mcp"
QARTEZ_BRANCH="main"
INSTALL_DIR="${HOME}/.local/bin"
SCRIPT_DIR="$(cd "$(dirname "$0")" 2>/dev/null && pwd)" || SCRIPT_DIR=""

if [ -t 1 ]; then
    GREEN='\033[0;32m'; BLUE='\033[1;34m'; RED='\033[1;31m'; YELLOW='\033[1;33m'; NC='\033[0m'
else
    GREEN=''; BLUE=''; RED=''; YELLOW=''; NC=''
fi

info()  { printf "${BLUE}==>${NC} %s\n" "$1"; }
ok()    { printf "${GREEN}[+]${NC} %s\n" "$1"; }
warn()  { printf "${YELLOW}[!]${NC} %s\n" "$1"; }
err()   { printf "${RED}[!]${NC} %s\n" "$1" >&2; }

# --- Argument parsing ---
FROM_SOURCE=0
SETUP_MODE="yes"
for arg in "$@"; do
    case "$arg" in
        --from-source) FROM_SOURCE=1 ;;
        --interactive) SETUP_MODE="interactive" ;;
        --skip-setup)  SETUP_MODE="skip" ;;
    esac
done

# --- Preflight: network client ---
if ! command -v curl >/dev/null 2>&1 && ! command -v wget >/dev/null 2>&1; then
    err "Neither curl nor wget found. Install one of them first."
    exit 1
fi

download() {
    if command -v curl >/dev/null 2>&1; then
        curl -sSfL -o "$2" "$1"
    else
        wget -qO "$2" "$1"
    fi
}

# stdout-capturing fetch for small API responses and SHA files.
fetch_stdout() {
    if command -v curl >/dev/null 2>&1; then
        curl -sSfL "$1"
    else
        wget -qO - "$1"
    fi
}

# --- Platform detection ---
# Returns the Rust target triple for the running host, or empty if unknown.
detect_target() {
    uname_s=$(uname -s 2>/dev/null || echo unknown)
    uname_m=$(uname -m 2>/dev/null || echo unknown)
    case "$uname_s" in
        Darwin)
            case "$uname_m" in
                arm64|aarch64) echo "aarch64-apple-darwin" ;;
                x86_64)        echo "x86_64-apple-darwin" ;;
                *) echo "" ;;
            esac
            ;;
        Linux)
            libc="gnu"
            if command -v ldd >/dev/null 2>&1; then
                if ldd --version 2>&1 | grep -qi musl; then
                    libc="musl"
                fi
            elif [ -f /etc/alpine-release ]; then
                libc="musl"
            fi
            case "${uname_m}-${libc}" in
                x86_64-gnu|amd64-gnu)   echo "x86_64-unknown-linux-gnu" ;;
                x86_64-musl|amd64-musl) echo "x86_64-unknown-linux-musl" ;;
                aarch64-gnu|arm64-gnu)   echo "aarch64-unknown-linux-gnu" ;;
                aarch64-musl|arm64-musl) echo "aarch64-unknown-linux-musl" ;;
                *) echo "" ;;
            esac
            ;;
        *)
            echo ""
            ;;
    esac
}

# --- Binary install from release archive ---
# Atomic: extract to temp, then `mv` onto the final path so concurrent
# invocations never observe a half-written binary.
install_binary() {
    src="$1"; dst="$2"
    cp "$src" "${dst}.new"
    if [ "$(uname)" = "Darwin" ]; then
        codesign -s - -f "${dst}.new" 2>/dev/null || true
    fi
    chmod +x "${dst}.new"
    mv -f "${dst}.new" "$dst"
}

install_from_prebuilt() {
    target="$1"

    if ! command -v tar >/dev/null 2>&1; then
        warn "tar not found - cannot extract release archive, falling back to source build."
        return 1
    fi

    sha_cmd=""
    if command -v sha256sum >/dev/null 2>&1; then
        sha_cmd="sha256sum"
    elif command -v shasum >/dev/null 2>&1; then
        sha_cmd="shasum -a 256"
    else
        warn "Neither sha256sum nor shasum found - cannot verify checksum, falling back to source build."
        return 1
    fi

    info "Resolving latest release tag for ${QARTEZ_REPO}..."
    api_json=$(fetch_stdout "https://api.github.com/repos/${QARTEZ_REPO}/releases/latest" 2>/dev/null) || api_json=""
    tag=$(printf '%s' "$api_json" | sed -n 's/.*"tag_name" *: *"\([^"]*\)".*/\1/p' | head -n 1)
    if [ -z "$tag" ]; then
        warn "Could not resolve latest release tag (network error or API rate limit)."
        return 1
    fi
    version=${tag#v}
    ok "Latest release: ${tag}"

    archive="qartez-${version}-${target}.tar.xz"
    base_url="https://github.com/${QARTEZ_REPO}/releases/download/${tag}"
    archive_url="${base_url}/${archive}"
    sums_url="${base_url}/SHA256SUMS"

    tmpdir=$(mktemp -d 2>/dev/null || mktemp -d -t qartez-install)
    # shellcheck disable=SC2064
    trap "rm -rf \"$tmpdir\"" EXIT INT TERM

    info "Downloading ${archive}..."
    if ! download "$archive_url" "${tmpdir}/${archive}" 2>/dev/null; then
        warn "No pre-built binary for target ${target} at ${tag} - falling back to source build."
        return 1
    fi

    info "Verifying checksum..."
    if ! download "$sums_url" "${tmpdir}/SHA256SUMS" 2>/dev/null; then
        warn "SHA256SUMS not available at ${tag} - falling back to source build."
        return 1
    fi

    expected=$(grep -E "  ${archive}\$" "${tmpdir}/SHA256SUMS" | awk '{print $1}')
    if [ -z "$expected" ]; then
        warn "Checksum for ${archive} missing from SHA256SUMS - falling back to source build."
        return 1
    fi

    actual=$(cd "$tmpdir" && $sha_cmd "$archive" | awk '{print $1}')
    if [ "$expected" != "$actual" ]; then
        err "Checksum mismatch for ${archive}:"
        err "  expected: $expected"
        err "  actual:   $actual"
        err "Refusing to install. This can indicate a corrupted download"
        err "or a tampered release asset. Re-run to retry, or pass"
        err "--from-source to build from source instead."
        exit 1
    fi
    ok "Checksum verified"

    info "Extracting archive..."
    (cd "$tmpdir" && tar -xJf "$archive")
    extract_dir="${tmpdir}/qartez-${version}-${target}"
    if [ ! -d "$extract_dir" ]; then
        err "Archive layout unexpected: ${extract_dir} not found"
        return 1
    fi

    mkdir -p "$INSTALL_DIR"
    for bin in qartez qartez-guard qartez-setup; do
        if [ ! -f "${extract_dir}/${bin}" ]; then
            err "Missing binary in archive: ${bin}"
            return 1
        fi
        install_binary "${extract_dir}/${bin}" "${INSTALL_DIR}/${bin}"
        size=$(wc -c < "${extract_dir}/${bin}" | awk '{printf "%.1f MB", $1/1048576}')
        ok "Installed: ${INSTALL_DIR}/${bin} (${size})"
    done
    ln -sf qartez "${INSTALL_DIR}/qartez-mcp"
    ok "Symlink: ${INSTALL_DIR}/qartez-mcp -> qartez"

    trap - EXIT INT TERM
    rm -rf "$tmpdir"
    return 0
}

# --- Source acquisition (curl|sh mode) ---
# When invoked via `curl ... | sh`, $0 is "sh" and SCRIPT_DIR has no Cargo.toml.
# Download the source tarball into a temp dir and build from there.
fetch_source_tarball() {
    if ! command -v tar >/dev/null 2>&1; then
        err "tar not found - required to extract source tarball."
        return 1
    fi
    info "Source not found locally - downloading from github.com/${QARTEZ_REPO}..."
    QARTEZ_TMPDIR="$(mktemp -d)"
    # shellcheck disable=SC2064
    trap "rm -rf \"$QARTEZ_TMPDIR\"" EXIT INT TERM
    download "https://codeload.github.com/${QARTEZ_REPO}/tar.gz/refs/heads/${QARTEZ_BRANCH}" "${QARTEZ_TMPDIR}/qartez.tar.gz" || return 1
    tar -xzf "${QARTEZ_TMPDIR}/qartez.tar.gz" -C "$QARTEZ_TMPDIR" || return 1
    SCRIPT_DIR="${QARTEZ_TMPDIR}/qartez-mcp-${QARTEZ_BRANCH}"
    if [ ! -f "${SCRIPT_DIR}/Cargo.toml" ]; then
        err "Tarball layout unexpected: ${SCRIPT_DIR}/Cargo.toml not found"
        return 1
    fi
    ok "Source extracted to ${SCRIPT_DIR}"
}

run_setup() {
    case "$SETUP_MODE" in
        interactive)
            info "Launching interactive IDE setup..."
            "${INSTALL_DIR}/qartez-setup"
            ;;
        skip)
            info "Skipping IDE setup (--skip-setup)."
            ;;
        *)
            info "Configuring all detected IDEs..."
            "${INSTALL_DIR}/qartez-setup" --yes
            ;;
    esac
}

warn_path() {
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
}

# --- Pre-built fast path ---
# Running from a checked-out repo with `./install.sh` always builds from
# source - that is the dev workflow, and downloading pre-builts would mask
# the local tree. For remote `curl|sh` invocations, try the pre-built
# archive first and fall through to source if anything goes wrong.
LOCAL_REPO=0
if [ -n "$SCRIPT_DIR" ] && [ -f "${SCRIPT_DIR}/Cargo.toml" ]; then
    LOCAL_REPO=1
fi

if [ "$FROM_SOURCE" -eq 0 ] && [ "$LOCAL_REPO" -eq 0 ]; then
    TARGET=$(detect_target)
    if [ -z "$TARGET" ]; then
        warn "Could not detect a supported target triple for this platform - falling back to source build."
    else
        info "Detected target: ${TARGET}"
        if install_from_prebuilt "$TARGET"; then
            run_setup
            ok "Deploy complete. Restart your IDEs to pick up MCP changes."
            warn_path
            exit 0
        fi
    fi
else
    if [ "$FROM_SOURCE" -eq 1 ]; then
        info "Forced source build (--from-source)."
    fi
fi

# --- Source build path (fallback and local dev) ---
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

if [ "$LOCAL_REPO" -eq 0 ]; then
    fetch_source_tarball || { err "Failed to download source tarball."; exit 1; }
fi

# --- Rust ---
# Must match `rust-version` in Cargo.toml. Edition 2024 needs >= 1.85.
RUST_MIN="1.88.0"

if command -v cargo >/dev/null 2>&1; then
    CARGO="$(command -v cargo)"
elif [ -x "${HOME}/.cargo/bin/cargo" ]; then
    CARGO="${HOME}/.cargo/bin/cargo"
else
    info "Rust not found. Installing via rustup..."
    RUSTUP_INIT="$(mktemp)"
    # shellcheck disable=SC2064
    trap "rm -f \"$RUSTUP_INIT\"" EXIT
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

# Version check: catch old rustc before cargo emits cryptic feature-gate errors.
# `rustc --version` output: "rustc 1.88.0 (abc 2025-06-26)"
RUSTC_BIN="$(dirname "$CARGO")/rustc"
[ -x "$RUSTC_BIN" ] || RUSTC_BIN="rustc"
if command -v "$RUSTC_BIN" >/dev/null 2>&1; then
    RUSTC_VER="$("$RUSTC_BIN" --version 2>/dev/null | awk '{print $2}' | cut -d- -f1)"
else
    RUSTC_VER=""
fi

if [ -n "$RUSTC_VER" ]; then
    OLDEST="$(printf '%s\n%s\n' "$RUST_MIN" "$RUSTC_VER" | sort -V | head -n 1)"
    if [ "$OLDEST" != "$RUST_MIN" ]; then
        warn "Rust ${RUSTC_VER} is older than the required ${RUST_MIN}."
        if command -v rustup >/dev/null 2>&1; then
            info "Updating Rust toolchain via rustup..."
            rustup update stable
            rustup default stable >/dev/null 2>&1 || true
            RUSTC_VER_NEW="$("$RUSTC_BIN" --version 2>/dev/null | awk '{print $2}' | cut -d- -f1)"
            OLDEST_NEW="$(printf '%s\n%s\n' "$RUST_MIN" "$RUSTC_VER_NEW" | sort -V | head -n 1)"
            if [ "$OLDEST_NEW" != "$RUST_MIN" ]; then
                err "Rust is still ${RUSTC_VER_NEW} after update. Minimum required: ${RUST_MIN}."
                err "Your stable channel may be pinned. Try: rustup default stable && rustup update"
                exit 1
            fi
            ok "Rust updated to ${RUSTC_VER_NEW}."
        else
            err "Rust ${RUSTC_VER} is too old. qartez-mcp requires >= ${RUST_MIN}."
            err "Your rustc was not installed via rustup, so we cannot auto-update it."
            err "Options:"
            err "  1. Install rustup and retry:  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
            err "  2. Upgrade Rust via your OS package manager to >= ${RUST_MIN}"
            exit 1
        fi
    fi
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
run_setup

ok "Deploy complete. Restart your IDEs to pick up MCP changes."

# --- PATH check ---
warn_path

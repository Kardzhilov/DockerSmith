#!/bin/sh
# DockerSmith installer.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/Kardzhilov/DockerSmith/main/install.sh | sh
#
# Environment overrides:
#   DOCKERSMITH_INSTALL_DIR   where to install the binary (default: ~/.local/bin)
#   DOCKERSMITH_VERSION       release tag to install, e.g. v1.2.0 (default: latest)

set -eu

REPO="Kardzhilov/DockerSmith"
BIN="dockersmith"

# ── Pretty output ───────────────────────────────────────────────────────────
if [ -t 1 ]; then
    BOLD="$(printf '\033[1m')"; DIM="$(printf '\033[2m')"
    GREEN="$(printf '\033[32m')"; RED="$(printf '\033[31m')"
    YELLOW="$(printf '\033[33m')"; NC="$(printf '\033[0m')"
else
    BOLD=""; DIM=""; GREEN=""; RED=""; YELLOW=""; NC=""
fi
info() { printf '%s\n' "$*"; }
ok()   { printf '%s✓%s %s\n' "$GREEN" "$NC" "$*"; }
warn() { printf '%s!%s %s\n' "$YELLOW" "$NC" "$*"; }
err()  { printf '%s✗ %s%s\n' "$RED" "$*" "$NC" >&2; exit 1; }

# ── Detect platform ─────────────────────────────────────────────────────────
os="$(uname -s)"
[ "$os" = "Linux" ] || err "Only Linux is supported right now (detected: $os)."

arch="$(uname -m)"
case "$arch" in
    x86_64 | amd64)  target="x86_64-unknown-linux-gnu" ;;
    aarch64 | arm64) target="aarch64-unknown-linux-gnu" ;;
    *) err "Unsupported architecture: $arch" ;;
esac

# ── Pick a downloader ───────────────────────────────────────────────────────
if command -v curl >/dev/null 2>&1; then
    DL="curl"
elif command -v wget >/dev/null 2>&1; then
    DL="wget"
else
    err "Neither curl nor wget is available; please install one and retry."
fi

fetch() { # fetch <url> <output-file>
    if [ "$DL" = "curl" ]; then
        curl -fSL --progress-bar "$1" -o "$2"
    else
        wget -q --show-progress -O "$2" "$1"
    fi
}

# ── Resolve the release tag ─────────────────────────────────────────────────
if [ -n "${DOCKERSMITH_VERSION:-}" ]; then
    tag="$DOCKERSMITH_VERSION"
elif [ "$DL" = "curl" ]; then
    # Follow the "latest" redirect and read the resolved tag from the final URL.
    tag="$(curl -fsSLI -o /dev/null -w '%{url_effective}' \
        "https://github.com/${REPO}/releases/latest" | sed 's#.*/##')"
else
    tag="$(wget -qO- "https://api.github.com/repos/${REPO}/releases/latest" \
        | grep -oE '"tag_name":[^,]*' | head -1 | sed -E 's/.*"([^"]+)".*/\1/')"
fi
[ -n "$tag" ] || err "Could not determine the latest release version."

# ── Resolve download URL ────────────────────────────────────────────────────
# Assets are named  dockersmith-<target>-<tag>  (version at the end).
asset="${BIN}-${target}-${tag}"
url="https://github.com/${REPO}/releases/download/${tag}/${asset}"

info "${BOLD}Installing DockerSmith ${tag}${NC} ${DIM}(${target})${NC}"

# ── Download ────────────────────────────────────────────────────────────────
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT INT TERM
fetch "$url" "$tmp" || err "Download failed. No prebuilt binary at ${url}"

[ -s "$tmp" ] || err "Downloaded file is empty."

# ── Install ─────────────────────────────────────────────────────────────────
install_dir="${DOCKERSMITH_INSTALL_DIR:-$HOME/.local/bin}"
mkdir -p "$install_dir"
chmod +x "$tmp"
mv "$tmp" "$install_dir/$BIN"
trap - EXIT INT TERM
ok "Installed to ${BOLD}${install_dir}/${BIN}${NC}"

# ── Ensure it's on PATH ─────────────────────────────────────────────────────
add_path_line() {
    # $1 = rc file, $2 = line to append (idempotent on the install dir).
    rc="$1"; line="$2"
    [ -f "$rc" ] || : > "$rc"
    if ! grep -qsF "$install_dir" "$rc"; then
        printf '\n# Added by the DockerSmith installer\n%s\n' "$line" >> "$rc"
        ok "Added ${install_dir} to PATH in ${DIM}${rc}${NC}"
        RC_UPDATED="$rc"
    fi
}

RC_UPDATED=""
case ":$PATH:" in
    *":$install_dir:"*)
        ok "${install_dir} is already on your PATH"
        ;;
    *)
        shell_name="$(basename "${SHELL:-sh}")"
        case "$shell_name" in
            bash) add_path_line "$HOME/.bashrc" "export PATH=\"$install_dir:\$PATH\"" ;;
            zsh)  add_path_line "${ZDOTDIR:-$HOME}/.zshrc" "export PATH=\"$install_dir:\$PATH\"" ;;
            fish)
                fish_rc="$HOME/.config/fish/config.fish"
                mkdir -p "$(dirname "$fish_rc")"
                add_path_line "$fish_rc" "fish_add_path $install_dir"
                ;;
            *) add_path_line "$HOME/.profile" "export PATH=\"$install_dir:\$PATH\"" ;;
        esac
        ;;
esac

# ── Verify + next steps ─────────────────────────────────────────────────────
info ""
if "$install_dir/$BIN" --version >/dev/null 2>&1; then
    ok "$("$install_dir/$BIN" --version)"
else
    warn "Installed, but the binary did not run — your glibc may be older than the build host's."
fi

info ""
info "${BOLD}Done!${NC}"
if [ -n "$RC_UPDATED" ]; then
    info "Restart your shell or run: ${BOLD}source $RC_UPDATED${NC}"
fi
info "Then launch it with: ${BOLD}${BIN}${NC}"

#!/usr/bin/env bash
# Organon setup script
# Installs dependencies, builds the binary, and wires up PATH.
set -euo pipefail

# ── colours ───────────────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'
BOLD='\033[1m'; RESET='\033[0m'

ok()   { echo -e "${GREEN}✓${RESET} $*"; }
warn() { echo -e "${YELLOW}!${RESET} $*"; }
err()  { echo -e "${RED}✗${RESET} $*" >&2; }
step() { echo -e "\n${BOLD}▶ $*${RESET}"; }

ORGANON_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BINARY="$ORGANON_ROOT/target/release/organon"
INSTALL_DIR="$HOME/.local/bin"

# ── 1. System checks ──────────────────────────────────────────────────────────
step "Checking system requirements"

check_cmd() {
    if command -v "$1" &>/dev/null; then
        ok "$1 found ($(command -v "$1"))"
        return 0
    else
        return 1
    fi
}

# Rust / cargo
if ! check_cmd cargo; then
    warn "cargo not found — installing Rust via rustup..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path
    # shellcheck source=/dev/null
    source "$HOME/.cargo/env"
    ok "Rust installed ($(rustc --version))"
else
    ok "Rust $(rustc --version)"
fi

# uv (Python package manager)
if ! check_cmd uv; then
    warn "uv not found — installing..."
    curl -LsSf https://astral.sh/uv/install.sh | sh
    export PATH="$HOME/.local/bin:$PATH"
    ok "uv installed ($(uv --version))"
else
    ok "uv $(uv --version)"
fi

# ollama (optional — for NL query feature)
if check_cmd ollama; then
    ok "ollama found — NL query feature available"
else
    warn "ollama not found — NL query will use fallback mode"
    warn "Install from https://ollama.com then: ollama pull llama3.2"
fi

# ── 2. Python dependencies ────────────────────────────────────────────────────
step "Installing Python dependencies"
cd "$ORGANON_ROOT"
uv sync --quiet
ok "Python deps ready (.venv)"

# ── 3. Build Rust binary ──────────────────────────────────────────────────────
step "Building organon (release)"
cargo build --release 2>&1 | grep -E '^(error|warning: unused|   Compiling|    Finished)' || true

if [[ ! -f "$BINARY" ]]; then
    err "Build failed — binary not found at $BINARY"
    exit 1
fi
ok "Built: $BINARY"

# ── 4. Install binary to PATH ─────────────────────────────────────────────────
step "Installing binary to $INSTALL_DIR"
mkdir -p "$INSTALL_DIR"
cp "$BINARY" "$INSTALL_DIR/organon"
chmod +x "$INSTALL_DIR/organon"
ok "Copied to $INSTALL_DIR/organon"

# ── 5. Wire up PATH in shell profile ─────────────────────────────────────────
step "Configuring PATH"

PATH_LINE="export PATH=\"\$HOME/.local/bin:\$PATH\""
PATH_COMMENT="# organon — added by setup.sh"

add_to_profile() {
    local profile="$1"
    if [[ -f "$profile" ]]; then
        if grep -q '.local/bin' "$profile" 2>/dev/null; then
            ok "$profile already has ~/.local/bin in PATH"
        else
            printf '\n%s\n%s\n' "$PATH_COMMENT" "$PATH_LINE" >> "$profile"
            ok "Added PATH to $profile"
        fi
    fi
}

# Detect shell and update appropriate profile
CURRENT_SHELL="$(basename "${SHELL:-bash}")"
case "$CURRENT_SHELL" in
    zsh)
        add_to_profile "$HOME/.zshrc"
        add_to_profile "$HOME/.zprofile"
        ;;
    bash)
        add_to_profile "$HOME/.bashrc"
        add_to_profile "$HOME/.bash_profile"
        ;;
    fish)
        FISH_CONFIG="$HOME/.config/fish/config.fish"
        mkdir -p "$(dirname "$FISH_CONFIG")"
        if [[ -f "$FISH_CONFIG" ]] && grep -q '.local/bin' "$FISH_CONFIG" 2>/dev/null; then
            ok "$FISH_CONFIG already has ~/.local/bin in PATH"
        else
            printf '\n# organon — added by setup.sh\nfish_add_path "$HOME/.local/bin"\n' >> "$FISH_CONFIG"
            ok "Added PATH to $FISH_CONFIG"
        fi
        ;;
    *)
        warn "Unknown shell '$CURRENT_SHELL' — add this to your profile manually:"
        echo "    $PATH_LINE"
        ;;
esac

# Make available in current session immediately
export PATH="$INSTALL_DIR:$PATH"

# ── 6. Smoke test ─────────────────────────────────────────────────────────────
step "Smoke test"

if organon --version &>/dev/null; then
    ok "organon --version → $(organon --version)"
else
    err "organon not found in PATH after setup"
    echo "    Run: export PATH=\"\$HOME/.local/bin:\$PATH\""
    exit 1
fi

# Verify Python layer works
if uv run python -c "from ai.embeddings.store import embed_text; embed_text('test')" &>/dev/null; then
    ok "Python AI layer (embeddings) OK"
else
    warn "Python AI layer check failed — run 'organon index' to diagnose"
fi

# ── 7. Data directory ─────────────────────────────────────────────────────────
step "Preparing data directory"
mkdir -p "$HOME/.organon/archive"
ok "Data dir: ~/.organon/"

# ── 8. Summary ────────────────────────────────────────────────────────────────
echo ""
echo -e "${BOLD}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${RESET}"
echo -e "${GREEN}${BOLD} Organon is ready!${RESET}"
echo -e "${BOLD}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${RESET}"
echo ""
echo "  Quick start:"
echo "    organon watch .              # index current directory"
echo "    organon index                # embed files for semantic search"
echo "    organon search \"your query\" # semantic search"
echo "    organon stats                # show graph stats"
echo "    organon mcp                  # start MCP server for Claude"
echo ""
if ! command -v ollama &>/dev/null; then
    echo -e "  ${YELLOW}Optional:${RESET} install ollama for NL queries"
    echo "    https://ollama.com → ollama pull llama3.2"
    echo ""
fi
echo -e "  ${YELLOW}Note:${RESET} reload your shell or run:"
echo "    export PATH=\"\$HOME/.local/bin:\$PATH\""
echo ""

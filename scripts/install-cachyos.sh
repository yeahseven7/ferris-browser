#!/usr/bin/env bash
# Ferris browser engine — bootstrap + run on CachyOS / Arch Linux.
#
#   ./scripts/install-cachyos.sh            build only
#   ./scripts/install-cachyos.sh --run      build, then serve on :8080
#   PORT=9000 ./scripts/install-cachyos.sh --run
#
# Dependencies are deliberately tiny: rustls+ring means no OpenSSL, and the
# fonts are embedded in the binary, so all we need is a Rust toolchain and a C
# compiler (ring compiles C via `cc`). base-devel provides gcc/make.

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

step() { printf '\n\033[1;36m==> %s\033[0m\n' "$*"; }
warn() { printf '\033[1;33m    %s\033[0m\n' "$*"; }

# --- 1. toolchain -----------------------------------------------------------
if ! command -v cargo >/dev/null 2>&1; then
    step "cargo not found - installing base-devel + rust via pacman"
    sudo pacman -Sy --needed --noconfirm base-devel rust
elif ! command -v cc >/dev/null 2>&1 && ! command -v gcc >/dev/null 2>&1; then
    step "no C compiler found (ring needs one) - installing base-devel"
    sudo pacman -Sy --needed --noconfirm base-devel
else
    step "toolchain present: $(cargo --version 2>/dev/null || true)"
fi

# rustup-managed toolchains are fine too; just make sure cargo is on PATH.
if ! command -v cargo >/dev/null 2>&1; then
    if [[ -x "$HOME/.cargo/bin/cargo" ]]; then
        export PATH="$HOME/.cargo/bin:$PATH"
    else
        echo "cargo still missing; install rustup: https://rustup.rs" >&2
        exit 1
    fi
fi

# --- 2. CachyOS-flavoured optimisations (optional, auto-detected) -----------
# target-cpu=native matches CachyOS's own philosophy of building for your CPU.
# The binary then will NOT run on other CPUs - drop the flag if you care.
RUSTFLAGS="${RUSTFLAGS:--C target-cpu=native}"

# mold makes the link step several times faster; use it if installed.
if ! command -v mold >/dev/null 2>&1; then
    warn "mold not found; 'sudo pacman -S mold' speeds up linking (optional)"
else
    RUSTFLAGS="$RUSTFLAGS -C link-arg=-fuse-ld=mold"
fi

export RUSTFLAGS
step "RUSTFLAGS=$RUSTFLAGS"

# --- 3. build ----------------------------------------------------------------
step "cargo build --release (first run compiles ~200 crates, a few minutes)"
cargo build --release --locked

BIN=./target/release/ferris-browser
step "built: $BIN ($($BIN version))"

# --- 4. run ------------------------------------------------------------------
if [[ "${1:-}" == "--run" ]]; then
    PORT="${PORT:-8080}"
    step "serving on http://localhost:$PORT  (binds 0.0.0.0 - LAN-reachable)"
    warn "Ctrl-C to stop"
    exec "$BIN" serve --port "$PORT"
fi

cat <<'EOF'

Done. Run it with:
    ./target/release/ferris-browser serve --port 8080     # browser UI
    ./target/release/ferris-browser shot <url> -o x.png   # headless render
EOF

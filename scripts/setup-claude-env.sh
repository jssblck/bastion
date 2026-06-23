#!/usr/bin/env bash
set -euo pipefail

# setup-claude-env.sh
#
# Provision a fresh Claude Code cloud environment for bastion.
# Targets a Debian/Ubuntu Linux container that starts with nothing installed.
# Idempotent: safe to re-run. Invoke as: ./scripts/setup-claude-env.sh
#
# What it does:
#   - installs build tooling (git, build-essential, pkg-config)
#   - installs the Rust stable toolchain
#   - installs `just` (the canonical task runner; `just check` runs the gate)
#   - fetches dependencies and warms the build cache
#
# Optional extras are best-effort and never fail the script:
#   - Node.js 20 for the Astro site in site/ (`cd site && npm install && npm run build`)
#   - the `nudge` lint gate (`just check` runs `nudge check`)

GREEN='\033[0;32m'; YELLOW='\033[0;33m'; NC='\033[0m'
log()  { printf "${GREEN}==>${NC} %s\n" "$1"; }
warn() { printf "${YELLOW}warn:${NC} %s\n" "$1" >&2; }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

SUDO=""
if [ "$(id -u)" -ne 0 ] && command -v sudo >/dev/null 2>&1; then SUDO="sudo"; fi

apt_install() {
  if ! command -v apt-get >/dev/null 2>&1; then
    warn "apt-get not found; please install manually: $*"
    return 0
  fi
  $SUDO apt-get update -y
  $SUDO DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends "$@"
}

log "Installing system build dependencies"
apt_install git build-essential pkg-config ca-certificates curl

if ! command -v cargo >/dev/null 2>&1; then
  log "Installing Rust (stable)"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain stable --profile minimal
fi
# shellcheck disable=SC1091
. "$HOME/.cargo/env"
rustup component add clippy rustfmt >/dev/null 2>&1 || warn "could not add clippy/rustfmt"

if ! command -v just >/dev/null 2>&1; then
  log "Installing just (task runner)"
  cargo install just || warn "could not install just; run cargo commands directly instead of 'just check'"
fi

# Optional: nudge lint gate, invoked by `just check`.
if ! command -v nudge >/dev/null 2>&1; then
  log "Installing nudge lint gate (optional)"
  curl -sSfL https://raw.githubusercontent.com/attunehq/nudge/main/scripts/install.sh | bash \
    || warn "could not install nudge; 'just check' will fail on the nudge step. Core cargo build/test are unaffected."
fi

log "Fetching dependencies"
cargo fetch --locked || cargo fetch

log "Warming the build (cargo build --all-targets --locked)"
cargo build --all-targets --locked || cargo build --all-targets

# Optional: the Astro marketing site under site/.
if [ -f site/package.json ]; then
  NODE_MAJOR=20
  if ! command -v node >/dev/null 2>&1 || [ "$(node -v 2>/dev/null | sed 's/^v\([0-9]*\).*/\1/')" != "$NODE_MAJOR" ]; then
    log "Installing Node.js ${NODE_MAJOR} for site/ (optional)"
    if command -v apt-get >/dev/null 2>&1; then
      curl -fsSL "https://deb.nodesource.com/setup_${NODE_MAJOR}.x" | $SUDO -E bash - >/dev/null 2>&1 \
        && apt_install nodejs || warn "could not install Node.js; skipping site/ setup"
    fi
  fi
  if command -v npm >/dev/null 2>&1; then
    log "Installing site/ dependencies (optional)"
    (cd site && npm install) || warn "site/ npm install failed; the Rust crate is unaffected"
  fi
fi

log "bastion environment ready"

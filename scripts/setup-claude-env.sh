#!/usr/bin/env bash
# Claude Code cloud environment setup for bastion.
#
# Runs as root on Ubuntu 24.04 before the session starts, per
# https://code.claude.com/docs/en/claude-code-on-the-web#setup-scripts
# Point an environment's Setup script at:  bash scripts/setup-claude-env.sh
#
# Design rules (from the docs):
#   - Never block session start: every step is non-fatal and the script exits 0.
#   - Keep total runtime under ~5 minutes so the environment cache can build.
#   - Rust (rustc/cargo), git and Node (20/21/22 via nvm) are pre-installed.
# This installs `just` (the task runner) and the `nudge` gate, fetches crates,
# and warms the build. `just check` runs fmt + test + clippy + nudge.
# Idempotent and cached; safe to re-run.

set -uo pipefail

log()  { printf '==> %s\n' "$1"; }
warn() { printf 'warn: %s\n' "$1" >&2; }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

persist_path() {
  [ -n "${CLAUDE_ENV_FILE:-}" ] || return 0
  printf 'export PATH="%s:$PATH"\n' "$1" >> "$CLAUDE_ENV_FILE"
}

if ! command -v cargo >/dev/null 2>&1; then
  log "cargo not found; installing Rust via rustup"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain stable --profile minimal || warn "rustup install failed"
  # shellcheck disable=SC1091
  . "$HOME/.cargo/env" 2>/dev/null || true
fi
command -v rustup >/dev/null 2>&1 && { rustup component add clippy rustfmt >/dev/null 2>&1 || warn "could not add clippy/rustfmt"; }
persist_path "$HOME/.cargo/bin"

# `just` is the canonical task runner. cargo installs it from crates.io (allowed).
if ! command -v just >/dev/null 2>&1; then
  log "Installing just"
  cargo install just || warn "could not install just; run the cargo commands directly instead of 'just check'"
fi

# `nudge` gate, invoked by `just check`. Its installer pulls a release binary
# from github.com / raw.githubusercontent.com, both allowlisted under Trusted.
if ! command -v nudge >/dev/null 2>&1; then
  log "Installing the nudge lint gate (optional)"
  curl -sSfL https://raw.githubusercontent.com/attunehq/nudge/main/scripts/install.sh | bash \
    || warn "could not install nudge; only the nudge step of 'just check' is affected"
fi

log "Fetching crates"
cargo fetch --locked || cargo fetch || warn "cargo fetch failed (check the environment's network access level)"

log "Warming the build (best-effort)"
cargo build --all-targets --locked || cargo build --all-targets \
  || warn "cargo build did not finish; crates are fetched and the session can build in-session"

# Optional: the Astro marketing site under site/ (Node 20 is pre-installed).
if [ -f site/package.json ] && command -v npm >/dev/null 2>&1; then
  log "Installing site/ dependencies (optional)"
  ( cd site && npm install ) || warn "site/ npm install failed; the Rust crate is unaffected"
fi

log "bastion environment ready"
exit 0

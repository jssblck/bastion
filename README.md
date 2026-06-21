# Bastion

> Agentic code review for a world where agents write all of the code.

Bastion runs code review as a set of **single-concern reviewers** — focused agent
prompts, each responsible for exactly one property — over a changeset. The same
reviewers run locally (fast, pre-PR) and in CI (authoritative), and their
verdicts aggregate into one merge gate. The human moves from reviewing diffs to
authoring, curating, and governing the reviewers.

The `bastion` CLI is the local surface: an authoring agent loops `bastion review`
until green, then opens a PR that CI largely just confirms.

## Status

- **Experimental, but no longer hollow.** The data and routing layers are real
  and tested — the reviewer registry, trigger routing, the verdict and event
  schemas, the on-disk run store, and human/JSONL rendering all work. The
  **Claude Code and Codex backends now execute reviewers for real** over an
  injectable subprocess seam, and the parallel, timeout-bounded runner aggregates
  their verdicts into the merge gate; Bastion reviews its own pull requests in CI
  (see [`.github/workflows/bastion.yml`](.github/workflows/bastion.yml)). The
  remaining `Pi` backend is still stubbed and fails closed when selected.
- Rust 2024 project using `cargo`. Single binary, `bastion`.
- The design is specified in detail under [`docs/`](docs/); the code implements
  the local surface from [`docs/LOCAL.md`](docs/LOCAL.md).

## How it works

A reviewer is declared in `bastion/reviewers.yaml` — a prompt, the file globs that
trigger it, whether it gates or advises, and an optional execution environment:

```yaml
reviewers:
  - name: single-responsibility
    trigger: [src/**/*.rs]
    mode: gate
    prompt: |
      Block the PR if any one file in the changeset concentrates too many
      responsibilities; otherwise approve it.
```

`bastion review` computes the files changed against the base branch, selects the
reviewers whose triggers match, runs them, and renders progress and verdicts. An
agent passes `--format jsonl` to read a machine stream instead of human output.

## Install

Prebuilt binaries for Linux (x86_64 and aarch64, glibc and musl), macOS (Intel
and Apple silicon), and Windows (x86_64) are attached to every
[GitHub release](https://github.com/jssblck/bastion/releases). Download the
archive for your platform, extract it, and put `bastion` on your `PATH`:

```sh
# Example: Linux x86_64
curl -sSL https://github.com/jssblck/bastion/releases/latest/download/bastion-x86_64-unknown-linux-gnu.tar.gz | tar -xz
sudo install bastion-x86_64-unknown-linux-gnu/bastion /usr/local/bin/
bastion --version
```

Each archive bundles the binary with `README.md`, `LICENSE`, and `NOTICE`, and the
release lists SHA-256 `checksums.txt`. Prefer to build it yourself? See below.

## Quick start

```sh
# Build and check the version (derived from the git tag, else short SHA).
cargo build --release
./target/release/bastion --version

# Run the reviewers triggered by your working-tree changes against a base branch.
bastion review --base main

# Read it as a machine stream instead.
bastion review --base main --format jsonl

# Inspect saved runs after the fact.
bastion runs
bastion show
bastion transcript <reviewer>

# Generate a CODEOWNERS block that protects the reviewer policy (GitHub adapter).
bastion github codeowners --owner @your-org/platform
```

See [`docs/LOCAL.md`](docs/LOCAL.md) for the full local surface and the on-disk
data directory layout.

## Versioning

`bastion --version` is derived at build time from `git describe --tags`: a release
tag when one is reachable, otherwise the short commit SHA, with a `-dirty` suffix
when the working tree has uncommitted changes. Release builds may pin it via the
`BASTION_VERSION` environment variable.

## License

Bastion follows the repository license split described in `NOTICE`: runtime
software is AGPL-3.0-or-later, while documentation and creative content are
CC-BY-SA-4.0 unless a file says otherwise.

## Documentation

- [Design](docs/DESIGN.md) — the core system: reviewers, verdicts, the merge gate.
- [Bastion on GitHub](docs/GITHUB.md) — the CI adapter: Actions, checks, governance.
- [Bastion locally](docs/LOCAL.md) — the local CLI surface this crate implements.
- [Security policy](SECURITY.md)
- [Contributing](CONTRIBUTING.md)
- [Code of conduct](CODE_OF_CONDUCT.md)
- [Agent guidance](AGENTS.md)
- [Repo-local Rust skills](.agents/skills/readme.md)

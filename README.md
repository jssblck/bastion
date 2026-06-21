# Bastion

> Agentic code review for a world where agents write all of the code.

Bastion runs code review as a set of **single-concern reviewers** -- focused agent
prompts, each responsible for exactly one property -- over a changeset. The same
reviewers run locally (fast, pre-PR) and in CI (authoritative), and their verdicts
aggregate into one merge gate. The human moves from reviewing diffs to authoring,
curating, and governing the reviewers.

The `bastion` CLI is the local surface: an authoring agent loops `bastion review`
until green, then opens a PR that CI largely just confirms.

> **Status: experimental, but no longer hollow.** Routing, the runner, verdict
> aggregation, and the on-disk run store are real and tested; the Claude Code and
> Codex backends execute reviewers for real. The `pi` backend is still stubbed and
> fails closed. Rust 2024, single binary.

## Install

Prebuilt binaries for Linux (x86_64 and aarch64, glibc and musl), macOS (Intel and
Apple silicon), and Windows (x86_64) are attached to every
[GitHub release](https://github.com/jssblck/bastion/releases). Download the archive
for your platform, extract it, and put `bastion` on your `PATH`:

```sh
# Example: Linux x86_64
curl -sSL https://github.com/jssblck/bastion/releases/latest/download/bastion-x86_64-unknown-linux-gnu.tar.gz | tar -xz
sudo install bastion-x86_64-unknown-linux-gnu/bastion /usr/local/bin/
bastion --version
```

Each archive bundles the binary with `README.md`, `LICENSE`, and `NOTICE`, and the
release lists SHA-256 `checksums.txt`. To build from source instead, you need a
Rust 2024 toolchain:

```sh
cargo build --release
./target/release/bastion --version
```

## Documentation

- **[User guide](docs/user-guide/README.md)** -- using Bastion on your project:
  concepts, writing reviewers, the local loop, CI, and governance. **Start here.**
- **[Developer guide](docs/developer-guide/README.md)** -- working on Bastion
  itself: architecture, the backend boundary, conventions, and the design
  references.

The [getting-started chapter](docs/user-guide/getting-started.md) takes you from
install to your first review in about five minutes.

## License

Bastion follows the repository license split described in [`NOTICE`](NOTICE):
runtime software is AGPL-3.0-or-later, while documentation and creative content are
CC-BY-SA-4.0 unless a file says otherwise. See also the
[security policy](SECURITY.md) and [contributing guide](CONTRIBUTING.md).

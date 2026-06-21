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

The install scripts detect your platform, download the matching archive from the
latest [GitHub release](https://github.com/jssblck/bastion/releases), verify its
SHA-256 checksum, and put `bastion` on your `PATH`.

**Linux and macOS:**

```sh
curl -sSfL https://raw.githubusercontent.com/jssblck/bastion/main/scripts/install.sh | bash
```

**Windows (PowerShell):**

```powershell
irm https://raw.githubusercontent.com/jssblck/bastion/main/scripts/install.ps1 | iex
```

The shell installer accepts `-v/--version`, `-b/--bin-dir`, and `-t/--tmp-dir`
(pass them after `bash -s --`); the PowerShell installer reads the `Version` and
`BinDir` environment variables. Run either with `--help` / `$env:Help="true"` for
details. For example, to pin a version and install location:

```sh
curl -sSfL https://raw.githubusercontent.com/jssblck/bastion/main/scripts/install.sh | bash -s -- -v 0.1.0 -b /usr/local/bin
```

Prefer to do it by hand? Prebuilt binaries for Linux (x86_64 and aarch64, glibc and
musl), macOS (Intel and Apple silicon), and Windows (x86_64) are attached to every
release. Each archive bundles the binary with `README.md`, `LICENSE`, and `NOTICE`,
and the release lists SHA-256 `checksums.txt`:

```sh
# Example: Linux x86_64
curl -sSL https://github.com/jssblck/bastion/releases/latest/download/bastion-x86_64-unknown-linux-gnu.tar.gz | tar -xz
sudo install bastion-x86_64-unknown-linux-gnu/bastion /usr/local/bin/
bastion --version
```

To build from source instead, you need a Rust 2024 toolchain:

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

# Bastion

Agentic code review for a world where agents write all of the code.

## Why Bastion exists

Agents now write most of the code on a growing number of teams, at a volume closer
to _engineers x 100_ than _x 1_. Two things stop teams from fully unlocking that:

- **Human diff review does not scale.** Asking a 5-person team to review their
  agents' output is like asking 5 people in a 500-person org to review the other
  495. You cannot fix that by trying harder.
- **Without review, codebases rot.** Things go great until they don't, and then
  you have a ball of mud nobody can work in.

Existing agentic reviewers (Copilot, CodeRabbit, and the like) do a decent job, but
they were built for the _old_ world: agentic review _for humans writing code_. They
read the whole diff at once and leave comments for a person to act on, and a single
generic reviewer's recall collapses as you pile on concerns. A 1-item checklist
agent works great; at 10 items it slips; at 100 it is useless. Attention is scarce,
for humans and still for agents, and smarter models do not seem to change that.

## What Bastion does

Bastion runs code review as a set of **single-concern reviewers**: focused agent
prompts, each responsible for exactly one property, run over a changeset. Because
each reviewer owns one concern, it stays at high recall; you cover more ground by
adding narrow reviewers, never by broadening one. The same reviewers run locally
(fast, pre-PR) and in CI (authoritative), and their verdicts aggregate into one
merge gate. The human moves from reviewing diffs to authoring, curating, and
governing the reviewers.

The `bastion` CLI is the local surface: an authoring agent loops `bastion review`
until green, then opens a PR that CI largely just confirms.

For the full motivation, mental model, and threat model, see
[the core design](docs/developer-guide/design.md#the-problem).

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

- **[User guide](docs/user-guide/README.md)**: using Bastion on your project.
  Concepts, writing reviewers, the local loop, CI, and governance. **Start here.**
- **[Developer guide](docs/developer-guide/README.md)**: working on Bastion
  itself. Architecture, the backend boundary, conventions, and the design
  references.

The [getting-started chapter](docs/user-guide/getting-started.md) takes you from
install to your first review in about five minutes.

## License

Bastion follows the repository license split described in [`NOTICE`](NOTICE):
runtime software is AGPL-3.0-or-later, while documentation and creative content are
CC-BY-SA-4.0 unless a file says otherwise. See also the
[security policy](SECURITY.md) and [contributing guide](CONTRIBUTING.md).

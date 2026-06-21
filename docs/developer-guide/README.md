# Bastion developer guide

> For people working on Bastion itself -- the codebase, not the review policy.

This guide is for contributors changing Bastion's own source. If you want to *use*
Bastion on your project, you are in the wrong place: read the
[user guide](../user-guide/README.md) instead.

## Orientation

Bastion is a Rust 2024 application (a single binary, `bastion`), not a library for
crates.io. It runs single-concern reviewers as fitness functions over a changeset,
both locally (the CLI) and in CI. The data and routing layers are real and tested;
the parallel, timeout-bounded runner and the Claude Code and Codex backends execute
reviewers for real over an injectable subprocess seam. The `pi` backend is still
stubbed and fails closed when selected. Keep that boundary honest: an unimplemented
backend must never claim to have reviewed anything, and gates must fail closed.

## Contents

1. **[Architecture](./architecture.md)** -- the module-by-module map and the data
   flow of a single `bastion review`. Read this first to find your way around.
2. **[Backends](./backends.md)** -- the agent execution boundary: the `Backend`
   trait, the `CommandRunner` subprocess seam, dispatch, `MockBackend`, and how to
   add a new backend.
3. **[Conventions](./conventions.md)** -- the repo-local Rust skills,
   parse-don't-validate at boundaries, newtypes over stringly-typed data, the
   clippy lint groups, fail-closed discipline, and the testing approach (real
   fixtures, not mocks).

### Design references

The authoritative specifications live alongside this guide. They describe intended
behavior and the rationale behind it; the code implements them, and where the two
disagree the design docs are the spec the code should converge to (or the docs
should be updated -- they are meant to stay in sync). Read
[`architecture.md`](./architecture.md) first to orient; reach for these references
when a chapter points you into one.

- **[Core design](./design.md)** -- reviewers, the verdict contract, the merge
  gate, and the threat model. The authoritative design reference.
- **[GitHub adapter](./github-adapter.md)** -- the CI adapter: Actions, checks,
  governance, authentication, and billing.
- **[Local surface](./local-surface.md)** -- the local CLI surface this crate
  implements, including the on-disk run store.

## Build, test, and run

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```

`just check` runs all three; it is the gate to pass before opening a PR. The test
suite is hermetic -- no external services -- using `tempfile` for filesystem
fixtures and throwaway `git init` repositories.

Common commands while developing:

```sh
cargo run -- --version
cargo run -- review --base main
cargo run -- review --base main --format jsonl
just review main            # the same review, via the Justfile
just version
```

## Targeted checks

Run these in addition to the core three when relevant:

- **Versioning changes:** `bastion --version` (the string is derived at build time
  by `build.rs` from `git describe --always --tags --dirty=-dirty`, overridable via
  the `BASTION_VERSION` env var, with a `Cargo.toml` fallback).
- **Schema changes:** update [`bastion/reviewers.yaml`](../../bastion/reviewers.yaml)
  and the affected docs. The local and GitHub surfaces are mirror images and must
  not drift; a schema change touches both surfaces, the user guide, and the design
  references.
- **Public scaffolding changes:** keep `README.md`, `CONTRIBUTING.md`,
  `SECURITY.md`, `NOTICE`, and the GitHub workflows in sync.

## Contributing and releases

The contribution workflow, AI-assisted-contribution policy, and the release process
(tagging, the release matrix, version derivation, and bumping the self-review pin)
live in [`CONTRIBUTING.md`](../../CONTRIBUTING.md) at the repository root. Agent
guidance -- the same rules in the form coding agents consume -- is in
[`AGENTS.md`](../../AGENTS.md).

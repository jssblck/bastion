# AGENTS.md

Guidance for coding agents working in this repository.

## Project overview

Bastion is a Rust 2024 agentic code-review system. Single-concern reviewers run
as fitness functions over a changeset, both locally (the `bastion` CLI) and in
CI. Each reviewer is a focused agent prompt with a trigger; matched reviewers
run, return a structured verdict, and Bastion aggregates them into one merge
gate. The human sits at the policy layer, authoring and governing reviewers.

**This crate is past the walking-skeleton stage but still partial.** The data and
routing layers are real and tested; the parallel, timeout-bounded runner
(`src/runner.rs`) and the Claude Code and Codex backends (`src/backend/`) are
implemented and execute reviewers for real over an injectable subprocess seam.
The `Pi` backend is still stubbed and fails closed when selected. Keep that
boundary honest: do not make an unimplemented backend claim to have reviewed
anything, and keep gates failing closed.

## Source of truth

- `README.md` — sparse user-facing intro, install, and links into the guides.
- `docs/user-guide/` — task-oriented guide for people *using* Bastion (concepts,
  authoring reviewers, the local loop, CI, governance). Progressive disclosure.
- `docs/developer-guide/` — guide for people working on Bastion itself
  (architecture, the backend boundary, conventions), plus the design references:
  - `docs/developer-guide/design.md` — the core system: reviewers, the verdict
    contract, the merge gate, the threat model. The authoritative design reference.
  - `docs/developer-guide/github-adapter.md` — the GitHub CI adapter and governance.
  - `docs/developer-guide/local-surface.md` — the local CLI surface this crate
    implements. The local and GitHub surfaces are deliberate mirror images; keep
    them in sync.
- `bastion/reviewers.yaml` — the example reviewer registry; update it when the
  schema changes.
- `.agents/skills/readme.md` — repo-local Rust coding skills and their provenance.
- `CLAUDE.md` is a bare `@AGENTS.md` import so guidance does not drift between
  agent surfaces.

## Build, test, and run

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```

`just check` runs all three. Commands:

```sh
bastion --version
bastion review --base main
bastion review --base main --format jsonl
bastion runs
bastion show
bastion transcript <reviewer>
bastion clean --keep 20
bastion github codeowners --owner @your-org/platform
```

## Architecture map

For the full module map and the life of a `bastion review`, see
[`docs/developer-guide/architecture.md`](docs/developer-guide/architecture.md); the
backend boundary is covered in
[`docs/developer-guide/backends.md`](docs/developer-guide/backends.md). The terse
version:

- `build.rs` — derives `BASTION_VERSION` from `git describe --always --tags
  --dirty=-dirty`, with a `BASTION_VERSION` env override and a `Cargo.toml`
  fallback.
- `src/main.rs` — thin binary entrypoint; wires the tokio runtime to
  `bastion::run`.
- `src/lib.rs` — library root; installs `color_eyre` + `tracing` and dispatches.
- `src/version.rs` — exposes the build-derived version string.
- `src/cli.rs` — clap derive command tree and dispatch.
- `src/commands.rs` — one handler per subcommand.
- `src/reviewer.rs` / `src/config.rs` — the declarative reviewer schema and
  registry loading/discovery.
- `src/routing.rs` — compiling trigger globs and matching changed files.
- `src/verdict.rs` / `src/event.rs` — the structured verdict and run-event
  schemas (the `Money` type carries cents but serializes as dollars).
- `src/git.rs` — the git queries the CLI needs (changed files, branch, root).
- `src/paths.rs` / `src/store.rs` — the data-directory layout and run history.
- `src/render.rs` — human and JSONL output.
- `src/runner.rs` — the parallel, timeout-bounded runner: fans matched reviewers
  out over a `JoinSet`, fails closed on error/timeout, streams run events, and
  persists each run.
- `src/backend/` — the agent execution boundary. `mod.rs` defines the `Backend`
  trait, the deterministic `MockBackend`, and `dispatch`; `command.rs` is the
  injectable `CommandRunner` subprocess seam; `claude_code.rs` and `codex.rs` are
  the real backends driven against a fake executable in tests. The `Pi` arm of
  `dispatch` is still unwired and bails.
- `tests/integration.rs` — the end-to-end suite. It drives the *real compiled
  `bastion` binary* (`CARGO_BIN_EXE_bastion`), each scenario in its own throwaway
  `git` repo and private `BASTION_DATA_DIR`, against a `rustc`-compiled fake agent
  wired in via `BASTION_CLAUDE_BIN`/`BASTION_CODEX_BIN`. The fake reads per-reviewer
  `env` (which Bastion propagates into the child) to stage passes, blocks, malformed
  output, crashes, and hangs, so the suite exercises the full subprocess path,
  fail-closed/fail-open aggregation, concurrency, persistence, and the read-back
  commands at scale. It detect-and-skips when `rustc`/`git` are absent.
- `scripts/install.sh` / `scripts/install.ps1` — the public install scripts
  (`curl | bash` and `irm | iex`). They detect the platform, download the matching
  release archive plus `checksums.txt`, verify the SHA-256, and place `bastion` on
  the user's `PATH`. They fail closed on any checksum problem; `tests/script_safety.rs`
  pins that. `.github/workflows/installers.yml` smoke-tests them against published
  releases on a schedule (not in PR CI, since it depends on release state).

## Development rules

- Do not preserve backwards compatibility by default. Mention breakage plainly.
- Keep the local surface and the GitHub adapter as mirror images: the same
  reviewers, verdicts, and findings, presented through whatever each transport
  makes natural. A schema change touches both surfaces and `docs/`.
- Reviewers are declarative and static. Do not add code paths that generate
  reviewers on the fly; that would break the stable trigger set and the
  governance story.
- When you fix an issue, consider whether the class of issue is one a Bastion
  reviewer could catch in future changesets (a recurring bug pattern, a convention
  that keeps getting violated, a footgun in the schema or CLI surface). If so,
  suggest adding or extending a reviewer in `bastion/reviewers.yaml` and say what
  its concern and trigger would be. Do not add the reviewer yourself: reviewers are
  governed policy, so leave the decision to the user.
- Gates fail closed. A gate that cannot produce a valid verdict is a block, never
  a silent pass. Advisors fail open.
- Do not use mocks for collaborators; prefer real pure functions and real
  filesystem/git fixtures (`tempfile`, throwaway `git init` repos), as the
  existing tests do. `MockBackend` is a deliberate deterministic test/dev double
  for the agent boundary, not a general mocking pattern.
- Follow the repo-local Rust skills under `.agents/skills/`: parse-don't-validate
  at boundaries, newtypes over stringly-typed data, and the clippy lint groups in
  `Cargo.toml`.
- Use plain ASCII quotes in docs, comments, and generated text.

## Releases

Bastion ships as a binary on GitHub Releases. It is not published to crates.io, so
a release is just a tag, never a crates.io publish.

- To cut a release, push a tag in the shape `vX.Y.Z` to the remote (for example
  `git tag v0.2.0 && git push origin v0.2.0`). The
  [release workflow](.github/workflows/release.yml) fires on `v*` tags, builds the
  platform matrix, and opens a draft GitHub Release for a human to publish.
- Do not bump the crate version in `Cargo.toml`. Leave `version = "0.0.0"` as it is:
  it is a deliberate placeholder, not a real version. The released binary's
  `--version` comes from the git tag (CI passes the tag through `BASTION_VERSION`;
  locally `build.rs` runs `git describe`). The `Cargo.toml` version is only a
  build-time fallback, not the source of truth, and is never published.
- A tag with a pre-release suffix (`v0.2.0-rc.1`) ships as a prerelease.

The full release runbook (the build matrix, version derivation, and bumping the
self-review pin in `.github/workflows/bastion.yml`) lives in `CONTRIBUTING.md`.

## Verification expectations

Run the core checks for ordinary changes:

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```

Also run targeted checks when relevant:

- Versioning changes: run `bastion --version`.
- Schema changes: update `bastion/reviewers.yaml` and the docs under `docs/`.
- Public scaffolding changes: keep `README.md`, `CONTRIBUTING.md`, `SECURITY.md`,
  `NOTICE`, and the GitHub workflows in sync.

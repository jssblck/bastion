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

- `README.md` — user-facing overview and project status.
- `docs/DESIGN.md` — the core system: reviewers, the verdict contract, the merge
  gate, the threat model. The authoritative design reference.
- `docs/GITHUB.md` — the GitHub CI adapter and the governance model.
- `docs/LOCAL.md` — the local CLI surface this crate implements. The local and
  GitHub surfaces are deliberate mirror images; keep them in sync.
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

## Development rules

- Do not preserve backwards compatibility by default. Mention breakage plainly.
- Keep the local surface and the GitHub adapter as mirror images: the same
  reviewers, verdicts, and findings, presented through whatever each transport
  makes natural. A schema change touches both surfaces and `docs/`.
- Reviewers are declarative and static. Do not add code paths that generate
  reviewers on the fly; that would break the stable trigger set and the
  governance story.
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

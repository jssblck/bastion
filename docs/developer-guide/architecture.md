# Architecture

> The module map and the life of a single `bastion review`.

[<- Developer guide index](./README.md) | Next: [Backends](./backends.md) ->

---

Bastion is a small, flat crate: a thin binary over a library, with one module per
concern. This chapter is the map: where each thing lives, and how a review flows
through it.

## The module map

| Module | Responsibility |
| --- | --- |
| [`build.rs`](../../build.rs) | Derives `BASTION_VERSION` from `git describe --always --tags --dirty=-dirty`, with a `BASTION_VERSION` env override and a `Cargo.toml` fallback. |
| [`src/main.rs`](../../src/main.rs) | Thin binary entrypoint; wires the tokio runtime to `bastion::run`. |
| [`src/lib.rs`](../../src/lib.rs) | Library root; installs `color_eyre` + `tracing` and dispatches. |
| [`src/version.rs`](../../src/version.rs) | Exposes the build-derived version string. |
| [`src/cli.rs`](../../src/cli.rs) | The clap derive command tree and dispatch; maps a `block` aggregate to a non-zero exit. |
| [`src/commands.rs`](../../src/commands.rs) | One handler per subcommand. |
| [`src/reviewer.rs`](../../src/reviewer.rs) | The declarative reviewer schema (`Reviewer`, `Mode`, `Backend`, `Capabilities`, `RunnerSpec`). |
| [`src/config.rs`](../../src/config.rs) | Registry loading and discovery (walk up for `bastion/reviewers.yaml`; validate name uniqueness). |
| [`src/routing.rs`](../../src/routing.rs) | Compiling trigger globs and matching them against changed files. |
| [`src/verdict.rs`](../../src/verdict.rs) | The structured verdict (`Decision`, `Verdict`, `Finding`, `Usage`, and `Money`, which carries cents but serializes as dollars). |
| [`src/event.rs`](../../src/event.rs) | The run-event schema streamed as JSONL and persisted to `run.jsonl`. |
| [`src/git.rs`](../../src/git.rs) | The git queries the CLI needs (changed files, branch, repo root). |
| [`src/paths.rs`](../../src/paths.rs) | The data-directory layout (`Layout`), resolved by platform convention or `BASTION_DATA_DIR`. |
| [`src/store.rs`](../../src/store.rs) | Run-history persistence: writing/reading `run.jsonl`, listing and pruning runs. |
| [`src/render.rs`](../../src/render.rs) | Human and JSONL output (`Format`). |
| [`src/runner.rs`](../../src/runner.rs) | The parallel, timeout-bounded runner: fans matched reviewers out over a `JoinSet`, fails closed on error/timeout, streams events, persists each run. |
| [`src/skills.rs`](../../src/skills.rs) | The agent skills bundled into the binary (from `skills/<slug>/SKILL.md`) and installed into a consuming repo by `bastion skills install`/`check`/`list`. The rendered file is deterministic so `check` is a version-independent drift guard. |
| [`src/backend/`](../../src/backend/) | The agent execution boundary. See [Backends](./backends.md). |
| [`src/github/`](../../src/github/) | The GitHub adapter (CI surface): `codeowners.rs` generates the governance block, `client.rs` is the `reqwest`-backed REST seam (a proof-carrying `ApiRequest` plus a `GitHubApi` trait and a recording test double, modeled on the backend's `CommandRunner`), and `report.rs` posts a finished run as a sticky PR comment and check runs. See the [GitHub adapter](./github-adapter.md). |

## The two boundaries that shape the design

Two seams are worth understanding before you change anything, because most of the
structure exists to keep them honest.

- **The backend boundary** ([`src/backend/`](../../src/backend/)). Bastion does not
  run agent loops; it shells out to existing agent CLIs. The `Backend` trait, the
  `CommandRunner` subprocess seam, and `dispatch` isolate everything agent- and
  subprocess-specific so the runner above stays pure orchestration and the tests
  drive real backends against a fake executable. Covered in
  [Backends](./backends.md).
- **The parse-don't-validate boundary** (`config.rs` -> `reviewer.rs` ->
  `routing.rs`). Untrusted text (the YAML registry, git output, CLI args) is parsed
  *once* at the edge into precise types (a `Reviewer`, a compiled glob matcher, a
  `RunId`) rather than carried around stringly-typed and re-checked. Covered in
  [Conventions](./conventions.md).

## The life of a `bastion review`

Following one review top to bottom touches most of the crate:

1. **Parse & resolve** (`cli.rs`). clap parses the command. The data directory is
   resolved into a `Layout` (`paths.rs`), from `--data-dir`/`BASTION_DATA_DIR` or
   the platform default.
2. **Load policy** (`config.rs`). The registry is discovered by walking up from the
   cwd for `bastion/reviewers.yaml`, parsed into `Config`, and validated (unique
   reviewer names). Malformed input fails here, before any agent runs.
3. **Compute the changeset** (`git.rs`). Bastion asks git for the files that differ
   from `--base` (tracked edits *and* untracked files, committed or not) plus the
   current branch and repo root.
4. **Route** (`routing.rs`). Each reviewer's trigger globs are compiled and matched
   against the changed files; the matched reviewers are the ones that will run.
5. **Run** (`runner.rs`). `execute` spawns every matched reviewer onto a `JoinSet`,
   bounds each by its `timeout` (default 15m), and emits `reviewer.started` up
   front. Each task calls `backend::dispatch` (`backend/mod.rs`), which selects the
   concrete backend and runs the agent.
6. **Resolve & aggregate** (`runner.rs`). Each result has fail-closed/fail-open
   policy applied: a gate that blocks, errors, or times out resolves to `block`
   (with a synthetic blocking finding); an advisor that fails is dropped. The
   aggregate is `block` if any gate blocked, else `pass`.
7. **Emit & persist** (`render.rs`, `store.rs`). Events stream out as human text or
   JSONL as they happen; the full event stream, plus per-reviewer transcript,
   verdict, and metadata, is written under the run's directory, and `latest` is
   updated.
8. **Exit** (`cli.rs`). The aggregate `Decision` maps to the process exit code:
   `pass` -> success, `block` -> failure, so an agent loop and CI agree on the gate.

The read-back commands (`transcript`, `show`, `runs`, `clean`) skip steps 3-6 and
read the persisted run store directly.

## Why the runner owns persistence

`execute` owns both event emission *and* persistence, so `commands::review` only
has to render the stream and map the aggregate to an exit code. This is deliberate:
it keeps the `run.jsonl` on disk identical to the live stream (it even reconstructs
the authoritative `run.started` and prepends the retained `reviewer.started` events
so a replay sees the exact sequence the live run emitted), and it means there is
one place, not two, that decides what a run records.

---

Next: [Backends](./backends.md). The agent execution boundary in detail.

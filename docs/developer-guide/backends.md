# Backends

> The agent execution boundary: the trait, the subprocess seam, dispatch, and how
> to add a backend.

[<- Architecture](./architecture.md) | [Developer guide index](./README.md) | Next: [Containers](./containers.md) ->

---

Bastion does not implement an agent loop. It translates a reviewer's execution
profile into an existing agent CLI's native config and shells out to it, capturing
the structured verdict. Everything agent- and subprocess-specific is confined to
[`src/backend/`](../../src/backend/) so the [runner](./architecture.md) above it
stays pure orchestration.

## The pieces

| File | Role |
| --- | --- |
| [`mod.rs`](../../src/backend/mod.rs) | The `Backend` trait, `ReviewRequest`/`ReviewOutcome`, `MockBackend`, `dispatch`, and the shared prompt/helpers (`changeset_preamble`, `EXHAUSTIVE_FINDINGS_INSTRUCTION`, `interpolate`, `money_from_dollars`). |
| [`command.rs`](../../src/backend/command.rs) | The `CommandRunner` subprocess seam: `CommandSpec` and `SystemCommandRunner`, plus a fake runner for tests. |
| [`claude_code.rs`](../../src/backend/claude_code.rs) | The Claude Code backend. |
| [`codex.rs`](../../src/backend/codex.rs) | The Codex backend. |
| [`container/`](../../src/backend/container/) | The container runner, split by concern: `plan.rs` (`ExecutionPlan` and image resolution), `runner.rs` (the `CommandRunner` decorator), `credentials.rs`, and `teardown.rs`. See [Containers](./containers.md). |

## The trait

```rust
pub trait Backend {
    fn id(&self) -> reviewer::Backend;
    async fn review(&self, request: &ReviewRequest<'_>) -> Result<ReviewOutcome>;
}
```

A backend is handed a `ReviewRequest` (the reviewer, the run id, the repo root, and
the base branch) and returns a `ReviewOutcome` (the structured `Verdict`, optional
`Usage`, and the optional full transcript). The trait is deliberately small and
stable: sibling backends implement the same signature, and `dispatch` is the single
place that grows when one lands; the trait does not.

An error from `review` is *not* a verdict. The runner turns it into a fail-closed
`block` for a gate (with a synthetic blocking finding) and drops it for an advisor.
A backend should return `Err` when it cannot produce a valid verdict, never a
fabricated pass.

## Dispatch

`dispatch` first rejects an unwired backend, then resolves the reviewer's
[`ExecutionPlan`](./containers.md), then selects a concrete backend:

```rust
ensure_backend_wired(request.reviewer.backend, &request.reviewer.name)?;
match ExecutionPlan::resolve(request.reviewer)? {
    ExecutionPlan::Native        => run_backend(request, SystemCommandRunner, Program::HostDefault).await,
    ExecutionPlan::Container(plan) => {
        let image = plan.ensure_image(&engine, &SystemCommandRunner, repo_root).await?;
        run_backend(request, ContainerRunner::new(..., image, ...), Program::InContainer).await
    }
}
```

The `ensure_backend_wired` preflight runs **before** plan resolution, so an
unimplemented backend (`Pi`) fails closed before any side effect. A `backend: pi`
reviewer with a `runner` fails at backend selection, before Bastion builds or pulls a
container image. Resolving the plan is then the **single place an unprovisioned
capability tier fails closed** (see [Containers](./containers.md)), so a backend is
only ever reached for a reviewer this build can actually run. `run_backend` then maps `reviewer::Backend` to
the concrete backend, shared by the native and container paths so backend selection
lives in one place:

```rust
// `program` is HostDefault on the native path, InContainer on the container path.
match request.reviewer.backend {
    Backend::Any | Backend::ClaudeCode => match program {
        Program::HostDefault => ClaudeCodeBackend::new(runner).review(request).await,
        Program::InContainer => ClaudeCodeBackend::with_program(runner, claude_code::DEFAULT_PROGRAM)
            .review(request).await,
    },
    Backend::Codex => match program {
        Program::HostDefault => CodexBackend::new(runner).review(request).await,
        Program::InContainer => CodexBackend::with_program(runner, codex::DEFAULT_PROGRAM)
            .review(request).await,
    },
    Backend::Pi => bail!("the pi backend is not yet wired ..."),
}
```

`Any` defaults to Claude Code until routing by availability/subscription exists.
**`Pi` fails closed**: it is named in the schema but not implemented, so selecting
it errors rather than silently passing. This is load-bearing: an unimplemented
backend must never claim to have reviewed anything. There is a test
(`dispatch_rejects_unwired_backends`) guarding exactly that. The only difference
between the native and container paths is how the program is resolved, the `program`
branch above: natively `new` takes it from the host (`BASTION_CLAUDE_BIN` / `PATH`),
while in a container `with_program` pins the bare default name (`claude` / `codex`)
so it resolves on the image's `PATH` rather than a host path that means nothing
inside the image.

## The subprocess seam

Backends never call `std::process::Command` directly. They build a `CommandSpec`
(program, args, working directory, environment) and hand it to a `CommandRunner`.
Production uses `SystemCommandRunner`; tests inject a fake that records the specs it
was given and returns canned stdout. This is what lets `claude_code.rs` and
`codex.rs` be tested against a *fake executable* with no real agent, network, or
cost, while still exercising the real argument-building, env-injection, output
parsing, and retry logic.

`ContainerRunner`'s drop guard is the one exception to this seam. `ContainerGuard`
runs the container teardown (`docker rm -f`) with a direct `std::process::Command` in
`Drop`. The seam is async and a `Drop` is not, so the cancellation teardown cannot
route through `CommandRunner::run`. It is a fixed engine invocation (`rm -f` on the
container's own generated name, no reviewer-controlled input), bounded by a teardown
budget and run on its own thread so it never blocks the runtime. See
[Containers](./containers.md#timeouts-and-teardown).

## Shared behavior in `mod.rs`

These shared helpers keep the backends consistent so a reviewer behaves the same
regardless of which agent runs it:

- **`changeset_preamble`**: the instruction prepended to every prompt telling the
  agent how to see its changeset. It steers to `git diff {base}` (the working-tree
  form: working tree vs. base) plus an untracked-file scan, and explicitly warns *off*
  `{base}...HEAD`, which shows only committed history and would miss the uncommitted
  work an author iterates on locally. In CI the head is committed and there are no
  untracked files, so the same instruction is correct there too.
- **`EXHAUSTIVE_FINDINGS_INSTRUCTION`**: a fixed instruction appended to every
  reviewer prompt (after the reviewer's own text, before the schema instruction)
  telling the agent to enumerate *every* qualifying finding in one pass rather than
  stopping at the first. A verdict is consistent with a single blocking finding, so
  without this an agent tends to report one issue and stop, forcing the author
  through a fresh review cycle per issue. It changes only how completely a reviewer
  reports, never the gate decision: a clean changeset still returns `pass` with no
  findings, and the reviewer's own prompt still decides what counts as an issue.
- **`interpolate`**: substitutes `${key}` placeholders in a prompt from the
  reviewer's `inputs`. Unknown placeholders are left as literal text (the reviewer
  author is trusted; a literal `${...}` is harmless).
- **`money_from_dollars`**: converts a backend-reported dollar cost into the exact
  `Money` (cents) type, clamping negative or non-finite values to zero so a
  malformed cost can never produce a nonsensical charge.

## What a backend applies from the profile

The reviewer schema is fuller than the current native execution path. Be precise
about what is honored, so the code does not over-promise:

| Field | Status in this build |
| --- | --- |
| `prompt`, `trigger`, `mode`, `name` | Fully honored. |
| `backend` | Honored (`claude-code`, `codex`; `any` -> Claude Code; `pi` fails closed). |
| `timeout` | Honored by the runner. |
| `inputs` | Honored, interpolated into the prompt. |
| `env` | Honored, injected into the child process environment. |
| `runner` (`dockerfile`, `image`) | **Honored.** A reviewer with a `runner` block runs its backend inside a container; `dockerfile` is built (cached by content hash), `image` is used as-is. See [Containers](./containers.md). |
| `capabilities.network: true` | **Honored in a container, but unscoped.** A containerized reviewer's container has outbound network. The `network: false` default is *not scoped* (egress allowlisting is unimplemented), so both attach the engine's default network. A *native* `network: true` (no `runner`) fails closed: with no container there is nothing to scope. |
| `capabilities` (`mcp`, `skills`) | **Not provisioned: fails closed.** A reviewer that declares either is failed closed by `ExecutionPlan::resolve` in `dispatch`. |

The unprovisioned opt-ins **fail closed** rather than silently degrading: a gate that
declares a tier it cannot get must block, never run degraded and report a pass (see
[`ExecutionPlan::resolve`](../../src/backend/container/plan.rs) and the
[core design](./design.md#aggregation--the-merge-gate)). As each tier is wired, its
arm of the preflight is removed and this row flips to "honored".

When you wire the next tier (`mcp`, then `skills`), this is the table to update, and
with it the
[authoring guide's note](../user-guide/authoring-reviewers.md#runner-and-capabilities)
and the [user-facing status](../user-guide/README.md#status).

## Adding a backend

1. Add a variant to `reviewer::Backend` in [`reviewer.rs`](../../src/reviewer.rs)
   (it is `#[non_exhaustive]`; keep `as_str` and the kebab-case serde form in sync).
2. Create `src/backend/<name>.rs` implementing the `Backend` trait, building its
   `CommandSpec` and parsing its CLI's structured-output envelope into a `Verdict`.
   Reuse `changeset_preamble`, `interpolate`, and `money_from_dollars`, and append
   `EXHAUSTIVE_FINDINGS_INSTRUCTION` to the prompt so the new backend enumerates
   every finding in one pass like the others.
3. Wire the variant into `dispatch` in [`mod.rs`](../../src/backend/mod.rs).
4. Test it against a fake `CommandRunner`, following `claude_code.rs` /
   `codex.rs`: assert the args and env you build, and the parsing of a representative
   envelope, including the malformed-output retry path.

`MockBackend` is *not* the template for a new backend; it is a deterministic
always-pass double for testing the runner without any agent. Real backends drive a
fake executable instead.

## The verdict round-trip

Backends capture the agent's structured output, then validate it against the
verdict schema: Claude Code via a JSON schema (`--json-schema`), Codex via a
requested fenced verdict block parsed from its final message. If the agent does not produce
complying output, the backend re-runs the *same session* with a turn that re-states
the schema and asks for just the structured output of the work already done; only
after that fails does it give up with an error (which the runner fails closed). The
verdict schema itself is specified in the
[core design](./design.md#the-verdict).

---

Next: [Containers](./containers.md). How a reviewer with a `runner` block executes
inside a container.

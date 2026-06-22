# Backends

> The agent execution boundary: the trait, the subprocess seam, dispatch, and how
> to add a backend.

[<- Architecture](./architecture.md) | [Developer guide index](./README.md) | Next: [Conventions](./conventions.md) ->

---

Bastion does not implement an agent loop. It translates a reviewer's execution
profile into an existing agent CLI's native config and shells out to it, capturing
the structured verdict. Everything agent- and subprocess-specific is confined to
[`src/backend/`](../../src/backend/) so the [runner](./architecture.md) above it
stays pure orchestration.

## The pieces

| File | Role |
| --- | --- |
| [`mod.rs`](../../src/backend/mod.rs) | The `Backend` trait, `ReviewRequest`/`ReviewOutcome`, `MockBackend`, `dispatch`, and the shared helpers (`changeset_preamble`, `interpolate`, `money_from_dollars`). |
| [`command.rs`](../../src/backend/command.rs) | The `CommandRunner` subprocess seam: `CommandSpec` and `SystemCommandRunner`, plus a fake runner for tests. |
| [`claude_code.rs`](../../src/backend/claude_code.rs) | The Claude Code backend. |
| [`codex.rs`](../../src/backend/codex.rs) | The Codex backend. |

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

`dispatch` maps `reviewer::Backend` to a concrete backend:

```rust
match request.reviewer.backend {
    Backend::Any | Backend::ClaudeCode => ClaudeCodeBackend::new(SystemCommandRunner).review(request).await,
    Backend::Codex                     => CodexBackend::new(SystemCommandRunner).review(request).await,
    Backend::Pi                        => bail!("the pi backend is not yet wired ..."),
}
```

`Any` defaults to Claude Code until routing by availability/subscription exists.
**`Pi` fails closed**: it is named in the schema but not implemented, so selecting
it errors rather than silently passing. This is load-bearing: an unimplemented
backend must never claim to have reviewed anything. There is a test
(`dispatch_rejects_unwired_backends`) guarding exactly that.

## The subprocess seam

Backends never call `std::process::Command` directly. They build a `CommandSpec`
(program, args, working directory, environment) and hand it to a `CommandRunner`.
Production uses `SystemCommandRunner`; tests inject a fake that records the specs it
was given and returns canned stdout. This is what lets `claude_code.rs` and
`codex.rs` be tested against a *fake executable* with no real agent, network, or
cost, while still exercising the real argument-building, env-injection, output
parsing, and retry logic.

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

## What a backend applies from the profile today

The reviewer schema is fuller than the current native execution path. Be precise
about what is honored, so the code does not over-promise:

| Field | Status in this build |
| --- | --- |
| `prompt`, `trigger`, `mode`, `name` | Fully honored. |
| `backend` | Honored (`claude-code`, `codex`; `any` -> Claude Code; `pi` fails closed). |
| `timeout` | Honored by the runner. |
| `inputs` | Honored, interpolated into the prompt. |
| `env` | Honored, injected into the child process environment. |
| `capabilities` (`network`, `mcp`, `skills`) | **Parsed but not provisioned.** Acknowledged in `base_spec` (`let _ = reviewer.capabilities...`) and deferred to the container runner. |
| `runner` (`dockerfile`, `image`) | **Parsed but not provisioned.** Execution is native only; no container is built. |

When you wire the container runner, this is the table to update, and with it the
[authoring guide's note](../user-guide/authoring-reviewers.md#runner-and-capabilities-declared-not-yet-provisioned)
and the [user-facing status](../user-guide/README.md#status).

## Adding a backend

1. Add a variant to `reviewer::Backend` in [`reviewer.rs`](../../src/reviewer.rs)
   (it is `#[non_exhaustive]`; keep `as_str` and the kebab-case serde form in sync).
2. Create `src/backend/<name>.rs` implementing the `Backend` trait, building its
   `CommandSpec` and parsing its CLI's structured-output envelope into a `Verdict`.
   Reuse `changeset_preamble`, `interpolate`, and `money_from_dollars`.
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

Next: [Conventions](./conventions.md). The coding rules this crate holds itself
to.

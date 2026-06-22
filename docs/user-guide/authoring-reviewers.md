---
title: Authoring reviewers
summary: "The registry schema in full, and how to write a reviewer that stays sharp."
order: 4
---

# Authoring reviewers

> The registry schema in full, and how to write a reviewer that stays sharp.

Reviewers are the whole policy. This chapter is the reference for writing them:
the file, the required fields, the optional execution profile, and the craft of a
prompt that keeps recall high. It progresses from the minimum you need to the
fields you will reach for only occasionally.

## The registry file

All reviewers live in one file: `bastion/reviewers.yaml`, relative to your
repository root. Bastion finds it by walking up from the current directory, so the
command works anywhere inside the repo. The file is a single `reviewers:` list:

```yaml
reviewers:
  - name: single-responsibility
    trigger: [src/**/*.rs]
    mode: gate
    prompt: |
      ...
  - name: test-coverage
    trigger: [src/**/*.rs]
    mode: advisor
    prompt: |
      ...
```

Reviewer **names must be unique** within the file; a duplicate name is a load
error. Because this file *is* the review policy, changes to it should require human
review; see [Governance](./governance.md) and `bastion github codeowners`.

## The required fields

Four fields are mandatory. A reviewer with just these is complete and runnable.

### `name`

A unique identifier. It is also the reviewer's check-run name in CI
(`bastion / single-responsibility`), so keep it short and descriptive.

### `trigger`

A list of path globs matched against the changed files. The reviewer runs if any
changed file matches any glob. Globs use the usual `**` (any depth) and `*` (one
segment) syntax:

```yaml
trigger: [src/**/*.rs]                       # all Rust under src, any depth
trigger: [src/server/**, src/client/**]      # either subtree
trigger: [src/**/*.rs, docs/**/*.md, "bastion/reviewers.yaml"]   # multiple kinds
```

Quote a glob if YAML would otherwise mis-parse it (a bare leading `*`, for
instance). Scope triggers tightly: a narrow trigger is what keeps an irrelevant
reviewer from waking on every change.

### `mode`

`gate` (blocks the merge when it returns `block`; fails closed) or `advisor`
(never blocks; fails open). See [Concepts](./concepts.md#the-mode-gate-vs-advisor)
for the full semantics.

### `prompt`

The instruction handed to the reviewing agent. This is where the craft lives; see
[Writing a good prompt](#writing-a-good-prompt) below.

## The optional execution profile

The remaining fields tune *how* a reviewer runs. All have defaults; omit them
until you need them.

### `backend`

Which agent harness runs the reviewer. Default `any` (resolves to Claude Code
today). Pin `claude-code`, `codex`, or `pi` to force a specific harness, usually
because a subscription's terms require it.

```yaml
backend: codex
```

> `pi` parses but is not wired in this build; a reviewer pinned to it fails closed.

### `timeout`

A per-reviewer wall-clock limit, written in human form (`90s`, `15m`). When a
reviewer exceeds it, a gate fails closed (block) and an advisor is skipped. The
default is **15 minutes**. Set a short timeout on cheap reviewers and a long one on
heavy end-to-end checks:

```yaml
timeout: 15m
```

### `env`

Environment variables injected into the reviewer's process, so the agent and any
tool it runs can see them. Use this to hand a reviewer a value your environment
already provides, say a preview URL:

```yaml
env:
  PREVIEW_URL: http://localhost:3000
```

Values are **literal**: Bastion does not perform shell `$VAR` expansion, so write
the actual value, not `${SOMETHING}`. The reviewer process also inherits Bastion's
own environment, so a variable your shell or CI has already exported is visible to
the agent even without listing it here; the `env` block is for setting additional
values explicitly. Bastion consumes environments, it does not provision them:
locally the value must already exist (a precommit script might boot the service and
export it), and in CI the workflow stands it up. See
[Continuous integration](./continuous-integration.md#environments--inputs).

### `inputs`

Values interpolated into the prompt *before* it reaches the agent. Reference an
input as `${name}` in the prompt; Bastion substitutes the value. Unknown
placeholders are left untouched.

```yaml
inputs:
  preview_url: http://localhost:3000
prompt: |
  Run the checkout flow against the preview environment at `${preview_url}`.
  If it fails, block the PR and explain; otherwise approve it.
```

`env` puts a value in the *process*; `inputs` puts a value in the *prompt text*.
They are independent: use `env` for tools the agent invokes, `inputs` for values
the agent should read in its instructions. Input values are literal as well: a
`${name}` in the prompt is substituted only from this `inputs` map, never from your
shell environment.

### `runner` and `capabilities` (declared, not yet provisioned)

The schema also accepts a `runner` block (`dockerfile` / `image`, to run a reviewer
in a container) and a `capabilities` block (`network`, `mcp`, `skills`, to opt into
privileges beyond the least-privilege default). These describe the design's intended
execution model and parse correctly today, **but this build executes every reviewer
natively and does not yet provision containers, extra network, MCP servers, or
skills.** Because a gate that quietly ran without a privilege it asked for would be a
silent fail-open, a reviewer that opts into one of these **fails closed**: a gate
blocks and an advisor is skipped, with a message naming the unprovisioned field,
rather than running degraded. So leave these out until the tier you need has landed.
The least-privilege default (no `runner`, `network: false`, no `mcp` or `skills`) is
what runs today. The authoritative description of the intended behavior is in the
[core design](../developer-guide/design.md#the-reviewer).

## A fully-loaded example

Putting the optional fields together (the `runner` and `capabilities` blocks are
shown for schema completeness; as written, this reviewer **fails closed** today
because it opts into tiers this build does not provision yet):

```yaml
reviewers:
  - name: e2e-checkout-flow
    trigger: [src/**]
    mode: gate
    backend: claude-code
    timeout: 15m
    env:
      PREVIEW_URL: http://localhost:3000     # literal value, no shell expansion
    inputs:
      preview_url: http://localhost:3000     # substituted into the prompt as ${preview_url}
    runner:                                  # not yet provisioned: fails the gate closed
      dockerfile: ./bastion/e2e.Dockerfile
    capabilities:                            # not yet provisioned: fails the gate closed
      network: true
      mcp: [playwright]
    prompt: |
      Run the e2e checkout flow against the preview environment at `${preview_url}`
      using Playwright. If it fails, block the PR and explain; otherwise approve it.
```

## Writing a good prompt

The prompt is the reviewer. A few habits keep recall high:

- **Say what to block on, explicitly.** End with a clear instruction: "block the
  PR if X; otherwise approve it." The reviewer's job is a decision, not an essay.
- **Name the one concern and stay on it.** If you find yourself writing "also
  check...", that "also" is a second reviewer. Split it.
- **Carve out the false positives you can predict.** "A single large but cohesive
  module is not a violation." "Panics in `#[cfg(test)]` code are acceptable."
  Pre-empting the obvious wrong flags keeps false positives down.
- **Match the mode to the language.** A gate's prompt should be decisive; an
  advisor's should say "report as optional findings... do not block," so its
  output stays advisory even if the model is tempted to be firm.
- **Let the agent explore.** Every reviewer gets a full checkout and is told how to
  see the changeset (the diff against the base, plus untracked files). You do not
  need to paste the diff into the prompt; point the reviewer at the property.
- **You do not need to ask for completeness.** Bastion appends an instruction to
  every reviewer prompt telling the agent to report every distinct finding in one
  pass, not just the first. Write the prompt for the concern and phrase findings
  per instance (one per file and line range), and the agent enumerates them all so
  the author fixes the whole set from one run.

Some worked examples, taken from Bastion's own registry
([`bastion/reviewers.yaml`](../../bastion/reviewers.yaml)):

```yaml
  - name: error-handling
    trigger: [src/**/*.rs]
    mode: gate
    backend: codex
    prompt: |
      Review the changeset for error-handling discipline: no `.unwrap()` or
      `.expect()` on recoverable errors in non-test code, errors propagated with
      `?` and given context, and gates that fail closed. Block the PR if you find
      a recoverable error that can panic in production; otherwise approve it.
      Panics in `#[cfg(test)]` code and in genuinely-unreachable invariants that
      are documented as such are acceptable.

  - name: test-coverage
    trigger: [src/**/*.rs]
    mode: advisor
    backend: codex
    prompt: |
      Check whether new or changed behavior in this changeset is covered by
      tests. This is advisory: report uncovered behavior as optional findings so
      the author can decide, but do not block.
```

## Validating your registry

There is no separate lint command; the registry is validated when it loads, before
any agent runs. Run `bastion review` and Bastion will report a malformed file, a
duplicate name, or a reviewer missing a required field with a clear error.

---

Next: [The local workflow](./local-workflow.md). Running `bastion review` in
depth, the JSONL agent stream, and inspecting saved runs.

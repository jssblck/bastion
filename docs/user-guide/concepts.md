---
title: Concepts
summary: "The vocabulary Bastion runs on: reviewers, triggers, modes, verdicts, and the merge gate."
order: 3
---

# Concepts

> The vocabulary Bastion runs on: reviewers, triggers, modes, verdicts, and the
> merge gate.

This chapter defines the terms the rest of the guide uses. It is short on purpose;
each idea has a deeper home later, linked as it comes up.

## The reviewer

A **reviewer** is the unit of the system: a focused agent prompt responsible for
exactly one property of a changeset. It is a bundle of *prompt + trigger + mode*,
plus an optional execution profile (backend, timeout, environment, inputs). All of
it is declared statically in `bastion/reviewers.yaml`.

Two properties matter most:

- **Single concern.** A reviewer checks one thing and checks it well. You scale
  coverage by adding reviewers, never by widening one. This is what keeps recall
  high (see [Introduction](./introduction.md#the-core-idea)).
- **Declarative and static.** Reviewers are data, not code. Bastion never
  generates them on the fly. That keeps the trigger set stable and makes every
  reviewer reviewable -- which is the foundation of [governance](./governance.md).

## The trigger and the changeset

A reviewer's **trigger** is a list of path globs. A reviewer runs only when at
least one changed file matches one of its globs. That is what makes a hundred
reviewers cheap: a docs-only change wakes the docs reviewers and nothing else.

```yaml
trigger: [src/server/**, src/client/**]   # runs when server or client code changed
```

The **changeset** is everything in your working tree that differs from the base
branch -- *including uncommitted edits and new untracked files*, not just committed
history. This is deliberate: it lets an author loop against reviewers before
committing anything. (Locally, this means a reviewer sees your work in progress; in
CI the head is already committed, so the same definition gives the same result.)

## The mode: gate vs. advisor

Every reviewer has a **mode** that decides whether it can block a merge:

| Mode | Blocks the merge? | On crash/timeout/bad output |
| --- | --- | --- |
| `gate` | Yes, when it returns `block` | **Fails closed** -- resolves to `block` |
| `advisor` | No, ever | **Fails open** -- ignored in the aggregate |

A **gate** is a hard requirement: it must produce a clean `pass` for the merge to
proceed. If it crashes, times out, or cannot produce a valid verdict, it resolves
to a block -- never a silent pass. An **advisor** comments but never holds up the
merge; even a clean `block` verdict from an advisor is treated as a pass for
aggregation (its findings still surface). A failed advisor is simply dropped.

Use a gate for properties that must hold (tenant isolation, fail-closed error
handling). Use an advisor for guidance you want surfaced but not enforced (test
coverage, doc gaps, style preferences).

## The verdict

Every reviewer returns a structured **verdict**, captured through the backend's
structured-output mechanism (a JSON schema for Claude Code, a requested verdict
block for Codex) so Bastion can parse and aggregate it:

```yaml
verdict: pass | block    # the authoritative gate decision (ignored for advisors)
summary: "..."           # a human-friendly one-paragraph explanation
findings:                # specific, located comments
  - kind: blocking       # blocking | optional
    path: src/server/db.rs
    line_start: 88
    line_end: 91
    detail: "scope this query by tenant_id"
```

The top-level `verdict` is the decision; `findings` explain it. A `block` should
carry at least one `blocking` finding (the reason), and a `pass` may still carry
`optional` findings as non-blocking suggestions. A finding's `kind` changes how it
is *surfaced*, not whether the merge proceeds -- only `verdict` decides that.

**Findings are the actionable surface.** An agent fixing a PR gets everything it
needs from the findings: a file, a line range, and what to change. It should never
have to open a transcript to learn what to do.

## The merge gate

Bastion runs all matched reviewers in parallel (they have wildly different
latencies -- one might take 90 seconds, another 15 minutes) and **aggregates** their
verdicts into a single decision:

- **All gates must pass.** The aggregate is `pass` only when every gate returned a
  clean `pass`.
- **Any blocked, errored, or timed-out gate blocks the aggregate.** "All gates
  pass" never includes a gate that failed to produce a verdict.
- **Advisors never affect the aggregate.** They contribute findings, not gate
  decisions.

Locally, that aggregate is the exit code of `bastion review`. In CI it is the result
of the Bastion review job (and, in the target adapter, a single always-present
required check named `bastion`). Either way it is the same decision computed the
same way -- which is the whole point of running the same reviewers in both places.

## The backend

A **backend** is the agent harness a reviewer runs on. Bastion does not implement
its own agent loop; it translates the reviewer into the backend's native config and
shells out to its CLI, reusing your local auth and billing.

- `any` (the default) -- Bastion chooses; today that resolves to Claude Code.
- `claude-code` -- Anthropic's Claude Code CLI.
- `codex` -- OpenAI's Codex CLI.
- `pi` -- named but not yet wired; selecting it fails closed.

You pin a backend when a subscription's terms require a specific harness, or when
one model is simply better at a given concern. See
[Authoring reviewers](./authoring-reviewers.md#backend----choosing-a-backend) and, for CI
billing, [Continuous integration](./continuous-integration.md#authentication--billing).

## How it all fits

```text
bastion/reviewers.yaml        you author this
        |
        v
   bastion review  --->  compute changeset (working tree vs base)
        |
        v
   route: select reviewers whose trigger globs match
        |
        v
   run matched reviewers in parallel (each on its backend, each timeout-bounded)
        |
        v
   each returns a verdict (pass/block + summary + findings)
        |
        v
   aggregate: all gates must pass  --->  one decision (exit code locally; the
                                          review gate in CI)
```

---

Next: [Authoring reviewers](./authoring-reviewers.md) -- the full registry schema,
from the four required fields out to timeouts, environment, and prompt inputs.

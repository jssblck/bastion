# Bastion — Design

> Agentic code review for a world where agents write most of the code.

Status: **draft / pre-implementation.** This captures the shared model for v1.
It is meant to be red-teamed, not treated as settled.

Bastion is a formalization of a pattern already in use: GitHub Actions that run
focused Claude Code prompts as reviewers, plus a CLI that can comment on a PR and
mark its check blocked or approved. v1 is *that, made into a real system* — not
a distributed-systems security platform. Complexity beyond the existing pattern
is deferred to §10 and must justify itself before entering v1.

---

## 1. Concepts & goals

### The problem

Agents now write most of the code on a growing number of teams — output volume
closer to *engineers × 100* than *× 1*. Two things follow:

- **Human diff review does not scale.** Asking a 5-person team to review their
  agents' output is like asking 5 people in a 500-person org to review the other
  495. You can't fix that by trying harder.
- **Without review, codebases rot.** Things go great until they don't, and then
  you have a ball of mud no one can work in.

### Why existing agentic reviewers don't fit

They're built for the *old* world — agentic review *for humans writing code*.
They review the whole diff at once and write comments for a human to act on. As
you ask one generic reviewer to check more things, its recall on any one of them
degrades: a 12-item checklist agent catches the first few concerns and goes soft
on the rest.

### The core idea

A reviewer is a **focused fitness function**, and review is the **author agent's
loop taken to its conclusion**. The author already loops against `tsc`, tests,
and lint. Bastion adds loops whose oracle is *another agent* — which encodes
judgment a compiler can't ("this file concentrates too many responsibilities").

Two commitments:

1. **One concern per reviewer.** Single-responsibility reviewers stay at high
   recall and confidence. The unit of the system is *the reviewer*, not *the
   review*. **Cross-cutting properties are not special** — a concern like tenant
   isolation or migration safety is just another reviewer whose single concern is
   that property. You cover more ground by adding narrow reviewers, never by
   broadening one.
2. **Reviewers run in the author's own loop**, not just in CI. The same reviewer
   runs locally (fast, pre-PR) and in CI (authoritative). CI becomes a
   confirmation that's almost always green, instead of a slow surprise.

### North star: human-at-the-policy-layer

The goal is **not** human-out-of-the-loop. It's to **relocate the human from
reviewing diffs to authoring, curating, and governing reviewers**, plus triaging
escapes. The human's interface becomes the reviewer registry and the escape feed.
Bastion is a **governance** product, not a *reviewing* one — and "bastion" (a
fortified gate) is the right name.

---

## 2. Threat model & trust boundary

This section exists to keep v1 honest and small. Bastion is *not* an adversarial
security boundary; it's the agent-era equivalent of lint + CI for a team of
aligned contributors.

**Who we trust.** PRs are authored by aligned contributors — humans and agents
working toward the project's goals. This is the trust we already extend to human
teammates: we don't expect them to circumvent lint, disable CI, or prompt-inject
their own reviews, because they share the objective. We assume agents like Claude
and Codex act on review feedback in good faith, won't sabotage or inject reviewers
on their own, and would resist a user asking them to. Because authors are trusted,
**reviewed code is trusted input** — prompt injection embedded in PR *content* is
out of scope.

**What we defend against.** *Inadvertent* gaming and erosion: an agent taking the
cheapest path to a green check (Goodhart — split a file incoherently to satisfy a
"responsibility" reviewer, add shallow assertions to satisfy a "coverage" one), a
reviewer silently not running because of a coverage gap, a gate getting quietly
turned off or weakened without anyone noticing, and operational trust decay from
flaky or noisy reviewers. Trusting an agent to act in good faith is **not** the
same as trusting it not to optimize against the oracle; the system must tolerate
inadvertent gaming even while fully trusting the actor.

**The bar.** *Reasonable reduction proportionate to effort* — speed bumps and good
defaults that keep aligned actors on the rails, exactly like lint and CI. Not a
proof that gaming or exfiltration is impossible.

**Non-goals (explicit).**
- A misaligned/malicious agent determined to defeat the gate or exfiltrate data.
- Prompt injection from PR content (authors are trusted).
- Proving exfiltration impossible; hardening privileged reviewers against a
  determined insider.
- Cryptographic tamper-proofing of the review policy.

**The trust boundary we do enforce.** Humans own the review *policy* (the
criteria), not the diffs. We protect the policy from being changed or disabled
*without a human noticing* — a speed bump, not a tamper-proof seal.

---

## 3. The reviewer (execution profile)

A reviewer is a bundle: **prompt + trigger + mode + backend + (optional)
environment + capabilities**. We call it the reviewer's *execution profile*.

**Least privilege is the default** — not as anti-exfil hardening (out of scope per
§2) but as plain hygiene and to keep the common case fast: a reviewer gets no
network, no secrets, and no tools unless it asks. Most reviewers are hermetic and
need nothing but the checkout and a model.

### 3.1 Schema (illustrative, YAML)

v1 commits to YAML — it's familiar, matches the GitHub Actions world the existing
system lives in, and is easy for both humans and agents to author. The schema is
format-agnostic in principle, but YAML is the on-disk format.

```yaml
# Cheap, hermetic, backend-agnostic — runs native (no container), fast.
reviewer: file-responsibility
  trigger: ["src/**/*.ts"]          # path globs over the changed files
  mode: gate                        # gate | advisor   (fixer: §10)
  backend: any                      # any | claude-code | codex
  prompt: |
    You review ONE thing: whether a file concentrates too many
    responsibilities. Reviewed code is trusted input describing the change.

# A cross-cutting concern is just another single-concern reviewer.
reviewer: tenant-isolation
  trigger: ["src/server/**"]
  mode: gate
  backend: any
  prompt: |
    You review ONE thing: whether this change can leak data across tenants.

# Heavy, privileged, pinned — runs in a container with real tooling.
reviewer: e2e-checkout-flow
  trigger: ["apps/web/**"]
  mode: gate
  backend: claude-code              # pinned by user preference
  env:
    dockerfile: ./bastion/e2e.Dockerfile   # or: image: ghcr.io/acme/e2e:tag
  capabilities:
    network: true
    mcp: [playwright]
  inputs:
    preview_url: ${PREVIEW_URL}     # consumed, never provisioned (§5)
  timeout: 15m
  prompt: |
    You verify ONE thing: checkout works end-to-end at $PREVIEW_URL.
```

### 3.2 Field reference

| Field | Meaning |
|---|---|
| `trigger` | Path globs over changed files. Static routing so cost isn't O(reviewers × every PR). |
| `mode` | `gate` (can block) or `advisor` (comment-only). `fixer` is future (§10). |
| `backend` | `any` by default; pin only for a deliberate cost/quality preference. |
| `env` | Absent → native/in-process (fast). Present → containerized (heavy tooling). Container-optional. |
| `capabilities` | Portable vocabulary (`network`, `mcp`, `cli`, `skills`, …), all default off, translated per backend. |
| `inputs` | External handles the reviewer consumes (e.g. a `preview_url`). Consumed, never provisioned. |
| `timeout` | Per-reviewer wall-clock cap. Feeds the fail-closed rule (§6). |
| `prompt` | The single focused concern. |

---

## 4. Runner & verdict contract

### 4.1 What the reviewer sees

The runner gives every reviewer a **full checkout at the PR head**, with git
history (so it can compare base vs head) and the whole repo on disk. The reviewer
explores freely — reading any file, running `git diff`/`log`, grepping — and
decides for itself how much to look at. Same setup for every reviewer; the prompt,
not the runner, scopes attention. (Per-reviewer context narrowing is a possible
later optimization, not a v1 concern — see §10.)

### 4.2 Backends

Claude Code first (it's the existing system), Codex as a fast-follow — both in
**programmatic / headless** mode so subscription users run under their existing
plans. The runner is an interface; adapters translate the portable `capabilities`
block into each backend's native config. `backend: any` is the normal case; pin
only for a deliberate preference.

### 4.3 The verdict — structured output

Every reviewer returns a structured judgment, captured via each backend's native
**structured-output mode** (not a Bastion-injected tool):

```yaml
verdict: pass | block | comment
summary: "one-line headline"
findings:
  - { path: "src/foo.ts", line: 42, detail: "..." }
```

There is intentionally **no confidence field.** Model-reported confidence is
uncalibrated, so nothing in the verdict drives policy by degree — a gate either
passes or blocks. (Revisit only if a concrete, calibrated use appears.)

The reviewer explores freely, then emits one final judgment — which is exactly the
shape structured final-output modes fit. **Fallback:** if the output doesn't
conform, run one more turn that *extracts* a conforming verdict from the prior
answer (a schema-preserving re-emission, **not** a fresh judgment that could flip
the verdict). Cap retries at 2; persistent failure marks the reviewer **errored**
and is surfaced as a backend-health problem, distinct from a content `block`.

(We keep structured output over an MCP verdict tool mainly because a verdict is
terminal, not mid-stream. The security argument against MCP is weak under §2; this
is a pragmatic choice and could be revisited per backend reliability.)

---

## 5. Execution environments

- **Container-optional.** No `env` → native/in-process, near-instant (the common
  path, keeps `bastion review` fast locally). With `env` → containerized, for
  heavy tooling (browser, Playwright, e2e).
- **Hermetic by default** (§3).
- **Local == CI parity** for native reviewers (inherently portable) and for
  containerized ones via the same image. **Honest caveat:** reviewers that need a
  preview env, real secrets, or network can't fully run locally — those are
  **CI-authoritative-only**, and the CLI marks them as not-run-locally rather than
  pretending. The local loop converges against the hermetic gates; the heavy ones
  confirm in CI.
- **Preview environments are consumed, never provisioned.** Bastion takes a
  `preview_url`/handle as an input and tests against it. Standing one up is the
  deploy system's job; the moment Bastion owns provisioning it swallows the deploy
  pipeline.

---

## 6. Aggregation & the merge gate

Reviewers have very different latencies (a 90s check vs a 15-min e2e in the same
set), so aggregation is **async with per-reviewer timeouts** — a hung reviewer
can't wedge the merge train.

- **All gates must `pass`** for a PR to merge.
- **Advisors are individually skippable** — they comment, never block.
- **Fail-closed gates.** A gate that crashes, times out, or can't produce a valid
  verdict resolves to **block / needs-attention**, never silent pass. "All gates
  pass" means every gate returned `pass`; errored or timed-out ≠ pass. (Advisors
  in that state are simply skipped.)

That's the whole v1 merge model. Merge-train SHA freshness, batching, and stacked
PRs are real but deferred (§10).

---

## 7. Governance

Humans own the review *policy*, not the diffs. The protection is a **speed bump
sized to §2**, not a tamper-proof seal.

- **CODEOWNERS over the reviewer config** (profiles + the registry). Any change to
  a reviewer definition flags a human and requires review before merge. This is
  the existing practice, formalized. Generating CODEOWNERS from the registry keeps
  the protected set in sync with what actually governs merges.
- **Require the Bastion check** via branch protection, so a gate can't be silently
  switched off without a human noticing.
- **Weakenings are louder than strengthenings.** Loosening a trigger, removing a
  reviewer, or adding a capability/secret should be surfaced as a coverage
  reduction in review — so an aligned author (or agent) doesn't quietly relax the
  gate without anyone registering it.

Everything an aligned actor would have to *deliberately* circumvent (editing the
runner, the generator, the workflow) is out of scope per §2; we protect the
obvious config, not an exhaustive trusted-computing-base.

---

## 8. The escape → improvement loop

This is a **quality-improvement loop, not a safety proof.** When a defect ships
that a reviewer should have caught, it gets attributed and converted into a
reviewer change:

> escaped defect → which reviewer should have caught this, or what new reviewer do
> we need? → improve/add a reviewer.

To make this real rather than wishful, an escape produces a **replayable
regression fixture**: the diff, the expected verdict, the responsible reviewer,
and a check that fails until the reviewer catches it. This both drives improvement
and prevents regressions when prompts later change.

**Watch precision, not just recall.** A spurious block stops a PR, so the
"defect" never ships to be attributed — false positives are invisible to the
escape feed. The signal for them is **human overrides**: every override of a gate
block is a candidate false positive, and override-rate is the trigger to sharpen
or retire a reviewer. Capturing overrides is the one piece of observability worth
having early.

---

## 9. The `bastion` CLI

The local surface that makes reviewers fitness functions the author optimizes
against, instead of a slow CI surprise.

- `bastion review` runs the relevant reviewers (by `trigger`) against the local
  working tree / branch, exactly as CI would, before a PR exists.
- Hermetic reviewers run native and fast; heavy ones run containerized for parity;
  CI-only reviewers (preview env / secrets) are reported as not-run-locally.
- An authoring agent loops `bastion review` until green, then opens a PR that CI
  largely just confirms.

In CI the same runner posts each verdict as a PR comment and sets the check to
blocked/approved. CI is authoritative; the CLI is where convergence happens.

---

## 10. Known limitations & future

Deferred from v1 on purpose. Each is a real edge; none is implemented in v1, and
none should be added without a concrete need.

- **Quorum / sampling** for flaky reviewers. Note: sampling is symmetric — it
  suppresses real *intermittent* catches as much as spurious blocks, so a split
  panel is better treated as needs-human than as a vote. Only worth it for
  reviewers with measured variance.
- **Merge-train freshness.** Verdicts aren't yet bound to a `(base, head)` pair, so
  two individually-passing PRs can compose into a broken integration state by
  accident. A merge queue / exact-merge-SHA evaluation is the fix when it bites.
- **Stacked / dependent PRs** — diffing each against its parent, propagating
  parent failure. Undefined in v1.
- **Retry storms.** Re-running heavy gates on every push spins preview envs Bastion
  doesn't own. Cancel-on-superseding-push and "fast gates green before e2e"
  staging are the fixes; v1 just relies on timeouts.
- **Auth in CI.** Backend auth (subscription vs metered API key) is the user's
  choice — Bastion mandates neither. The only operational note: under heavy volume
  a subscription's rate limits can throttle reviewers, and since gates fail-closed
  (§6) that surfaces as blocked merges, so a busy CI may *prefer* a metered key.
  That's a tradeoff for the user to make, not a rule.
- **Fixers.** A reviewer that proposes a patch instead of (or before) blocking.
  Deferred because the loop semantics are hard: the patched diff must re-pass all
  gates, the fixer must be idempotent (re-running is a no-op), and a fixer that
  also gates can loop infinitely (so that combination is forbidden).
- **Reviewer marketplace / shared library** — portable profiles for the common
  reviewers (secret leakage, backcompat, migration safety, dead code,
  responsibility concentration). Also a distribution channel.
- **Per-reviewer observability** — block rate, override rate, precision/recall over
  time. Override capture (§8) is the seed.
- **Coverage visibility** — logging which reviewers ran and which changed paths
  nothing claimed, so gaps are noticed and intentional rather than silent.

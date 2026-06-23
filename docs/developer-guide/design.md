# Bastion: core design

> Agentic code review for a world where agents write all of the code.

This is the authoritative design reference: reviewers, the verdict contract, the
merge gate, and the threat model. It is CI-agnostic; the [GitHub
adapter](./github-adapter.md) and the [local surface](./local-surface.md) are the
two concrete surfaces built on top of it. For a task-oriented introduction aimed
at people *using* Bastion, see the [user guide](../user-guide/README.md).

Bastion is a formalization of a pattern already in use privately: GitHub Actions that run focused agent prompts as reviewers, plus a CLI that can comment on a PR and mark its check blocked or approved. v1 is _that, made into a real system_. Complexity beyond the existing pattern is deferred to the Known limitations section and must justify itself before entering v1.

---

## Concepts & goals

### The problem

Agents now write most of the code on a growing number of teams. Output volume is closer to _engineers x 100_ than _x 1_ when fully unlocked. Two things prevent teams from fully unlocking:

- **Human diff review does not scale.** Asking a 5-person team to review their agents' output is like asking 5 people in a 500-person org to review the other 495. You can't fix that by trying harder.
- **Without review, codebases rot.** Things go great until they don't, and then you have a ball of mud no one can work in.

### Why existing agentic reviewers don't fit

There's a lot of existing code review agents, like GitHub Copilot or CodeRabbit. They do a decent job.

But they're built for the _old_ world: agentic review, yes, but _for humans writing code_.
They obviously market themselves as for agents too, but they fundamentally are designed
in such a way that they are not well-suited for the _new_ world of agentic code review.

They review the whole diff at once and write comments, really clearly designed for a human to act on.
And they don't really scale: as you ask one generic reviewer to check more things, its recall on any one of them
degrades. A 1-item checklist agent works great, at 10 items it's less effective, at 100 items it's not effective at all.

This is because attention is scarce: it's scarce for humans, and it's still scarce for agents.
This is unlikely to change as agents get smarter; intelligence doesn't seem to be correlated with attention so far.
If anything, the smarter the models get, the more focused their attention seems to be, much like we see in humans.

### The core idea

Bastion proposes a new approach to agentic code review. The intention is that after adopting Bastion,
teams are unlocked and high confidence. They're able to merge code changes without human review,
and without turning the codebase into mud.

In Bastion, a reviewer is a **focused fitness function**, and review is the **author agent
loop taken to its conclusion**. The author agent already loops against the compiler, linter, tests, and more.
Bastion adds loops whose oracle is _another agent_, which encodes judgment a compiler or a linter or a test can't.

What does that look like in practice? The core principles are:

1. **One concern per reviewer.** Single-responsibility reviewers stay at high recall and confidence. The unit of the system is _the reviewer_, not _the review_. Cross-cutting properties are not special: a concern like tenant isolation or migration safety is just another reviewer whose single concern is that property. You cover more ground by adding narrow reviewers, never by broadening one.
2. **Reviewers run in the author's own loop**, not just in CI. The same reviewer runs locally (fast, pre-PR) and in CI (authoritative). CI becomes a confirmation that's almost always green, instead of a slow surprise.
3. **Human at the policy layer.** The goal is not human-out-of-the-loop. It's to _relocate the human from reviewing diffs to authoring, curating, and governing reviewers_, plus triaging escapes. The human's interface becomes the reviewer registry and the escape feed. Bastion is best thought of as a product of _governance_ or _consensus_. The reviewers are the agents you already trust.
4. **Even aligned agents acting in earnest can inadvertently game the system.** The system must tolerate this, and the human governance layer must be able to detect and correct it. The goal is not to categorically prevent gaming; this is likely impossible without giving up the very benefits of agentic development in the first place. The goal is to make gaming visible when it happens and to make it easy to fix by adjusting the reviewers.
5. **Reviewers converge over time.** Start with a reviewer that's good enough, then sharpen it from the escapes you hit in production. This is the escape-to-improvement loop. The human team owns the reviewers and adjusts them as escapes come in. No reviewer ever gets everything right, so the team keeps updating them from feedback in the codebase and from agent-authored PRs.

Bastion also makes two non-guarantees explicit:

- **No guarantee of correctness.** Bastion does not guarantee that the code is free of bugs or security vulnerabilities; it's like human code review without the human. The agent code reviewers can only be as good as the underlying model they are using, and from there only as good as the prompt directing their review.
- **No guarantee that the right thing is being built.** Catching "this is the wrong thing to build" was never review's job, human _or_ agent. By PR time the ship has sailed; that's a design-time question. Bastion doesn't change that: keep humans in the design loop, and point your agents at what the project needs; Bastion operates under the implicit assumption that all PRs are at the very least directionally aligned with the goals of the project.

---

## Threat model & trust boundary

Bastion is _not_ an adversarial security boundary; it's the agent-era equivalent of
team code review for aligned contributors.

Think of how code review was done at most companies before agentic development:

1. A human author writes a PR.
2. A human reviewer reads the PR and provides feedback; tools also provide automated feedback. The human reviewer assumes the author is broadly aligned with the project's goals and is trying to do their best, so the reviewer tries to provide constructive feedback and guidance. At the same time, they cannot guarantee alignment, so formal approval is withheld until the reviewer is satisfied. Some of the feedback is optional, and some is blocking. The human reviewer explicitly notes which items are blocking and which are optional.
3. The human author fixes some or all of the comments and requests a re-review; the expectation is that all blocking items are resolved, where "resolved" might mean that the original author changed the code to address the feedback _or_ that the author convinced the reviewer that the approach is actually correct and should be approved.
4. The human reviewer reviews again, and the process repeats until the PR is approved or abandoned.

The intention of Bastion is to bring this same review process to the agent era,
so that AI agents can collaborate with human reviewers to ensure code quality and
security.

Formalized, Bastion is built around the following threat model:

1. **PRs are authored by aligned contributors.** Humans and agents that create PRs are assumed to be earnestly working toward the project's goals to the best of their ability. In the human world, this would look like "we assume contributors are not disabling lints or CI or obfuscating code; they appreciate the review process because it helps the team move faster". Bastion is built around the model that agents like Claude and Codex are well aligned: they act on review feedback in good faith, won't sabotage or inject reviewers on their own, and would resist a user asking them to.
2. **Because authors are trusted, reviewed code is trusted input.** Bastion does not try to protect reviewer agents against prompt injections or data exfiltration from the code they review. The system is designed to be robust against _inadvertent_ gaming and erosion, not against a deliberately malicious actor.
3. The bar is **reasonable reduction proportionate to effort**. Bastion is not a proof that gaming or exfiltration is impossible; it's a speed bump and good defaults that keep aligned actors on the rails, exactly like lint and CI and human review for a human author. The goal is to make it easy for aligned contributors to do the right thing, and hard for them to do the wrong thing, without trying to make it impossible.
4. **Humans own the review policy.** Humans are responsible for the reviewers, the prompts, and the triggers. They are responsible for triaging escapes and improving the reviewers over time. The system is designed to make it easy for humans to govern the review process and to make it easy for them to detect and correct any gaming or erosion that happens. For this reason, any PR that modifies reviewer policy (the reviewer registry, the prompts, the triggers) requires human review before it can be merged. This ensures that humans are always in the loop and can catch any changes that might weaken the review process.

---

## The reviewer

A reviewer is a bundle: **prompt + trigger + mode + backend + capabilities + (optional) runner + (optional) environment**. We call it the reviewer's _execution profile_. The optional `runner` provisions a container the backend runs inside (see the [honored-fields table](./backends.md#what-a-backend-applies-from-the-profile-today) and [Containers](./containers.md)); without it the reviewer runs natively on the host.

**Least privilege is the default.** This isn't intended as anti-exfil hardening but as plain hygiene and to keep the common case fast: a reviewer gets no secrets and no tools unless it asks. Most reviewers are hermetic and need nothing but the checkout and a model. Access to the model provider is always permitted, since every reviewer needs it; `network: true` is the opt-in for _general_ outbound network beyond that. (One caveat in the current build: network scoping is not yet enforced, so a containerized reviewer attaches the engine's default network whether or not it asks. The `network` flag is recorded but does not yet restrict egress; see the implementation-status note below and the [honored-fields table](./backends.md#what-a-backend-applies-from-the-profile-today).)

Reviewers are **composable**. They run independently and asynchronously, and their verdicts are aggregated at the end. This means you can add a new reviewer for a concern without affecting the existing ones, and you can have some reviewers that run fast and others that run slow without blocking the whole process.

Reviewers are **declarative and static**. They're defined in a config file, not generated on the fly by code. This makes them reviewable and ensures that the trigger set is stable. It also means that any change to a reviewer definition requires human review, which helps prevent accidental or malicious weakening of the review process.

Finally, Bastion does not own CI. One of the examples below indicates a preview URL; Bastion doesn't set this up, the example reviewer just expects it to be there. The reviewer is responsible for defining its own execution environment, and the CI workflow is responsible for providing it. This keeps Bastion flexible and allows teams to integrate it with existing CI setups.

### Schema

The schema is format-agnostic in principle, but YAML is the on-disk format we start with because it's human-friendly and widely used for config. The important part is that it's **declarative and static**: no code, no dynamic logic, so it's reviewable and generates a stable trigger set.

The registry is a single top-level `reviewers:` list; each entry is one reviewer
keyed by `name`. (This is the on-disk shape the loader expects; see
[`src/config.rs`](../../src/config.rs) and [`.bastion.yaml`](../../.bastion.yaml).)

```yaml
reviewers:
  # Runs native (no container), fast and cheap.
  - name: file-responsibility
    trigger: ["src/**/*.ts"]          # path globs over the changed files
    mode: gate                        # gate | advisor | ...
    backend: any                      # any | claude-code | codex | ...
    prompt: |
      Review the changeset to determine whether any one file concentrates too many
      responsibilities. If so, block the PR and point out which file(s) and why; otherwise, approve it.

  # A cross-cutting concern is just another single-concern reviewer.
  - name: api-compatibility
    trigger: ["src/server/**", "src/client/**"]
    mode: gate
    backend: any
    prompt: |
      Review the changeset for API compatibility between the currently deployed production client and server.
      Production OpenAPI spec is at `https://api.acme.com/v1/openapi.json`.
      If you find any breaking changes, block the PR and explain; otherwise, approve it.

  # Heavy and privileged: runs in a container with real tooling.
  - name: e2e-checkout-flow
    trigger: ["src/**"]
    mode: gate
    backend: claude-code                     # pinned by user preference. optional; `any` by default.
    timeout: 15m
    runner:
      dockerfile: ./.bastion/e2e.Dockerfile   # builds a hermetic image with tools installed. optional within `runner`; if absent, falls back to `image`. (Omit the whole `runner` block to run native; a `runner` with neither source fails closed.)
      image: ghcr.io/acme/e2e:latest         # alternative to `dockerfile` for a pre-built image. optional; if both `dockerfile` and `image` are present, `dockerfile` takes precedence.
    env:
      PREVIEW_URL: http://preview.internal   # literal environment variables injected into the reviewer process. optional.
    capabilities:
      network: true                          # containers always need some network for the model provider; this enables general network access.
      mcp: [playwright]                      # loads MCPs needed by the review into the agent's context, and gives permission to call them.
      skills: [checkout-flow, browser]       # loads skills needed by the review into the agent's context.
    inputs:
      preview_url: http://preview.internal   # values interpolated into the prompt (`${preview_url}`) by Bastion before handing off to the agent.
    prompt: |
      Run the e2e checkout flow against the preview environment at `${preview_url}` using Playwright.
      If it fails, block the PR and explain; otherwise, approve it.
```

> **Implementation status.** This schema is the design target. In the current
> build, `name`, `trigger`, `mode`, `backend`, `prompt`, `timeout`, `env`,
> `inputs`, and `runner` (containers) are honored: a reviewer with a `runner` block
> runs its backend inside the built or named image (see
> [Containers](./containers.md)). `capabilities.network: true` is honored inside a
> container but not yet scoped (egress allowlisting is a later milestone), and a
> native `network: true` fails closed; `mcp` and `skills` parse but are **not yet
> provisioned**, so a reviewer that declares one **fails closed** (a gate blocks, an
> advisor is skipped) rather than running without it. The least-privilege default
> (`network: false`, no `mcp`/`skills`, no `runner`) runs natively. `env` and
> `inputs` values are literal strings (no shell `$VAR` expansion). See the
> [honored-fields table](./backends.md#what-a-backend-applies-from-the-profile-today).

---

## Runner & verdict contract

### What the reviewer sees

The runner gives every reviewer a full checkout at the PR head, with request metadata such as "what is the base branch" and "what is the PR description".
The reviewer explores freely like any coding agent and decides for itself how much to look at.

Same setup for every reviewer. The prompt, not the runner, scopes attention.

### Agent backends

Bastion supports Claude Code, Codex, and Pi as first-class harnesses.

Instead of running its own agent loops, Bastion supports existing tooling as backends. The runner translates the reviewer's execution profile into the backend's native config, and Bastion's CI workflow calls the backend's CLI to run the review. This keeps Bastion simple and lets it leverage the strengths of each backend, as well as supporting subscription-based usage that requires users to run on a specific backend.

For local usage, a native reviewer reuses the same configs the user has configured locally for the harness being used, so the billing or other configuration the host CLI already holds is reused in the reviewer agents. A containerized reviewer (one with a `runner`) does not get that host config: Bastion bind-mounts only the checkout and forwards the reviewer's literal `env` plus a fixed set of provider-credential variable names, so the in-container agent authenticates from those forwarded credentials (or from auth baked into the image), not from the host's `~/.claude` / `~/.codex`. See [Containers](./containers.md).

To comply with subscription terms of service (which tie a subscription to an individual, not a team) in CI, Bastion can be configured with mappings for different authentication to use per reviewer. Bastion does not store these subscription details; teams must store these separately. For example, GitHub Actions secrets can be used to store API keys or subscription details, and the Bastion runner can be configured to read different secrets depending on the user making the request in CI. Bastion can also optionally default to API billing if no subscription is configured.

### CI backends

Bastion supports local execution and GitHub Actions as first-class CI backends.

Bastion is designed to be portable so that it can run locally as well as in CI; for this reason Bastion config does not specify CI details.
Where Bastion interacts with CI systems, it does so using a plugin-style interface that allows it to integrate without being tightly coupled; the GitHub implementation of that interface is specified in [Bastion on GitHub](./github-adapter.md).

Since Bastion supports local execution, technically any CI that allows arbitrary code execution can be made to work with Bastion, and more may be supported over time.

### The verdict

Every reviewer returns a structured judgment, captured via each backend's native
**structured-output mechanism**, with a stable schema that Bastion can parse and aggregate. (In the current build that is a JSON schema for Claude Code and a requested fenced verdict block for Codex.) The schema is:

```yaml
verdict: pass | block   # Ignored for "advisor" style reviewers, which always functionally "pass" with reported findings.
summary: "..."          # Human-friendly review summary.
findings:               # Allow the reviewer to point to specific files and lines with blocking or optional comments.
- kind: "blocking"
  path: "src/foo.ts"
  line_start: 42
  line_end: 42
  detail: "..."
- kind: "optional"
  path: "src/bar.ts"
  line_start: 24
  line_end: 25
  detail: "..."
```

The top-level `verdict` is the authoritative gate decision; `findings` explain it. A `block` should carry at least one `blocking` finding (the reason it blocked), while a `pass` may still include `optional` findings as non-blocking suggestions. A finding's `kind` affects how a comment is surfaced, not the gate outcome; only `verdict` decides that.

A reviewer enumerates every finding it can identify for the changeset in one pass, one per distinct instance, rather than stopping at the first. The author can then fix the whole set from a single run instead of paying a fresh review cycle per issue. This is a property of how each backend asks for findings (a shared exhaustive-findings instruction appended to the prompt), not of the gate logic, so a clean changeset still returns `pass` with no findings. The verdict schema itself caps nothing: `findings` is an unbounded list.

Bastion requests the structured output, then parses the final agent turn against the schema requested. If the reviewer agent doesn't provide complying output, Bastion re-runs the same session with a new turn explaining the schema again and asking for just the structured output of the work already performed.

Reviewer agents that continually fail (either unable to produce structured output, timeouts, or simple execution failures) are failed closed if they are a gate, and skipped if they are an advisor.

---

## Aggregation & the merge gate

Reviewers may have very different latencies: one might be a 90 second check, another might be a 15 minute e2e test.
Given this, aggregation is async with per-reviewer timeouts and error handling; a hung reviewer can't wedge the merge train.

- **All gates must `pass`** for a PR to merge.
- **Fail-closed gates.** A _gate_ that crashes, times out, or can't produce a valid verdict resolves to **block / needs-attention**, never silent pass. "All gates pass" means every gate returned `pass`; errored or timed out is not a pass.
- **Fail-open advisors.** An _advisor_ that crashes, times out, or can't produce a valid verdict is ignored in the aggregate verdict. Advisors are best-effort and do not block, so they don't need to fail closed.

---

## The escape-to-improvement loop

An "escape" is a PR that gets merged erroneously, i.e. it should have been blocked by a reviewer but wasn't. Escapes are inevitable, especially early on when reviewers are still being tuned, but they are also the most valuable source of information for improving the system.

When an escape is detected (either through monitoring, user reports, or other means), it should be triaged to understand which reviewer(s) failed to catch it and why. This information can then be used to improve the reviewers, either by adjusting their prompts, adding new reviewers for missed concerns, or improving their execution environments.

Bastion cannot, itself, detect escapes: if it could, it would prevent them in the first place. This is a governance story: humans are responsible for monitoring the system, detecting escapes, and improving the reviewers over time based on real-world feedback. The system is designed to make it easy for humans to govern the review process and to make it easy for them to detect and correct any gaming or erosion that happens.

The main reason this is mentioned in this design at all, when it is really a human governance story, is to emphasize that the system is designed to be iteratively improved based on real-world feedback, and that escapes are not a failure of the system but rather an expected part of the process that provides valuable information for improvement. It's also to emphasize that this is a critical part of successfully deploying Bastion in a project.

---

## The `bastion` CLI

The local CLI surface makes reviewers fitness functions agent authors can optimize against locally prior to even opening a PR.

`bastion review` runs the relevant reviewers (by `trigger`) against the local working tree / branch, exactly as CI would. Progress and verdicts are rendered in the CLI output for agents to read. Since Bastion is CI-agnostic, things like environment variables are expected to be provided to Bastion in the local environment. For example, a `precommit` script might boot and run the service being reviewed locally to provide the `PREVIEW_URL` to Bastion-based reviewers, but the preview URL is just something like `http://localhost:3000` instead of something more formal. A native reviewer inherits that local environment directly. A containerized reviewer (one with a `runner`) inherits none of it; only its literal `env` pairs plus the fixed provider-credential set cross into the container, so a dynamic local value must be written into the reviewer's `env` (templated, if it is not known at authoring time). See [Containers](./containers.md).

The intention is that an authoring agent loops `bastion review` until green, then opens a PR that CI largely just confirms.

---

## Known limitations & future

Deferred from v1 on purpose, but may be added in the future.

- Sampling for flaky reviewers.
- Quorum for reviewer councils.
- Reviewer graph ordering expensive and slow reviews behind cheap and fast reviews.
- Reviewer marketplace.
- Observability and statistics.
- Coverage visibility per reviewer.

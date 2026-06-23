# Bastion on GitHub

> The GitHub adapter: how Bastion runs in Actions, reports to PRs, and gates merges.

The core design ([`design.md`](./design.md)) is deliberately CI-agnostic; it describes reviewers, verdicts, and the merge gate without saying how any of it touches a real forge. This doc is the GitHub adapter: the concrete answer to "where does the workflow live, how does a verdict become a check, and how is the policy layer enforced" when the forge is GitHub. Everything here is one implementation of the plugin-style CI interface the core design refers to; another forge would get its own doc and reuse the same core.

> **Implementation status.** This document describes the *target* adapter, and most
> of it is implemented. The self-hosted workflow in
> [`.github/workflows/bastion.yml`](../../.github/workflows/bastion.yml) runs
> `bastion review`, gates on the job's exit code, and then runs `bastion github
> report` to post the results: a sticky PR comment carrying every reviewer's verdict
> and findings (optional ones included), one check run per reviewer, and the
> always-present aggregate `bastion` check. The full run is uploaded as an
> artifact too. Not implemented: findings as *inline* diff comments (in this release
> they ride the sticky comment and check annotations), live mid-run progress (spinners
> and the rewritten aggregate table, which need the engine to talk to the API during
> the run), and the packaged `review-action`/GitHub App. Bastion's GitHub helpers
> are `bastion github codeowners` and `bastion github report`.

The guiding rule is the same as the core: Bastion does not own CI, it plugs into yours. The workflow, the secrets, the preview environments, and the branch protection rules are GitHub's; Bastion reads and writes them through a thin adapter and otherwise stays out of the way.

---

## How it runs

Bastion runs as a GitHub Actions workflow triggered on pull request events: `opened`, `synchronize` (a new push to the PR), and `reopened`. On each event the adapter:

1. Computes the changed file set for the PR.
2. Routes: selects the reviewers whose `trigger` globs match the changed files.
3. Runs each selected reviewer through its backend, in parallel, with per-reviewer timeouts (see the core design's _Aggregation & the merge gate_).
4. Reports each verdict back to the PR.

Native reviewers run directly on the Actions runner. A reviewer that declares a container `runner` runs its backend inside that container on the Actions runner (the engine is already present on GitHub-hosted runners); see [Containers](./containers.md). None of routing or aggregation is GitHub-specific; only the steps that read the PR and write results go through the adapter.

---

## Reporting verdicts

A verdict (the core schema: `verdict`, `summary`, `findings`) maps onto two GitHub surfaces.

- **Findings become PR review comments.** Each finding is posted as an inline comment on its `path` and line range. `kind: blocking` and `kind: optional` are rendered differently so a reader can tell at a glance which comments hold up the merge and which are suggestions; this mirrors how a human reviewer marks some comments blocking and some optional.
- **The verdict becomes a check run.** Each reviewer reports a check run named after itself (`bastion / file-responsibility`), so the PR's checks list shows exactly which reviewers ran and how each landed. A gate that blocks reports a `failure` conclusion; a gate that passes reports `success`; an advisor reports `success` with its findings attached, because advisors comment but never gate.

The summary and the full finding list also go into each check run's output, so everything is visible from the Checks tab even before you scroll the diff.

The PR comments are the surface the implementing agent is meant to read. A reviewer's actionable feedback is its findings, and findings are inline comments; an agent fixing the PR gets everything it needs to act from the comments alone, without opening a single check. The check runs carry status and the gate, for humans watching and for the merge logic; the detail page, transcript included, is there for the occasional surprising decision worth investigating. None of it is required reading to act on a review. An agent should never have to open a check and read a transcript just to learn what to change; the comment already says it.

> **What is implemented.** `bastion github report` surfaces findings two ways: every finding (blocking and optional) is rendered into a single *sticky* PR comment, and each located finding is also attached to its reviewer's check run as an annotation on its `path` and line range. True *inline diff review comments* (the first bullet above) are not implemented; the sticky comment is the surface an implementing agent reads, and it carries the same findings. The per-reviewer and aggregate check runs ship. The report reads the run that `bastion review` persisted and renders the recorded outcome, the aggregate `bastion` check carrying the recorded `run.completed` verdict (a recorded pass goes green, a recorded block fails, and a run that never completed reads as an incomplete failure). It trusts that recorded run because the runner already decided it. The runner fails a gate closed at write time and clamps advisors to a pass, so the report does not re-derive the merge gate. The one boundary it still checks is gate-verdict consistency. A gate row recorded as a pass that nonetheless carries a blocking finding contradicts itself, so the report fails it closed rather than publishing a green check; the backends already reject such a verdict upstream, so this is a boundary safeguard, not a recomputation of the gate. The per-reviewer *detail* layout below ships only in part; each item notes what is and is not in the shipped check output.

### The aggregate check

There's a wrinkle GitHub forces on us. Branch protection requires you to name the checks that must pass, but Bastion's set of reviewers varies per PR; a docs-only PR and a server PR trigger different reviewers, so there is no fixed list of check names to require.

The fix is a single always-present check, `bastion`, and it is the only one branch protection requires. It always runs, even when zero reviewers match (a trivial pass in that case), so it is a stable required check. Internally it reflects the aggregate: `success` only when every triggered gate passed, and `failure` if any gate blocked, errored, or timed out (fail-closed, per the core design). The per-reviewer check runs stay informational; `bastion` is the gate.

### Live progress

> **Live progress is a target.** None of the live progress below is implemented. `bastion github report` runs *after* `bastion review` has finished, so it creates each check run already `completed` with its final conclusion: there are no `in_progress` spinners, and the aggregate `bastion` check is posted once, completed, never PATCHed mid-run. Live progress needs the engine to talk to the API while reviewers are still running (a packaged action or GitHub App); the rest of this section describes that target.

Reviewers can take anywhere from seconds to many minutes, so a PR must never look like it hung. GitHub gives us live status for free through check runs, and we lean on that rather than building anything external.

- **Per-reviewer spinners.** Each reviewer's check run is created with `status: in_progress` the moment it is dispatched, so GitHub renders a live spinner next to it in the PR's checks list; an `e2e-checkout-flow` reviewer shows as in progress with a spinner for its full 15 minutes instead of reading as a stall. When the reviewer resolves its check run flips to `completed` with the right conclusion.
- **A live aggregate table.** The `bastion` check stays `in_progress` until every reviewer resolves, and Bastion PATCHes its `output` as each one finishes; the output holds a table of every triggered reviewer with its mode, current status, and elapsed time, rewritten on each update. This is the at-a-glance view: one place to see what is running, what passed, and what blocked, updating as it goes.
- **A permanent run summary.** Step summaries (`$GITHUB_STEP_SUMMARY`) do not update while a step runs; GitHub only captures them when the step finishes. So we don't use them for progress; we write one at the very end of the job as a permanent rendered report on the run summary page, which is the one thing step summaries are good at.

One mechanical note shapes the layout: check run _annotations_ are appended on each update and cannot be replaced, while the `output` summary and text _are_ replaced on each update. So the live, rewritten table lives in the check `output`; annotations are reserved for the final per-line findings, which only need to be written once.

### Reviewer detail

Each reviewer's check run is also where its detail lives; a reader clicks "Details" on that reviewer in the checks list and lands on a page Bastion owns the markdown for. This is for humans and for the occasional surprising decision, not part of the implementing agent's normal loop; the agent already has the feedback in the comments. We present three things there, in order.

- **Metadata and decision.** A short header: the reviewer name, its mode (`gate` or `advisor`), the backend it ran on, and how long it took; then the verdict and summary. The check run _title_ carries the one-line decision ("Blocked: `src/foo.ts` concentrates three responsibilities") so it is legible without opening anything. *(Target:* the matched trigger globs and whether it ran native or in a container are part of the intended header but are not in the shipped check output.*)*
- **Session transcript, collapsed.** *(Target.)* The full agent session is included inside a `<details>` block, so it is collapsed by default and one click to expand; most readers never need it, but when a decision is surprising the transcript is right there to explain it. Transcripts can be long and the check `output` is capped at 64KB, so an oversized transcript is truncated with a note pointing to the full job logs. *Implemented:* `bastion github report` does not embed transcripts in the check output; the full run, transcripts included, is uploaded as the workflow artifact, and the sticky comment footer points there.
- **Tokens and cost, when available.** When the backend reports usage, a small table shows input and output token counts and the session cost; when it doesn't, the block is omitted rather than shown empty. This is per reviewer, so an expensive e2e reviewer and a cheap hermetic one are each individually accountable.

*(Target.)* The aggregate `bastion` check links each row of its table to the matching reviewer's check run, so the table doubles as the index into all of this detail. *Implemented:* the aggregate check renders a plain Markdown table of the triggered reviewers with columns `Reviewer`, `Mode`, `Verdict`, and `Summary`, and no per-row links.

A sketch of a reviewer's check output:

```markdown
> - Check: tenant-isolation
> - Kind: gate
> - Agent: claude-code
> - Matched: `src/server/**`
> - Runner: `native`
> - Duration: 38s

**Blocked:** A new query path reads rows without scoping by tenant id.

<details>
<summary>Session transcript</summary>

...full agent session...

</details>

| tokens in | tokens out | cost  |
| --------- | ---------- | ----- |
| 18,204    | 1,560      | $0.21 |
```

---

## The merge gate

Merge is GitHub-native. Repository admins should configure branch protection on the default branch to require the `bastion` check and to require review of the reviewer policy (next section).

An author, human or agent, enables GitHub auto-merge on the PR. Once `bastion` goes green and any required policy review is satisfied, GitHub merges; nothing in Bastion presses the button. This is deliberate: the merge mechanics, the queue, the "all required checks pass" logic are GitHub's, and Bastion just supplies one of the required checks.

A push to the PR re-triggers the workflow; the `bastion` check returns to `pending` and resolves again. An agent looping toward green sees the same check transition locally through the CLI and in CI. Cancellation of the old job is also managed by GitHub if configured.

---

## Governance

The core design puts humans at the policy layer; on GitHub that is enforced with two native mechanisms (see the core design's _Threat model & trust boundary_).

**CODEOWNERS.** The Bastion CLI supports generating a CODEOWNERS block covering the reviewer config: the `bastion` review job in GitHub, reviewer definitions, the registry, and the CODEOWNERS file itself. Any PR that adds, removes, or edits a reviewer; loosens a trigger; or changes a prompt touches an owned path, so GitHub requires a human review before merge. Repository maintainers can also obviously provide their own CODEOWNERS instead of using the generated suggestion. The main reason we can't have Bastion automatically manage this is because CODEOWNERS changes only take effect after a PR is merged; as such the CODEOWNERS needs to be written in such a way that it statically protects all paths Bastion will write into in the future.

**Branch protection requires the check.** Requiring `bastion` means a PR can't merge with the gate switched off, and because the workflow file and the Bastion config are themselves owned paths, switching it off is itself a policy change that a human sees.

That is the whole enforcement story, and it is intentionally modest. The contributor we are designing for is an aligned agent that would never quietly disable CI; the CODEOWNERS trip wire and the required check exist so that if policy does change a human is in the loop, not so that a determined adversary is stopped. Anything stronger, like signing, external rule storage, or an enumerated trusted-computing-base, is out of scope for the same reason it is in the core design.

---

## Authentication & billing

Backends bill per individual, and coding agent subscriptions tie usage to one person rather than a team. The core design leaves the choice to the user; on GitHub it lands like this.

The PR author is the requester. Bastion runs the reviewers for a PR under credentials mapped to its author, so reviewing Alice's PR is billed to Alice's subscription; that is the ToS-compliant reading, where each contributor's plan powers the review of their own changes. The adapter resolves the author's GitHub login to a secret name and reads that secret from GitHub Actions secrets at run time.

Bastion does not store any credentials; the team stores them as Actions secrets and tells Bastion the mapping. If no subscription is configured for an author, Bastion can fall back to a shared metered API key, so a new contributor is never blocked from review; whether to allow that fallback is the team's call.

This author-mapped flow works by placing the credential in the runner environment that the backend CLI reads (the subscription `auth.json` flow below writes `~/.codex/auth.json` on the runner). A native reviewer reads that host config directly. A containerized reviewer (one with a `runner`) does not see the runner's home directory: it receives only the reviewer's literal `env` plus a fixed set of provider-credential variable *names* forwarded into the container, so it authenticates from an env-based provider credential (for example `CODEX_API_KEY` / `ANTHROPIC_API_KEY`) or from auth baked into the image, not from a host `auth.json`. See [Containers](./containers.md).

One operational note carried over from the core design: under heavy volume a subscription's rate limits can throttle reviewers, and because gates fail closed a throttled reviewer reads as a blocked merge. Bastion can optionally support API key fallback for this sort of situation as well, or teams may decide to simply use API billing and keep subscriptions for the local loop. That is a tradeoff to make per org and repo.

### Self-hosted example: Bastion reviewing Bastion

This repository dogfoods the adapter through [`.github/workflows/bastion.yml`](../../.github/workflows/bastion.yml). The job runs a published `bastion` release rather than a binary built from the PR's own sources, so a change can never edit the engine that judges it. It downloads the *latest* published release (resolved with `gh release view`, which excludes prereleases), so engine improvements land without a per-PR pin bump while the engine remains a maintainer-published release rather than the PR's sources. Reviewer policy in [`bastion/reviewers.yaml`](../../bastion/reviewers.yaml) is still read from the checkout, and both that file and the workflow are CODEOWNERS-protected paths. Every reviewer in `reviewers.yaml` pins `backend: codex`, so each review runs on the Codex CLI billed to the PR author's own subscription. The workflow wires that up by mapping the author's GitHub login to a per-author credential:

1. **Capture the credential once, locally.** Each contributor runs `codex login` on a machine signed in to their billed ChatGPT/Codex subscription. Codex writes an OAuth credential (an access token plus a refresh token) to `~/.codex/auth.json`.
2. **Store it as a per-author secret.** Copy the contents of that `auth.json` into a repository secret named `CODEX_AUTH_<LOGIN>`: the login uppercased, e.g. `CODEX_AUTH_JSSBLCK` for `jssblck`. Bastion never stores credentials; the secret lives in GitHub Actions.
3. **Map the login to the secret.** The `Authenticate Codex as the PR author` step resolves `github.event.pull_request.user.login` to the matching secret through a `case` arm, so reviewing `jssblck`'s PR bills `jssblck`'s subscription. Onboarding a contributor is two reviewed lines: their secret and a `case` arm. Because the mapping lives in the workflow, which is a CODEOWNERS-protected path, changing who may spend a subscription is itself a human-reviewed change.
4. **The job rehydrates it at run time.** Before running `bastion review`, the step writes the resolved credential back to `$HOME/.codex/auth.json`; Codex refreshes the short-lived access token from the stored refresh token on each run, so the secret does not need rotating every time the access token expires.

An author with no mapped secret fails closed: the step errors and the gate blocks, rather than silently billing someone else's subscription. Two further boundaries keep this safe: GitHub does not expose secrets to workflows triggered by fork pull requests, and the job additionally guards on `head.repo.full_name == github.repository`, so an agentic backend never runs over untrusted code with a live credential; a fork contribution is reviewed by a maintainer re-running it from a trusted branch. The job's pass/fail is the gate (a blocked review exits non-zero); a following `bastion github report` step then posts the sticky comment and the per-reviewer and aggregate check runs, and the full run is also uploaded as an artifact. That report step runs with `if: always()` so the PR is updated even when the review blocked and failed the job; it needs `pull-requests: write` and `checks: write`, and it relies on the default `GITHUB_TOKEN` being a GitHub App installation token (a classic personal access token cannot create check runs).

Bot authors are the one wrinkle. A bot like `dependabot[bot]` opens same-repo PRs (so they clear the `head.repo` guard and want reviewing) but has no subscription of its own, so its `case` arm points at a maintainer's credential, billing the bot's PRs to whoever opted in to sponsor them. Two GitHub-specific gotchas come with that. First, the bot login is literally `dependabot[bot]`; the `[bot]` brackets are a glob character class in a shell `case` pattern, so the arm must quote them (`'dependabot[bot]')`) to match literally. Second, GitHub serves secrets to Dependabot-triggered runs from a *separate Dependabot secret store*, not the Actions store, so the same `CODEX_AUTH_<LOGIN>` must be set in both places (`gh secret set CODEX_AUTH_JSSBLCK --app dependabot`), or the secret arrives empty on Dependabot PRs and the gate blocks with a misleading "mapped but empty" error.

For API-key billing instead of a subscription, store an `OPENAI_API_KEY` secret and export it into the review step rather than writing `auth.json`; the same mapping shape applies.

---

## Environments & inputs

Bastion consumes environments, it does not provision them. A reviewer that needs a preview URL, a database, or any other running dependency expects the workflow to have stood it up and exposed it; the reviewer just reads it.

On GitHub that means the workflow owns whatever a reviewer's `env` and `inputs` reference. A typical setup deploys a preview environment for the PR in an earlier job, or a separate workflow, and passes its URL into the Bastion job as an environment variable. How it reaches the agent depends on where the reviewer runs. A native reviewer inherits the job environment, so a variable exported into the Bastion job is visible to the agent, and `env`/`inputs` add literal values on top. A containerized reviewer (one with a `runner`) inherits none of the arbitrary job environment; only its literal `env` pairs and the fixed provider-credential set cross into the container, so a per-PR value must be written into the reviewer's `env`, usually by templating the registry before the job runs. A secret a reviewer needs comes from Actions secrets the same way author credentials do.

Standing up a preview environment is a deploy concern, and the deploy system already knows how. Bastion's job starts once the environment exists.

---

## Example workflow

A minimal workflow wiring Bastion into PR review:

```yaml
name: bastion
on:
  pull_request:
    types: [opened, synchronize, reopened]

# The aggregate `bastion` check this job reports is what branch protection requires.
jobs:
  review:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0          # full history; reviewers compare base vs head

      # Stand up whatever your reviewers consume; Bastion does not do this.
      - id: preview
        run: ./scripts/deploy-preview.sh   # exports the preview URL

      - uses: bastion/review-action@v1
        with:
          author: ${{ github.event.pull_request.user.login }}
        env:
          PREVIEW_URL: ${{ steps.preview.outputs.url }}
          # Author-mapped credentials live in Actions secrets; the action
          # resolves the author login to the right secret at run time.
```

Branch protection on the default branch requires the `bastion` check and review of the owned reviewer-config paths; everything else is standard GitHub.

---

## Known limitations & future

GitHub-specific deferrals, separate from the core design's list.

- Merge queue integration. The current design relies on GitHub auto-merge plus a required check. Merge queue support is undefined.
- A GitHub App instead of a workflow action, for repos that want Bastion to report checks under its own identity and skip per-repo workflow wiring.

---
title: Continuous integration
summary: "Promoting your reviewers into GitHub Actions: one required check, live progress, and per-author billing."
order: 6
---

# Continuous integration

> Promoting your reviewers into GitHub Actions: one required check, live progress,
> and per-author billing.

The local loop gets you to green before you open a PR. CI is the authoritative
confirmation: it runs the *same* reviewers from the *same* `bastion/reviewers.yaml`
and reports one merge gate. Because routing and aggregation are shared, CI rarely
surprises an author who looped locally. This chapter covers the GitHub adapter --
the one forge Bastion targets.

> Bastion does not own CI; it plugs into yours. The workflow, the secrets, the
> preview environments, and the branch-protection rules are GitHub's. Bastion
> reads and writes them through a thin adapter and otherwise stays out of the way.

> **What ships today vs. the target.** Most of the adapter below now ships. The
> self-hosted workflow runs `bastion review`, gates on its exit code, and then runs
> `bastion github report` to post the results back: a sticky PR comment with every
> reviewer's verdict and findings (optional ones included), one check run per
> reviewer, and the always-present aggregate `bastion` check. The full run is also
> uploaded as an artifact. Still on the target list: findings as *inline* diff
> comments (today they ride the sticky comment and check annotations), the live
> aggregate table and per-reviewer spinners (which need the engine to talk to the API
> mid-run), and the packaged `bastion/review-action@v1`. Jump to
> [What ships today](#what-ships-today) for a workflow you can use now; the rest of
> the chapter describes the target shape.

## How a run maps to GitHub

When fully wired, on each pull-request event (`opened`, `synchronize`, `reopened`)
the adapter computes the changed files, routes to the matching reviewers, runs them
in parallel with per-reviewer timeouts, and reports back. A verdict maps onto two
GitHub surfaces -- the same two a human reviewer uses:

- **Findings become inline PR review comments.** Each finding is posted on its
  `path` and line range. `blocking` and `optional` render differently so a reader
  can tell at a glance which comments hold up the merge. These comments are the
  surface an implementing agent reads -- everything it needs to act is there.
- **Each verdict becomes a check run** named after the reviewer
  (`bastion / tenant-isolation`). A blocking gate reports `failure`; a passing gate
  reports `success`; an advisor always reports `success` with its findings
  attached.

The local-to-GitHub mapping is one-to-one -- the JSONL events you read locally are
the same decisions GitHub renders as checks and comments. The full parity table is
in the [local surface reference](../developer-guide/local-surface.md#parity-with-github).

## The one required check

Branch protection needs you to name the checks that must pass, but Bastion's set of
reviewers *varies per PR* -- a docs-only PR and a server PR trigger different
reviewers, so there is no fixed list of names to require.

The fix is a single always-present check, **`bastion`**, and it is the only one
branch protection requires. It runs even when zero reviewers match (a trivial pass)
so it is always there to require. Internally it reflects the aggregate: `success`
only when every triggered gate passed, `failure` if any gate blocked, errored, or
timed out (fail-closed). The per-reviewer checks stay informational; `bastion` is
the gate.

## Live progress

Reviewers can take seconds or many minutes, so a PR must never look hung. The
adapter leans on GitHub's native check-run status:

- **Per-reviewer spinners.** Each reviewer's check is created `in_progress` the
  moment it is dispatched, so a 15-minute end-to-end reviewer shows a live spinner
  rather than reading as a stall, then flips to its conclusion when it resolves.
- **A live aggregate table.** The `bastion` check stays `in_progress` until every
  reviewer resolves, and its output is rewritten as each one finishes -- a table of
  every triggered reviewer with its mode, status, and elapsed time. One place to
  see what is running, what passed, and what blocked.
- **A permanent run summary.** A rendered report is written to the run summary page
  at the end of the job.

Each reviewer's "Details" page carries its metadata, its verdict, the collapsed
session transcript, and a tokens/cost table when the backend reports usage. That
page is for humans and the occasional surprising decision -- not part of the
implementing agent's normal loop, which lives entirely in the comments.

## What ships today

The working approach is a self-hosted workflow that installs a published `bastion`
release plus your backend CLI, authenticates the backend, runs `bastion review`, and
then runs `bastion github report` to post the results to the PR. The CLI exits
non-zero if any gate blocks, so the job's pass/fail *is* your merge gate; the report
step adds the sticky comment and the per-reviewer and aggregate check runs:

```yaml
name: bastion
on:
  pull_request:
    types: [opened, synchronize, reopened]

# The report step writes the PR comment and the check runs, so the job needs more
# than read access.
permissions:
  contents: read
  pull-requests: write
  checks: write

jobs:
  review:
    runs-on: ubuntu-latest
    # Agentic backends run over the PR's code with live credentials, so restrict to
    # same-repo PRs; a maintainer re-runs a fork PR from a trusted branch.
    if: github.event.pull_request.head.repo.full_name == github.repository
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0          # full history; reviewers diff against the base

      # 1. Install a published bastion release (not built from the PR).
      # 2. Install and authenticate your backend CLI (e.g. claude or codex),
      #    ideally billed to the PR author -- see the auth pattern referenced below.
      # 3. Stand up anything your reviewers consume (a preview env, a database).

      - name: Review
        env:
          BASTION_DATA_DIR: ${{ github.workspace }}/.bastion
        # Non-zero exit on a blocked gate fails the job; that is the merge gate.
        run: bastion review --base "origin/${{ github.base_ref }}"

      - name: Report to the PR
        # Runs even when the review blocked and failed the job, so the comment and
        # checks always land. The default GITHUB_TOKEN is a GitHub App token, so it
        # can create check runs (a classic PAT cannot).
        if: always()
        env:
          GITHUB_TOKEN: ${{ github.token }}
          BASTION_DATA_DIR: ${{ github.workspace }}/.bastion
        run: |
          bastion github report \
            --repo "${{ github.repository }}" \
            --pr "${{ github.event.pull_request.number }}" \
            --sha "${{ github.event.pull_request.head.sha }}"
```

For a complete, working example -- latest-release install, per-author backend
credentials, and fork-PR safety -- see this repository's own
[`.github/workflows/bastion.yml`](../../.github/workflows/bastion.yml) and the
[GitHub adapter reference](../developer-guide/github-adapter.md).

Configure branch protection on your default branch to require this job (and to
require review of the reviewer-policy paths -- see [Governance](./governance.md)).
Merging stays GitHub-native: an author enables auto-merge, and once the required
job is green GitHub merges. A push re-triggers the workflow and it resolves again.

## The target workflow (forward-looking)

The design target is a packaged action that reports per-reviewer checks and inline
comments and exposes the single aggregate `bastion` check
([The one required check](#the-one-required-check), above), so the workflow
collapses to:

```yaml
# Forward-looking: bastion/review-action@v1 is not yet published.
      - uses: bastion/review-action@v1
        with:
          author: ${{ github.event.pull_request.user.login }}
        env:
          PREVIEW_URL: ${{ steps.preview.outputs.url }}
```

When it lands, branch protection would require the aggregate `bastion` check rather
than the job itself.

## Authentication & billing

Coding-agent subscriptions tie usage to an individual, not a team, so Bastion bills
a PR's reviews to the *PR author*. The adapter resolves the author's GitHub login
to a secret name and reads that secret at run time. Bastion never stores
credentials; the team stores them as Actions secrets and tells Bastion the mapping.

This is the ToS-compliant reading: reviewing Alice's PR is billed to Alice's
subscription. If no subscription is mapped for an author, the team can choose to
fall back to a shared metered API key (so a new contributor is never blocked) or to
fail closed. Under heavy volume, a throttled subscription reads as a blocked merge
(gates fail closed), so some teams use API billing in CI and keep subscriptions for
the local loop.

The full mechanics -- per-author secret naming, the rehydration step, fork-PR
safety -- are in the
[GitHub adapter reference](../developer-guide/github-adapter.md#authentication--billing),
including the worked example of Bastion reviewing its own PRs.

## Environments & inputs

Bastion consumes environments; it does not provision them. A reviewer that needs a
preview URL, a database, or any running dependency expects the workflow to have
stood it up and exposed it. Typically an earlier job deploys a preview environment
for the PR and passes its URL into the Bastion job as an environment variable; the
reviewer process inherits the job environment, so the agent can see it. A reviewer's
`env` and `inputs` values are literal (Bastion does not shell-expand them), so to
put a dynamic value into the prompt itself you template the registry or have the
prompt read the inherited variable. Standing up the environment is a deploy concern;
Bastion's job starts once it exists. (See
[Authoring reviewers](./authoring-reviewers.md#env) for the reviewer side.)

## Self-hosting note

This repository dogfoods the adapter through
[`.github/workflows/bastion.yml`](../../.github/workflows/bastion.yml), running the
latest published `bastion` release rather than a binary built from the PR's own
sources -- so a change can never edit the engine that judges it. That workflow is
the concrete, self-hosted adapter described in the
[GitHub adapter reference](../developer-guide/github-adapter.md).

---

Next: [Governance](./governance.md) -- keeping humans at the policy layer with
CODEOWNERS and branch protection, and the escape-to-improvement loop.

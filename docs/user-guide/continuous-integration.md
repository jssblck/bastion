---
title: Continuous integration
summary: "Promoting your reviewers into GitHub Actions: one required check and per-author billing."
order: 6
---

# Continuous integration

> Promoting your reviewers into GitHub Actions: one required check and per-author
> billing.

The local loop gets you to green before you open a PR. CI is the authoritative
confirmation: it runs the *same* reviewers from the *same* `.bastion.yaml`
and reports one merge gate. Because routing and aggregation are shared, CI rarely
surprises an author who looped locally. This chapter covers the GitHub adapter,
the one forge Bastion targets.

> Bastion does not own CI; it plugs into yours. The workflow, the secrets, the
> preview environments, and the branch-protection rules are GitHub's. Bastion
> reads and writes them through a thin adapter and otherwise stays out of the way.

## How a run maps to GitHub

On each pull-request event (`opened`, `synchronize`, `reopened`) the workflow runs
`bastion review`, which computes the changed files, routes to the matching
reviewers, runs them in parallel with per-reviewer timeouts, and persists the run. A
second step, `bastion github report`, reads that run and posts it. A verdict reaches
two GitHub surfaces:

- **Findings are posted to the PR.** `bastion github report` renders every finding
  (blocking and optional) into a single sticky PR comment, and attaches each located
  finding to its reviewer's check run as an annotation on the finding's `path` and
  line range. The sticky comment is the surface an implementing agent reads; it
  carries everything it needs to act.
- **Each verdict becomes a check run** named after the reviewer
  (`bastion / tenant-isolation`). A blocking gate reports `failure`; a passing gate
  reports `success`; an advisor reports `success` with its findings attached.

The local-to-GitHub mapping is one-to-one: the JSONL events you read locally are
the same decisions GitHub renders as checks and a comment. The full parity table is
in the [local surface reference](../developer-guide/local-surface.md#parity-with-github).

## The one required check

Branch protection needs you to name the checks that must pass, but Bastion's set of
reviewers *varies per PR*: a docs-only PR and a server PR trigger different
reviewers, so there is no fixed list of names to require.

The fix is a single always-present check, **`bastion`**, and it is the only one
branch protection requires. It runs even when zero reviewers match (a trivial pass)
so it is always there to require. Internally it reflects the aggregate: `success`
only when every triggered gate passed, `failure` if any gate blocked, errored, or
timed out (fail-closed). The per-reviewer checks stay informational; `bastion` is
the gate.

## The workflow

The adapter is a self-hosted workflow that installs a published `bastion`
release plus your backend CLI, authenticates the backend, runs `bastion review`, and
then runs `bastion github report` to post the results to the PR. The CLI exits
non-zero if any gate blocks, so the job's pass/fail *is* your merge gate; the report
step adds the sticky comment and the per-reviewer and aggregate check runs. That host
backend CLI and its auth cover **native** reviewers (the default). A reviewer with a
[`runner`](./authoring-reviewers.md#runner-and-capabilities) runs its backend
*inside a container* instead (and must declare `capabilities.network: true`; without it
the reviewer is rejected before it runs, so a gate blocks and an advisor is skipped), so
for those the job needs a container engine on the runner (`docker` by default, or
whatever `BASTION_CONTAINER_ENGINE` names) and the backend executable plus its auth
inside the image, not on the host. The fixed provider
credential variables are forwarded from the job into the container by name, so the host
auth still reaches a containerized reviewer's provider even though the CLI itself lives
in the image:

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
      # 2. For native reviewers: install and authenticate your backend CLI (e.g.
      #    claude or codex) on the runner, ideally billed to the PR author; see the
      #    auth pattern referenced below. For reviewers with a `runner`: ensure a
      #    container engine is on the runner (docker by default, or set
      #    BASTION_CONTAINER_ENGINE) and that the backend CLI and its auth live inside
      #    the image; the provider credential variables are forwarded in by name.
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

### `bastion github report`

The report step reads the run that `bastion review` just persisted (under
`BASTION_DATA_DIR`) and posts it to the pull request. Its full surface:

```
bastion github report --repo <OWNER/NAME> --pr <N> --sha <SHA> [RUN]
```

- `--repo <OWNER/NAME>`: the repository to post to. Defaults to the
  `GITHUB_REPOSITORY` environment variable that Actions sets, so you can usually
  omit it.
- `--pr <N>`: the pull request number (required).
- `--sha <SHA>`: the head commit the check runs attach to (required); pass the
  PR's `head.sha`, not the merge commit.
- `RUN`: an optional positional run id to report; defaults to the latest recorded
  run, which is what you want right after `bastion review`.

It needs a token with `pull-requests: write` and `checks: write` in `GITHUB_TOKEN`,
and reads `GITHUB_API_URL` (Actions sets it; also the hook for GitHub Enterprise).
Creating check runs requires a GitHub App installation token, which the Actions
`GITHUB_TOKEN` is; a classic personal access token cannot. If the run cannot be
found (an earlier failure persisted nothing), it prints a notice and exits 0 rather
than failing the step a second time. The command is CI-facing and has no local
mirror: locally you read findings straight from `bastion review --format jsonl`.

For a complete, working example (latest-release install, per-author backend
credentials, and fork-PR safety), see this repository's own
[`.github/workflows/bastion.yml`](../../.github/workflows/bastion.yml) and the
[GitHub adapter reference](../developer-guide/github-adapter.md).

Configure branch protection on your default branch to require this job (and to
require review of the reviewer-policy paths; see [Governance](./governance.md)).
Merging stays GitHub-native: an author enables auto-merge, and once the required
job is green GitHub merges. A push re-triggers the workflow and it resolves again.

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

The full mechanics (per-author secret naming, the rehydration step, fork-PR
safety) are in the
[GitHub adapter reference](../developer-guide/github-adapter.md#authentication--billing),
including the worked example of Bastion reviewing its own PRs.

## Environments & inputs

Bastion consumes environments; it does not provision them. A reviewer that needs a
preview URL, a database, or any running dependency expects the workflow to have
stood it up and exposed it. Typically an earlier job deploys a preview environment
for the PR and passes its URL into the Bastion job as an environment variable. How
that variable reaches the agent depends on where the reviewer runs. A **native**
reviewer inherits the job environment, so the agent can see it directly. A
**containerized** reviewer (one with a
[`runner`](./authoring-reviewers.md#runner-and-capabilities)) runs in a container and
does *not* inherit the arbitrary job environment. Only the reviewer's literal `env`
pairs cross that boundary (plus a fixed provider-credential set, except that a
credential name set in the reviewer's own `env` wins and is not also forwarded from the
job environment), so a per-PR value reaches a containerized reviewer only if you write
its value into the registry,
typically by templating `.bastion.yaml` before the Bastion job runs. A reviewer's
`env` and `inputs` values are literal (Bastion does not shell-expand them), so to put
a dynamic value into the prompt itself you template the registry or have the prompt
read the variable. Standing up the environment is a deploy concern; Bastion's job
starts once it exists. (See
[Authoring reviewers](./authoring-reviewers.md#env) for the reviewer side.)

## Self-hosting note

This repository dogfoods the adapter through
[`.github/workflows/bastion.yml`](../../.github/workflows/bastion.yml), running the
latest published `bastion` release rather than a binary built from the PR's own
sources, so a change can never edit the engine that judges it. That workflow is
the concrete, self-hosted adapter described in the
[GitHub adapter reference](../developer-guide/github-adapter.md).

---

Next: [Governance](./governance.md). Keeping humans at the policy layer with
CODEOWNERS and branch protection, and the escape-to-improvement loop.

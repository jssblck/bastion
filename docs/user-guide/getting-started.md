---
title: Getting started
summary: "Install Bastion, write one reviewer, and run your first review."
order: 2
---

# Getting started

> Install Bastion, write one reviewer, and run your first review.

This chapter gets you from nothing to a working review loop. It assumes you have a
git repository and one of the supported agent backends installed (the Claude Code
or Codex CLI).

A little vocabulary shows up here in passing -- *reviewer*, *gate*, *advisor*,
*verdict*, *findings*. The inline comments are enough to follow along; the next
chapter, [Concepts](./concepts.md), defines each precisely.

## 1. Install the CLI

The quickest path is the install script. It detects your platform, downloads the
matching archive from the latest
[GitHub release](https://github.com/jssblck/bastion/releases), verifies its
SHA-256 checksum, and puts `bastion` on your `PATH`.

On Linux and macOS:

```sh
curl -sSfL https://raw.githubusercontent.com/jssblck/bastion/main/scripts/install.sh | bash
bastion --version
```

On Windows, from PowerShell:

```powershell
irm https://raw.githubusercontent.com/jssblck/bastion/main/scripts/install.ps1 | iex
bastion --version
```

The shell installer takes `-v/--version`, `-b/--bin-dir`, and `-t/--tmp-dir` (pass
them after `bash -s --`); the PowerShell installer reads the `Version` and `BinDir`
environment variables. Pass `--help` (or set `$env:Help="true"`) to see them all.

Prefer to grab the archive yourself? Prebuilt binaries are attached to every
release for Linux (x86_64 and aarch64, glibc and musl), macOS (Intel and Apple
silicon), and Windows (x86_64). Download the one for your platform, extract it, and
put `bastion` on your `PATH`:

```sh
# Example: Linux x86_64
curl -sSL https://github.com/jssblck/bastion/releases/latest/download/bastion-x86_64-unknown-linux-gnu.tar.gz | tar -xz
sudo install bastion-x86_64-unknown-linux-gnu/bastion /usr/local/bin/
bastion --version
```

Prefer to build from source? You need a Rust 2024 toolchain:

```sh
cargo build --release
./target/release/bastion --version
```

`bastion --version` reports a release tag when one is reachable, otherwise the
short commit SHA, with a `-dirty` suffix when the tree has uncommitted changes.

## 2. Make sure the backend is ready

Bastion does not run its own agent loop. It shells out to an existing coding-agent
CLI and reuses whatever you already have configured locally -- so your billing and
auth come along for free. Install and sign in to one of:

- **[Claude Code](https://docs.claude.com/en/docs/claude-code)** (`claude`) -- the
  default when a reviewer does not pin a backend.
- **[Codex](https://github.com/openai/codex)** (`codex`) -- pin it with
  `backend: codex` on a reviewer.

(The `pi` backend is named in the schema but not wired in this build; selecting it
fails closed.)

Bastion invokes the backend as a plain executable on your `PATH` (`claude` or
`codex`), so confirm the one you intend to use is installed and authenticated
before running a review:

```sh
claude --version    # for the Claude Code backend
codex --version     # for the Codex backend
```

If the binary lives elsewhere or you want to point at a wrapper, set
`BASTION_CLAUDE_BIN` or `BASTION_CODEX_BIN` to its path.

## 3. Write your first reviewer

Reviewers live in a single declarative file: `bastion/reviewers.yaml`, in your
repository root. Bastion discovers it by walking up from your current directory,
so you can run `bastion` from anywhere inside the repo. Create the file:

```yaml
# bastion/reviewers.yaml
reviewers:
  - name: single-responsibility
    trigger: [src/**/*.rs]   # which changed files wake this reviewer
    mode: gate               # gate = blocks the merge; advisor = comments only
    prompt: |
      Review the changeset to determine whether any one file concentrates too
      many unrelated responsibilities. If a file has clearly taken on multiple
      distinct concerns that should be separate modules, block the PR and name
      the file(s) and the concerns; otherwise approve it. A single large but
      cohesive module is not a violation.
```

That is a complete reviewer. Four fields carry the meaning: a unique `name`, the
`trigger` globs over your changed files, the `mode`, and the `prompt`. Everything
else has a sensible default. The next chapter, [Concepts](./concepts.md), explains
each of these; [Authoring reviewers](./authoring-reviewers.md) covers the full
schema.

> Adapt the trigger to your language: `src/**/*.ts`, `app/**/*.py`, and so on. The
> glob matches against the paths git reports as changed.

## 4. Run a review

Make a change in your working tree (you do not need to commit it -- Bastion reviews
the working tree, including uncommitted and untracked files), then:

```sh
bastion review --base main
```

Bastion computes the files that differ from `main`, selects the reviewers whose
triggers match, runs them in parallel, and renders progress and verdicts. A blocked
review exits non-zero; a clean one exits zero. That exit code is what lets an agent
-- or a shell loop -- know whether to keep working:

```sh
while ! bastion review --base main; do
  # ... fix what blocked, then loop ...
done
```

## 5. Read it as a machine stream

An agent driving the loop wants structured events, not rendered text. Ask for
JSONL -- one JSON object per line, emitted as each thing happens:

```sh
bastion review --base main --format jsonl
```

You will get one typed event per line as the run progresses, ending in a
`run.completed` that carries the aggregate verdict. The
[local workflow](./local-workflow.md) chapter documents every event type and the
exact contract an agent should follow when consuming them.

## 6. Look at what was saved

Every run is persisted. Inspect history without re-running anything:

```sh
bastion runs                      # list recent runs and their verdicts
bastion show                      # re-print the latest run's findings
bastion transcript <reviewer>     # the full agent session for one reviewer
```

These are the on-demand detail; the common loop never needs them, but they are one
command away when a verdict surprises you. (`show` and `transcript` default to the
latest run; pass a run id for an older one -- the full forms are in
[the local workflow](./local-workflow.md).)

## Keeping scratch runs out of your history

While you are experimenting, point Bastion at a throwaway data directory so trial
runs do not pile up in your real run history:

```sh
bastion --data-dir /tmp/bastion-scratch review --base main
```

The same override is available as the `BASTION_DATA_DIR` environment variable.

Note that `bastion review` always runs your reviewers on a real backend -- there is
no built-in mode that fabricates verdicts without an agent, so a review still costs
a model call. To keep cost down while iterating, start with one cheap, fast
reviewer and a tight `timeout`. (The internal subprocess seam that lets the test
suite run reviewers against a fake executable -- via `BASTION_CLAUDE_BIN` /
`BASTION_CODEX_BIN` -- is documented for contributors in
[the developer guide](../developer-guide/backends.md#the-subprocess-seam), not as an
end-user feature.)

## When something goes wrong

The most common first-run snags and what they mean:

- **"no reviewer registry found ..."** -- there is no `bastion/reviewers.yaml` in
  this repo or any ancestor. Create one (step 3).
- **A reviewer registry error (malformed YAML, duplicate name, missing field).**
  The registry is validated before any agent runs, so these fail fast with a clear
  message. Fix the file and re-run; see [Authoring reviewers](./authoring-reviewers.md).
- **The review blocks immediately with "did not produce a verdict".** A gate failed
  closed -- usually the backend binary is missing or unauthenticated. Re-check
  `claude --version` / `codex --version` and that you are signed in (step 2).
- **No reviewers ran (a trivial pass).** Nothing in your changeset matched any
  reviewer's `trigger`. Confirm you actually changed a file the globs cover, and
  that `--base` points at the right branch.
- **Everything looks unchanged.** Bastion diffs against `--base` (default `main`);
  if your base branch has a different name, pass it explicitly.

---

You now have a working reviewer and a review loop. Next:
[Concepts](./concepts.md) -- the vocabulary (triggers, modes, verdicts, the gate)
the rest of the guide builds on.

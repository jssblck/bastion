# Bastion locally

> The local surface: the same review data as GitHub, streamed out of the CLI for an agent, with the noisy parts kept on disk and read on demand.

The core design ([`design.md`](./design.md)) describes `bastion review` in a single section; this doc is the detail of the local surface, the way the GitHub adapter ([`github-adapter.md`](./github-adapter.md)) is the detail of the CI surface. The two are mirror images: the same reviewers, verdicts, and findings, presented through whatever the surface makes natural. On GitHub that is check runs and PR comments; locally it is a stream on stdout and a few files on disk.

The guiding rule carries over: Bastion does not own your environment, it plugs into it. Locally that means the agent's loop drives Bastion, the local shell provides whatever the reviewers consume, and Bastion streams results back in a shape an agent can read without help.

---

## How it runs

`bastion review` runs the reviewers whose `trigger` globs match the working tree's changes against the base branch, exactly as CI would; routing, the runner, and aggregation are the shared core, not local-specific. The one thing that differs from CI is where the inputs come from: there is no preview-deploy job, so anything a reviewer's `env` or `inputs` reference is expected to be in the local environment already. A `precommit` script might boot the service on `http://localhost:3000` and export that as the preview URL. A native reviewer inherits that local environment directly. A containerized reviewer (one with a `runner`) inherits none of it; only its literal `env` pairs and the fixed provider-credential set cross into the container, so a local value reaches it only when written into the reviewer's `env` (see [Containers](./containers.md)).

The intended use is the loop from the core design: an agent runs `bastion review`, reads the stream, fixes what blocks, runs it again, and repeats until it is green, before ever opening a PR.

---

## Streaming output

Two audiences, two formats. By default `bastion review` renders human-readable progress for a person watching. An agent passes `--format jsonl` (or sets it once in config) and gets a machine stream instead.

We stream **JSONL**: one JSON object per line, emitted as each thing happens. It is the natural fit for a live, append-only sequence of events; an agent can read it line by line as it arrives, and every agent already parses JSON without a library. A run is a sequence of typed events:

```jsonl
{"type":"run.started","run":"r-0f3a","branch":"feat/cart","base":"main","changed":12,"reviewers":[{"name":"file-responsibility","mode":"gate"},{"name":"tenant-isolation","mode":"gate"}]}
{"type":"reviewer.started","run":"r-0f3a","reviewer":"tenant-isolation","mode":"gate","backend":"claude-code"}
{"type":"reviewer.resolved","run":"r-0f3a","reviewer":"tenant-isolation","verdict":"block","summary":"A new query path reads rows without scoping by tenant id.","findings":[{"kind":"blocking","path":"src/server/db.ts","line_start":88,"line_end":91,"detail":"scope this query by tenant_id"}],"usage":{"tokens_in":18204,"tokens_out":1560,"cost_usd":0.21},"duration_ms":38120,"has_transcript":true}
{"type":"run.completed","run":"r-0f3a","verdict":"block","gates":{"total":2,"passed":1,"blocked":1},"duration_ms":41030,"cost_usd":0.37}
```

The event types mirror the GitHub surfaces directly: `run.started` is the set of reviewers that would have appeared as pending checks; `reviewer.started` is the spinner; `reviewer.resolved` is a check run reaching its conclusion, carrying the verdict, findings, and usage; `run.completed` is the aggregate `bastion` check. An agent that wants only the outcome can ignore everything until `run.completed`; one that wants to react as it goes reads each `reviewer.resolved` as it lands.

Note what is _not_ in the stream: the transcript. `reviewer.resolved` carries a `has_transcript` flag rather than the transcript itself; when it is set, the saved transcript is one command away (`bastion transcript <run> <reviewer>`). The reasoning is in the next section.

---

## What we stream, what we save

The principle is the one that put transcripts behind a `<details>` block on GitHub, taken a step further: locally the verbose data is not even sent down the stream. A transcript is mostly noise to an agent that just wants to know what to fix; streaming it on every run would bury the findings under thousands of lines and burn the agent's own context for nothing.

So the split is:

- **Streamed:** the decisions and the things an agent acts on immediately; the reviewer set, the start and resolve events, verdicts, summaries, findings, and per-reviewer usage.
- **Saved, not streamed:** the verbose detail; full session transcripts, raw verdict payloads, and per-reviewer metadata. These go to the data directory and are read on demand.

This keeps the common loop tight: the agent reads a short stream, acts, and re-runs, while nothing is lost; the detail is one command away when a decision is surprising enough to want it.

---

## The data directory

Bastion persists every run under a per-user data directory, resolved by platform convention:

- Linux: `$XDG_DATA_HOME/bastion`, defaulting to `~/.local/share/bastion`.
- macOS: `~/Library/Application Support/bastion`.
- Windows: `%APPDATA%\bastion`.

Each run gets a directory keyed by its run id, holding the full event stream and a subdirectory per reviewer:

```
<data-dir>/
  runs/
    r-0f3a/
      run.jsonl                  # the full event stream, always JSONL regardless of display format
      reviewers/
        tenant-isolation/
          transcript.jsonl       # the full agent session
          verdict.json           # the raw structured verdict
          meta.json              # backend, timing, usage, matched trigger
    latest                       # a plain file holding the most recent run id
```

The run is always persisted as JSONL regardless of the `--format` used on screen, so `run.jsonl` holds the same events whether a human or an agent triggered it; a run can be replayed or inspected after the fact without re-running it, and the per-reviewer files hold what was deliberately kept off the stream. Runs accumulate; `bastion review` does not prune, so history grows until you run `bastion clean` (which keeps the most recent 20 when given no arguments).

---

## On-demand detail

The commands that read saved data back are the local equivalent of clicking "Details" on a check in GitHub. The run-targeted ones (`transcript`, `show`) default to the latest run when a run id is omitted, since that is almost always what an agent wants.

- `bastion transcript [<run>] <reviewer>` prints the saved session transcript for one reviewer. This is the explicit, opt-in way to see the thing we kept off the stream; an agent reaches for it when a verdict is surprising and it wants to know why.
- `bastion show [<run>]` re-emits a past run's summary, verdicts, and findings without re-running it; the same content as the stream's resolve and complete events, on demand.
- `bastion runs` lists recent runs with their id, aggregate verdict, branch, and reviewer count.
- `bastion clean [--keep N | --older-than <dur>]` prunes saved runs.

`show` and `runs` accept `--format human|jsonl`; `transcript` is raw text by default, since a transcript is already a document.

---

## Parity with GitHub

The local and GitHub surfaces carry the same data; only the transport differs. The mapping is one to one:

| GitHub                                       | Local                                   |
| -------------------------------------------- | --------------------------------------- |
| Pending checks for the triggered reviewers   | `run.started` event                     |
| Per-reviewer check-run spinner               | `reviewer.started` event                |
| A check run reaching its conclusion          | `reviewer.resolved` event               |
| Findings as PR review comments               | `findings` in `reviewer.resolved`       |
| The aggregate `bastion` check                | `run.completed` event                   |
| Transcript in a collapsed `<details>`        | saved on disk, `bastion transcript`     |
| Tokens and cost table                        | `usage` in `reviewer.resolved`          |
| Permanent run summary on the run page        | persisted `run.jsonl`, `bastion show`   |

The data mapping is exact, but some left-column *renderings* remain target. `bastion github report` runs after the review, so it posts *completed* checks with no live spinners, renders findings in one sticky PR comment plus check-run annotations (not inline review comments), keeps transcripts in the uploaded run artifact rather than a collapsed `<details>`, and writes the run summary into that artifact. Only how the GitHub side is drawn differs between this release and the target; which event maps to which surface does not.

Anyone who understands one surface understands the other; this is deliberate, so that an agent's local loop and the CI gate never disagree about what a review means.

---

## Known limitations & future

Local-specific deferrals, separate from the core design's list.

- Watch mode. A `bastion review --watch` that re-runs affected reviewers as files change, instead of once per invocation.
- A shared verdict cache so an unchanged reviewer result can be reused across local runs, and even handed to CI, rather than recomputed.
- Transport beyond a process. Driving Bastion over a socket with the same event stream, if an agent harness ever wants to consume it that way rather than from stdout.

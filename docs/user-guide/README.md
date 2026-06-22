---
title: Bastion user guide
summary: "Agentic code review for a world where agents write all of the code."
order: 0
---

# Bastion user guide

> Agentic code review for a world where agents write all of the code.

This guide teaches you how to use Bastion on your own project: what it is, how to
run it, how to write reviewers, and how to wire it into CI and governance. It is
written for two audiences at once (the human curating the review policy and the
agent looping against it), because Bastion shows both the same review through
whatever surface is natural to each.

If you want to change Bastion itself rather than use it, see the
[developer guide](../developer-guide/README.md).

## Read in order

The chapters build on each other. If you read them top to bottom you will go from
"what is this" to "running it in CI with a governed policy" without backtracking.

1. **[Introduction](./introduction.md)**: the problem Bastion solves, the core
   idea (reviewers as fitness functions), and the mental model. Start here.
2. **[Getting started](./getting-started.md)**: install the CLI, write your
   first reviewer, and run your first review in about five minutes.
3. **[Concepts](./concepts.md)**: reviewers, triggers, modes, the verdict, and
   the merge gate. The vocabulary the rest of the guide assumes.
4. **[Authoring reviewers](./authoring-reviewers.md)**: the registry schema in
   full, from the four required fields to timeouts, backends, environment, and
   prompt inputs. How to write a reviewer that stays at high recall.
5. **[The local workflow](./local-workflow.md)**: the `bastion review` loop in
   depth: human output vs. the JSONL agent stream, exit codes, and inspecting
   saved runs (`runs`, `show`, `transcript`, `clean`).
6. **[Continuous integration](./continuous-integration.md)**: promoting the
   same reviewers into GitHub Actions: checks, the aggregate gate, live progress,
   and per-author billing.
7. **[Governance](./governance.md)**: keeping humans at the policy layer with
   CODEOWNERS and branch protection, the escape-to-improvement loop, and what
   Bastion deliberately does not guarantee.

## The one-paragraph version

You declare **reviewers** (focused agent prompts, one concern each) in
`bastion/reviewers.yaml`. Each reviewer has a **trigger** (file globs) and a
**mode** (`gate` blocks the merge, `advisor` only comments). `bastion review`
finds the reviewers whose triggers match your working-tree changes, runs them in
parallel, and aggregates their verdicts into one decision: all gates must pass.
An authoring agent loops `bastion review` until it is green, then opens a PR where
CI runs the very same reviewers and largely just confirms the result. Humans stay
in the loop by owning the reviewer registry, not by reading every diff.

## Status

Bastion is experimental and still partial. The routing, runner, verdict
aggregation, and on-disk run store are implemented and tested, and the Claude Code
and Codex backends execute reviewers for real. Some schema fields (the container
`runner` and the `network`/`mcp`/`skills` capabilities) are accepted but not yet
provisioned, so a reviewer that opts into one fails closed rather than running
without it; those are called out where they appear in
[Authoring reviewers](./authoring-reviewers.md). The deep reference for any of
this is the [core design](../developer-guide/design.md).

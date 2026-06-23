# Product

## Register

brand

## Users

Engineering leaders and senior/staff engineers on teams where agents now write
most of the code, evaluating how to keep quality high without human diff review at
agent volume. They are technical, skeptical of marketing, and have likely already
tried or seen CodeRabbit, Greptile, Cursor BugBot, or Copilot review. They arrive
asking two questions: "why would I care?" and "how does this actually work?" They
read code and config fluently and trust what they can inspect over what they are
told.

## Product Purpose

The site explains Bastion: agentic code review built as single-concern reviewers
(declarative, human-authored fitness functions) that aggregate into one merge gate
that fails closed. Success is a visitor who understands the thesis (govern the
policy, don't trust a bot's judgment) and converts to the install command or the
docs. It is a single long-form landing page, not an app.

## Brand Personality

Load-bearing, exacting, sober. The voice is that of a well-typeset engineering
standard: precise, confident, free of hype. It earns trust by being honest about
what is and isn't built, and by showing real artifacts (the policy file, the
verdict, the gate decision) instead of claims. Dry wit is allowed; exclamation
marks and growth-hack superlatives are not.

## Anti-references

- The category monoculture: dark-mode gradient backgrounds, a glowing 3D
  graph/sphere, a vanity stat bar (6M repos, 75M defects), a 3-step
  Connect/Review/Fix diagram, a friendly mascot. Looking like that signals
  "follower."
- The commodified copy: "high signal, low noise", "code review for the AI era",
  "ship with confidence", "X faster, Y more bugs", "beyond LGTM". All burned.
- The proof artifact everyone else shows (a bot leaving a comment on a PR). Ours is
  the policy a human wrote and the gate's decision.

## Design Principles

- **Show the artifact, not the claim.** The hero is a real `.bastion.yaml` and a
  real verdict, not a mockup of a bot talking. Inspectability substitutes for the
  scale metrics we can't (and shouldn't) fake.
- **Color is the product concept.** The verdict triad (pass / block / advisor) is
  the only saturated color and it is always semantic. The brand accent sits outside
  the triad so the semantics stay legible.
- **Honesty is the brand.** Say plainly what is partial (the `mcp` and `skills`
  capability tiers parse but are not yet provisioned, so a reviewer that opts into
  one fails closed). "We'd rather block than pretend" is more persuasive than any
  benchmark.
- **Govern, don't tolerate.** Every section reinforces one thesis: you author and
  own the policy; the gate fails closed; nothing is a black box.
- **Quiet confidence over volume.** Restraint, structural rules, and exact
  typesetting carry the seriousness. No shouting.

## Accessibility & Inclusion

Target WCAG 2.1 AA. Body text meets >= 4.5:1 contrast; large text >= 3:1. Verdict
state is never carried by color alone (always paired with a text label). All
motion has a `prefers-reduced-motion` path, and content is fully visible without
JavaScript or when reveals don't fire (reveals enhance an already-visible default,
never gate it). Full keyboard navigation with a visible focus ring and a skip link.

---
name: stop-slop
description: Use when writing or reviewing any user-facing prose (marketing copy, docs, README, blog posts, release notes, UI text, commit messages) to strip AI-register "slop". Catches the structural tells (manufactured antithesis, aphorism openers and closers, virtue and character framing, uniform clipped-fragment headers, triadic parallelism, the dramatic colon, the "every sentence does rhetorical work" cadence), not just buzzwords. Also use when prose feels vaguely machine-written and you need to find and fix why.
---

# Stop slop

"Slop" is prose that reads as machine-written. The giveaway is almost never a
single word. A banned-word list catches "delve", "tapestry", and "in today's
landscape"; it will not catch the real problem, which is rhythm and rhetorical
scaffolding. The tells are structural.

Two failure modes, not one:

1. Slop: the copy is doing constant rhetorical work, reassuring the reader and
   resolving every thought into a balanced contrast or a tidy maxim.
2. Over-correction: someone ran a deslopper, so the prose is now conspicuously
   fragment-phobic and antithesis-phobic, every sentence flattened to the same
   informational monotone. That is its own tell.

The target is between them: plain, specific, a little dry, varied. One beat of
personality per section is fine. The point is mechanism-forward writing, not
lobotomized writing.

## The core test

After an edit, ask: is every fact still present? If yes and the copy got
shorter, the cut was slop. If a rewrite would drop information, it is not a
deslop, it is a deletion. Each edit should lose zero information and usually get
shorter.

## The structural tells

Scan for these. `references/structures.md` covers each in depth with kill/keep
tests; this is the working list.

1. Virtue or character framing. The text asserts the subject is honest,
   trustworthy, principled, humble, or that it "knows its edges". Delete it and
   state what the thing does; let the mechanism demonstrate the character.
2. Manufactured antithesis ("X, not Y"). Kill it when the second half is a
   strawman or a vibe. Keep it when both halves name real, distinct, true things.
3. Aphorism openers and closers. A section that opens or closes on a tidy maxim
   ("attention is scarce, and smarter models do not change that"). Cut the maxim;
   the surrounding fact already carries it.
4. Triadic parallelism. Rule-of-three cadence ("read, audit, and build on") used
   for rhythm rather than because there are exactly three real items.
5. The dramatic colon. Setup then payoff staged for suspense ("runs locally, end
   to end: capture to definition").
6. Uniform clipped-fragment headers. Every header a short declarative with a
   terminal period. Vary them; let some be plain noun phrases.
7. The meta-pattern. Nothing is allowed to be boring. Real writing is lumpier:
   it lets some sentences be flat and informational and varies length chaotically.

## Discriminate, do not carpet-bomb

The "X, not Y" structure is the highest-frequency tell, which makes it the
easiest to over-correct. Discriminate:

- KILL: "a gate you govern, not a bot you tolerate" (the second half is a
  strawman invented for cadence).
- KEEP: "gates block, advisors comment" and "gates fail closed, advisors fail
  open" (both halves name real, distinct, opposed behaviors).

Same with fragments and short sentences: they are not banned, only their
uniformity is. Strip the manufactured ones, leave the real ones, and let the
rhythm stay uneven.

## Voice anchor (highest leverage)

"Remove AI tells" is underspecified. "Write like this" is a target. The single
biggest improvement is two or three paragraphs of the actual target voice
pasted in as an anchor before you start. If you have a sample of how this author
or product already writes well, match its sentence length, its comment density,
and its idiom. Absent a sample, default to: an engineer who built the thing
explaining what it does and respecting the reader's time.

## Workflow

1. Get a voice anchor if one exists (prior copy by the same author, a section
   you already like). Read it before editing.
2. Read the prose for sense first. Mark every sentence that is doing rhetorical
   work rather than carrying a fact.
3. For each, identify the tell from the list and apply the core test: rewrite
   shorter, keep every fact, drop the rhetoric.
4. Protect the real opposed pairs and the one-beat-of-personality lines. Do not
   flatten everything.
5. Leave code, commands, data, tables, and quoted or third-party material
   exactly as they are. This is a prose edit.
6. Re-read end to end for monotony. If every sentence now sounds the same, you
   over-corrected: put some variation back.

## Mechanics this project cares about

- Plain ASCII only. No em dashes, no en dashes, and no literal `--` used as a
  dash in prose. Recast with a comma, a colon, parentheses, or two sentences.
  Leave `--flag` forms inside commands and code alone.

## References

- `references/structures.md`: the structural tells in depth, each with a
  kill-or-keep test and a rewrite.
- `references/phrases.md`: the lexical tells (a lower-priority backstop; the
  structure matters more than the vocabulary).
- `references/examples.md`: before and after rewrites drawn from this project.

## Provenance

The structural framing here draws on Hardik Pandya's `stop-slop` skill
(`github.com/hardikpandya/stop-slop`, with a Codex fork at
`github.com/pa4uslf/stop-slop-for-codex`) and on a project-specific deslop brief.
For a deterministic CI backstop, Vale (`vale.sh`) with a `reject.txt` and custom
regex rules catches the lexical tells that an agent can miss; it does not catch
the structural ones, so the two are complementary.

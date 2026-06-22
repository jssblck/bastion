---
name: rust100k-architecture-docs
description: Maintain Rust project architecture documentation in the Matklad Rust100k style. Use when changing module boundaries, runtime layers, cross-cutting concerns, architectural invariants, an architecture doc or AGENTS.md architecture map, or when reviewing whether code structure and the architecture docs still agree.
---

# Rust100k Architecture Docs

Keep architecture docs short, stable, and useful as a map. This skill comes from Matklad's `ARCHITECTURE.md` article in the Rust100k series.

## Workflow

1. Start with the architecture doc (for example `docs/architecture.md` or the architecture map in `AGENTS.md`), then inspect the touched Rust modules.
2. Update architecture docs only for durable structure: problem overview, coarse boundaries, invariants, and cross-cutting concerns.
3. Name important modules, files, traits, and types, but avoid fragile Markdown links to local code paths.
4. Call out important absences and boundaries explicitly, such as "Win32-free hover logic stays in hover/*" or "watch does not use a generic pipeline scheduler".
5. Keep implementation details in code comments, module docs, or narrower docs when they are likely to churn.
6. Run the usual cargo checks and review the architecture doc for durable boundaries.

## Policy

- Preserve the first plain-language overview for readers who are new to the project.
- Keep the top-level sections (the runtime boundaries, the module map, the public contracts, and any cross-cutting concerns) current when the corresponding code moves.
- If code has moved, update the codemap rather than adding a migration note.
- If a rule is enforced by type construction or config validation, state the invariant where the boundary is described.
- If a topic becomes too detailed, split it into a focused doc and leave only a pointer-level summary in the architecture doc.

## Validation

Run the usual cargo checks, then review the architecture doc for durable boundaries:

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```

Required sections and boundary terms are a human review rule because they depend on the shape of the change.

Read `references/article-notes.md` for the source summary and source URL.

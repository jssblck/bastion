---
name: names-are-not-type-safety
description: Apply Alexis King's "Names are not type safety" advice. Use when adding, reviewing, or refactoring Rust newtypes, wrapper structs, ID/name/path/text types, type aliases, domain enums, smart constructors, or APIs that rely on naming conventions for correctness.
---

# Names Are Not Type Safety

Use Rust types to encode functional differences and invariants. Do not add wrappers that only rename an underlying value.

## Workflow

1. Ask what illegal operation or invalid state the type prevents.
2. If the answer is "it documents the role," use a field name, module name, doc comment, or type alias instead of a wrapper.
3. If the answer is an invariant, encode it by construction: enum variants, structured fields, private tuple fields, smart constructors, parser functions, or a data structure that cannot represent the bad case.
4. Keep the trusted module small. Every public constructor, `DerefMut`, `From`, serde derive, or public inner field can become a trapdoor around the invariant.
5. If a transparent wrapper is used for secrecy, display redaction, trait coherence, or distant pass-through clarity, document that it discourages misuse but does not prove safety.
6. Prefer correct-by-construction datatypes over wrappers with comments when Rust can express the shape directly.
7. Run `cargo fmt --check`, `cargo test`, `cargo clippy --all-targets -- -D warnings`, and targeted tests.

## Rules

- Do not add `pub struct FooId(pub String);`-style public transparent domain wrappers. Use a private field plus a parser/smart constructor, or use a type alias if it is only a label.
- Avoid deriving broad conversion or serde traits on checked wrappers unless that does not weaken the invariant.
- Avoid `DerefMut` for checked wrappers unless mutation preserves the invariant or reparses before reuse.
- Do not make taxonomy types just because the real-world concepts have different names. Split types when they behave differently or rule out different states.
- Prefer enum variants over booleans when control flow changes the valid shape of the data.

## Enforcement

A lint or nudge rule can flag public tuple wrappers around primitive/string/path values whose names look like an ID, name, path, text, key, token, or config wrapper. This is intentionally conservative: if the wrapper only labels a value, use a type alias; if it enforces an invariant, hide the field.

Read `references/article-notes.md` for source notes and examples.

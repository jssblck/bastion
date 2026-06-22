---
name: rust100k-testing-discipline
description: Apply Matklad Rust100k testing discipline. Use when adding, reviewing, or reorganizing Rust tests, choosing unit versus integration tests, designing fixture formats, rejecting mocks, adding doctests, or changing the check-suite coverage.
---

# Rust100k Testing Discipline

Design tests around product behavior and data, keep them real, and avoid mocks.
This skill combines Matklad's `Delete Cargo Integration Tests` and `How to Test`
guidance with the conflict decisions in `.agents/skills/readme.md`.

## Workflow

1. Define the feature boundary first. Good boundaries are usually where data enters or leaves: config parsing, input validation, discovery, and the planning or translation a request drives.
2. Prefer a small `check(...)` helper with data inputs and expected data outputs over many tests that call internal APIs directly.
3. Keep core tests sans IO: build values in memory and let the function under test compute.
4. Use externalized fixture files when they make cases easy to add, but keep at least one small smoke test that can be run/debugged directly from the IDE.
5. For internal code, prefer unit tests in `src/` over Cargo integration crates. If a separate integration crate is needed, use one modular crate, not many root `tests/*.rs` binaries.
6. Do not use mocks. Use real pure functions, deterministic fixture directories under `tests/testdata`, `tempfile` filesystem fixtures, and throwaway git repositories. Where the code has a real external dependency, prefer a real instance in tests over a fake.
7. Keep real executable doctests unless the project is large enough that doctest build cost dominates.
8. Run `cargo fmt --check`, `cargo test`, and `cargo clippy --all-targets -- -D warnings`.

## Policy

- Do not use mocks, test doubles, `mockall`, or fake service boundaries for first-party behavior.
- Keep real executable doctests at this project size.
- Prefer inline `#[cfg(test)] mod tests { ... }` blocks while the crate is small. When touching a large test module, prefer migrating that module to `#[cfg(test)] mod tests;` plus a sibling `tests.rs`.
- Deterministic fixture directories are encouraged for boundary-data tests because they exercise real data without mocks.
- Avoid sleep-based synchronization in tests. If concurrency is involved, expose a join, receiver, or observable side channel.

## Validation

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```

No mocks, no `mockall`, no first-party `Mock*` identifiers, doctests on, no
many-root Cargo integration test crates, and no sleep-based tests remain review
rules unless the project adds a dedicated policy checker. Inline test-module
migration remains a review rule for touched files when a repo already has
existing inline tests.

Read `references/article-notes.md` for source notes and conflict context.

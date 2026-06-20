---
name: rust100k-testing-discipline
description: Apply Matklad Rust100k testing discipline to Homeport. Use when adding, reviewing, or reorganizing Rust tests, choosing unit versus integration tests, designing fixture formats, rejecting mocks, adding doctests, or changing Justfile check coverage.
---

# Rust100k Testing Discipline

Design tests around product behavior and data, keep them real, and avoid mocks.
This skill combines Matklad's `Delete Cargo Integration Tests` and `How to Test`
guidance with the Homeport conflict decisions in `.agents/skills/readme.md`.

## Workflow

1. Define the feature boundary first. For Homeport, good boundaries are config parsing, profile validation, skill discovery, adapter capability reporting, and translation planning.
2. Prefer a small `check(...)` helper with data inputs and expected data outputs over many tests that call internal APIs directly.
3. Keep core tests sans IO: build values in memory and let the function under test compute.
4. Use externalized fixture files when they make cases easy to add, but keep at least one small smoke test that can be run/debugged directly from the IDE.
5. For internal Homeport code, prefer unit tests in `src/` over Cargo integration crates. If a separate integration crate is needed, use one modular crate, not many root `tests/*.rs` binaries.
6. Do not use mocks. Use real pure functions, real `sqlx::test` Postgres
   databases, deterministic fixture directories under `tests/testdata`, and
   `tempfile` restore targets.
7. Keep real executable doctests. Homeport is not large enough to disable them for build-time reasons.
8. Run `cargo fmt --check`, `cargo test`, and `cargo clippy --all-targets -- -D warnings`.

## Homeport Policy

- Homeport does not use mocks, test doubles, `mockall`, or fake service
  boundaries for Homeport-owned behavior.
- `rust-skills` wins on executable doctests for this project size.
- Inline `#[cfg(test)] mod tests { ... }` blocks already exist. When touching a large test module, prefer migrating that module to `#[cfg(test)] mod tests;` plus a sibling `tests.rs`.
- Deterministic fixture directories are encouraged for adapter, profile, and
  backup tests because they test boundary data without mocks.
- Client/server tests should use `sqlx::test` against real Postgres, with
  `DATABASE_URL` pointing at a database where SQLx can create isolated test
  databases.
- Avoid sleep-based synchronization in tests. If concurrency is involved, expose a join, receiver, or observable side channel.

## Validation

```zsh
export DATABASE_URL="postgres://homeport:homeport_dev_password@127.0.0.1:54329/homeport"
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```

No mocks, no `mockall`, no first-party `Mock*` identifiers, doctests-on, no
many-root Cargo integration test crates, and no sleep-based tests remain review
rules unless Homeport adds a dedicated policy checker. Inline test-module
migration remains a review rule for touched files because the current repo has
existing inline tests.

Read `references/article-notes.md` for source notes and conflict context.

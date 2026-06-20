# Repo-Local Rust Skills

This directory contains three Rust guidance sources:

- `rust-skills/`: installed from `leonardomso/rust-skills` at commit `89910e8585331dabbecd400ae132b4070ecf24af`.
- `rust100k-*`: skills derived from Matklad's Rust100k article index at https://matklad.github.io/2021/09/05/Rust100k.html.
- `parse-dont-validate/` and `names-are-not-type-safety/`: skills derived from Alexis King's articles at https://lexi-lambda.github.io/blog/2019/11/05/parse-don-t-validate/ and https://lexi-lambda.github.io/blog/2020/11/01/names-are-not-type-safety/.

These were brought into Bastion from a sibling project and adapted; externally
sourced guidance remains subject to its own upstream license terms where
identified (`rust-skills/` is MIT; see `rust-skills/LICENSE`).

## Conflict Decisions

These are the semantic conflicts found while installing the skills and reading the Rust100k series, resolved for Bastion.

| Topic | Matklad | rust-skills | Bastion decision |
|---|---|---|---|
| Cargo integration tests | Internal crates should avoid integration crates; public libraries should use at most one modular integration crate. | Put integration tests under `tests/`, with examples using multiple files. | Prefer Matklad. Bastion is an internal CLI/application, so use `src/` tests by default and at most one modular integration crate if a true external boundary needs it. |
| Test module shape | For larger test bodies, use `#[cfg(test)] mod tests;` and a sibling `tests.rs` so test-only edits avoid normal library recompilation. | Use inline `#[cfg(test)] mod tests { ... }`. | Prefer inline `#[cfg(test)] mod tests` while the crate is small; migrate large touched modules to sibling `tests.rs` if compile time becomes a problem. |
| Doctests | Disable doctests for internal libraries in large projects when link cost dominates. | Keep examples executable as doctests. | Prefer `rust-skills`: this is not yet a large project, and executable docs are valuable. |
| Mocking | Favor boundary/data-driven tests and observability over mocks. | Use trait mocks and `mockall` for isolation. | Bastion does not use a mocking framework. Use real pure functions, `tempfile` filesystem fixtures, and throwaway `git init` repositories. `MockBackend` in `src/runner.rs` is a deliberate deterministic double for the single agent-execution boundary, not a general pattern. |
| Generic and `dyn` boundaries | In large systems, avoid generic code across crate boundaries; use thin wrappers over concrete or `dyn` internals. | Prefer `impl Trait` or generics over type erasure for runtime performance. | Prefer `rust-skills` today: Bastion is small enough that runtime clarity usually wins. Revisit if compile-time evidence changes. |

## Enforcement

The repo-level checks are:

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```

`just check` runs those commands.

Cargo/Clippy config enforces what fits Rust-native tooling:

- `Cargo.toml` keeps explicit Clippy lint groups.
- `clippy::inline_always` is denied.
- `clippy::unnecessary_wraps` is denied to catch functions that claim
  fallibility without needing it.

The remaining skills are review guidance rather than mechanically enforced
rules: parse-don't-validate at boundaries (compile trigger globs and durations
into refined types once), newtypes over stringly-typed data, and fail-closed
error handling for gates.

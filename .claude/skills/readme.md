# Rust Coding Skills

This directory contains Rust guidance adapted from external sources:

- `rust-skills/`: installed from `leonardomso/rust-skills` at commit `89910e8585331dabbecd400ae132b4070ecf24af`.
- `rust100k-*`: skills derived from Matklad's Rust100k article index at https://matklad.github.io/2021/09/05/Rust100k.html.
- `parse-dont-validate/` and `names-are-not-type-safety/`: skills derived from Alexis King's articles at https://lexi-lambda.github.io/blog/2019/11/05/parse-don-t-validate/ and https://lexi-lambda.github.io/blog/2020/11/01/names-are-not-type-safety/.

Externally sourced guidance remains subject to its own upstream license terms
where identified (`rust-skills/` is MIT; see `rust-skills/LICENSE`).

## Conflict Decisions

These reconcile the semantic conflicts between Matklad's Rust100k guidance and
the rust-skills rules, resolved as defaults for a small, single-crate internal
application.

| Topic | Matklad | rust-skills | Default |
|---|---|---|---|
| Cargo integration tests | Internal crates should avoid integration crates; public libraries should use at most one modular integration crate. | Put integration tests under `tests/`, with examples using multiple files. | Prefer Matklad. For an internal CLI or application, use `src/` tests by default and at most one modular integration crate if a true external boundary needs it. |
| Test module shape | For larger test bodies, use `#[cfg(test)] mod tests;` and a sibling `tests.rs` so test-only edits avoid normal library recompilation. | Use inline `#[cfg(test)] mod tests { ... }`. | Prefer inline `#[cfg(test)] mod tests` while the crate is small; migrate large touched modules to a sibling `tests.rs` if compile time becomes a problem. |
| Doctests | Disable doctests for internal libraries in large projects when link cost dominates. | Keep examples executable as doctests. | Prefer rust-skills while the project is not large: executable docs are valuable. |
| Mocking | Favor boundary/data-driven tests and observability over mocks. | Use trait mocks and `mockall` for isolation. | Do not use a mocking framework. Use real pure functions, `tempfile` filesystem fixtures, and throwaway `git init` repositories. A single deterministic double at one true external boundary is acceptable; a general mocking pattern is not. |
| Generic and `dyn` boundaries | In large systems, avoid generic code across crate boundaries; use thin wrappers over concrete or `dyn` internals. | Prefer `impl Trait` or generics over type erasure for runtime performance. | Prefer rust-skills while the project is small: runtime clarity usually wins. Revisit if compile-time evidence changes. |

## Enforcement

The repo-level checks are:

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```

The project's check recipe (`just check`) runs these together.

Cargo/Clippy config enforces what fits Rust-native tooling:

- `Cargo.toml` keeps explicit Clippy lint groups.
- `clippy::inline_always` is denied.
- `clippy::unnecessary_wraps` is denied to catch functions that claim
  fallibility without needing it.

The remaining skills are review guidance rather than mechanically enforced
rules: parse-don't-validate at boundaries (compile weak inputs into refined
types once), newtypes over stringly-typed data, and fail-closed error handling
at trust boundaries.

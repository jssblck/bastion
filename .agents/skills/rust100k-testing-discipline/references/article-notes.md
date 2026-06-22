# Article Notes

Sources:

- https://matklad.github.io/2021/02/27/delete-cargo-integration-tests.html
- https://matklad.github.io/2021/05/31/how-to-test.html

Matklad's durable points:

- Cargo compiles each root `tests/*.rs` file as a separate test binary, so many integration test crates cost compile time and runtime parallelism.
- For internal crates, prefer unit tests in `src/`; for public libraries, use one modular integration crate when an external-public-API test is valuable.
- Prefer `#[cfg(test)] mod tests;` in a sibling test file for larger test bodies so test-only edits do not force normal library recompilation.
- Use data-driven `check(...)` helpers so refactors touch one adapter rather than many test cases.
- Test features and boundaries rather than implementation details.
- Keep IO out of core tests; use explicit data in and data out.
- Use expectation/externalized tests when outputs are large, but keep a direct smoke test for debugging.
- Avoid sleep-based concurrency tests. Preserve causality with join handles, receivers, or observable side channels.
- Use tests as automation for project invariants.

Decisions for a small project:

- Prefer Matklad on test layout. Do not use mocks.
- Prefer real executable doctests from `rust-skills` while the project is not large enough for doctest build cost to dominate.
- Use real pure functions, deterministic fixture directories under
  `tests/testdata`, and `tempfile` filesystem fixtures. Where the code has a
  real external dependency, prefer a real instance in tests over a fake.

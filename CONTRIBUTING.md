# Contributing

Bastion is an application, not a Rust library intended for crates.io.
Contributions should improve the actual review experience, the reviewer schema,
the runner and backends, the CI adapter, or the maintainability of the system.

## Local setup

Install Rust stable, Git, and (optionally) [`just`](https://github.com/casey/just).

```sh
just check        # or: cargo fmt --check && cargo test && cargo clippy --all-targets -- -D warnings
```

There are no external services to stand up: the test suite is hermetic and uses
`tempfile` for filesystem fixtures and throwaway git repositories.

## Working style

- Fix root causes when they are in scope.
- Do not preserve backwards compatibility by default; if the clean solution means
  changing schemas, renaming concepts, or rewriting call sites, do it and mention
  the breakage plainly.
- Keep the reviewer schema, the verdict/event schema, and the docs under `docs/`
  in sync when behavior changes — the local and GitHub surfaces are meant to be
  mirror images and must not drift.
- Update the example `bastion/reviewers.yaml` when the schema changes.
- Run `just check` before opening a PR.
- Use plain ASCII quotes in docs, comments, and generated text.

## AI-assisted contributions

AI-assisted PRs are welcome. The human submitter is responsible for the change:
understand the code, review the generated output, test it, and explain the
intent. Do not submit a raw dump of generated code that you cannot defend or
maintain. Maintainers may ask for simplification, tests, or clearer rationale
before merging.

## Pull requests

PRs should explain why the change exists, what behavior changed, any impact on
the reviewer/verdict schemas or the governance model, and the verification
performed (especially the core Rust checks).

## Releases

Bastion uses GitHub Releases as the changelog. Release builds derive their
reported version from the git tag through `git describe --tags`.

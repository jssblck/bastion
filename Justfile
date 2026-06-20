default:
    @just --list

fmt:
    cargo fmt --check

test:
    cargo test

clippy:
    cargo clippy --all-targets -- -D warnings

check: fmt test clippy

build:
    cargo build --release

# Run the reviewers triggered by the working tree against a base branch.
review base="main":
    cargo run -- review --base {{base}}

version:
    cargo run -- --version

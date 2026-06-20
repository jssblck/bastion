---
name: rust100k-build-performance
description: Apply Matklad Rust100k build-time, inline, workspace, and generic-boundary guidance to Homeport. Use when changing Rust crate structure, public generic APIs, #[inline] attributes, release profiles, compile-time-sensitive code, or build/check scripts.
---

# Rust100k Build Performance

Keep Rust build time and hot-path performance visible together. This skill covers Matklad's `Inline In Rust`, `Large Rust Workspaces`, and `Fast Rust Builds` posts, with Homeport's smaller-project conflict decisions documented in `.agents/skills/readme.md`.

## Workflow

1. Measure before tuning. For runtime changes use targeted command timings or smoke runs; for build changes use cargo timings, CI duration, or `cargo metadata` plus targeted cargo commands.
2. Treat `#[inline]` as cross-crate body exposure, not a magic speed switch. In applications, add it reactively after profiling or for tiny public wrappers.
3. Keep `#[inline(always)]` rare and evidence-backed. Prefer `#[cold]` or `#[inline(never)]` for error/cold paths only when profiling or layout concerns justify it.
4. For ergonomic generic public APIs, keep the generic wrapper thin and delegate immediately to a concrete implementation when compile-time bloat matters.
5. For Homeport's current size, prefer `impl Trait` or generics when `rust-skills` runtime/perf guidance is clearly better. Revisit if Homeport becomes a large multi-crate workspace.
6. If Homeport becomes a workspace, use a flat `crates/*` layout, a virtual root manifest, folder names matching crate names, and a dedicated automation crate rather than scattered scripts.
7. Run `cargo fmt --check`, `cargo test`, and `cargo clippy --all-targets -- -D warnings`.

## Homeport Policy

- Keep `cargo fmt --check`, `cargo test`, and `cargo clippy --all-targets -- -D warnings` as the ordinary gate.
- Keep clippy lint groups in `Cargo.toml` explicit so the policy is visible outside the Git hook and `Justfile`.
- Do not add compile-time-heavy abstractions for hypothetical reuse.
- Prefer concrete data boundaries in shared internals, but allow `impl Trait` where it improves current clarity or runtime performance.
- Treat release profile tuning as a measured change; document before/after latency and build-time impact.

## Validation

```zsh
export DATABASE_URL="postgres://homeport:homeport_dev_password@127.0.0.1:54329/homeport"
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```

Clippy rejects `#[inline(always)]`. Cargo keeps explicit Clippy groups in `[lints.clippy]`. Generic boundary tradeoffs remain a review rule because this project currently prefers `rust-skills` runtime clarity unless compile-time evidence changes.

Read `references/article-notes.md` for source notes and tool suggestions.

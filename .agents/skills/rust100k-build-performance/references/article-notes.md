# Article Notes

Sources:

- https://matklad.github.io/2021/07/09/inline-in-rust.html
- https://matklad.github.io/2021/08/22/large-rust-workspaces.html
- https://matklad.github.io/2021/09/04/fast-rust-builds.html

Matklad's durable points:

- `#[inline]` mainly exposes function bodies for cross-crate inlining. It can improve optimization but increases compile work.
- Private same-crate functions usually do not need proactive `#[inline]`.
- Application code should add inline hints reactively, guided by profiling; library code can inline tiny public non-generic wrappers more proactively.
- Generic functions are effectively body-exposed and can cause monomorphization bloat. Thin ergonomic generic wrappers should delegate to concrete implementations when build time matters.
- Large workspaces should use a flat `crates/*` layout, a virtual root manifest, folder names matching crate names, and Rust-based automation such as xtask.
- Build time is a productivity multiplier and should be watched before it becomes painful.
- CI duration is a useful standardized build-time benchmark.
- Cache dependencies in CI more readily than project crates, which change often.

Homeport decisions:

- Homeport is still a single-crate project, so do not force large-workspace ceremony.
- Prefer `rust-skills` runtime-oriented generic and `impl Trait` advice for current Homeport unless compile-time evidence says otherwise.
- Keep build/perf changes evidence-driven through cargo checks, clippy, targeted command smoke runs, and CI duration when available.

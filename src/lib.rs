//! Bastion: agentic code review.
//!
//! Bastion runs single-concern reviewers as fitness functions over a changeset,
//! both locally (this CLI) and in CI. Each reviewer is a focused agent prompt
//! with a trigger; matched reviewers run, return a structured [`verdict`], and
//! Bastion aggregates them into a single merge gate.
//!
//! This crate is the local surface described in `docs/developer-guide/local-surface.md`. The data and
//! routing layers, the Claude Code [`backend`], and the parallel [`runner`] are
//! real and tested; sibling backends (Codex, Pi) are stubbed behind the stable
//! [`backend::Backend`] trait awaiting implementation.
//!
//! The module layout follows the domain rather than file kind:
//!
//! - [`reviewer`] / [`config`] — the declarative reviewer registry.
//! - [`routing`] — matching changed files to reviewers by trigger glob.
//! - [`verdict`] / [`event`] — the structured outputs reviewers and runs emit.
//! - [`git`] — the few git queries the CLI needs (changed files, branch).
//! - [`paths`] / [`store`] — the on-disk run history under the data directory.
//! - [`render`] — turning events into human or JSONL output.
//! - [`backend`] — the agent execution boundary (Claude Code and siblings).
//! - [`runner`] — the parallel, timeout-bounded runner and aggregation.
//! - [`skills`] — the agent skills bundled into the binary and installed into a
//!   consuming repo so its agents learn how to use Bastion.
//! - [`cli`] / [`commands`] — the argument surface and command handlers.

#![warn(missing_docs)]

pub mod backend;
pub mod cli;
pub mod commands;
pub mod config;
pub mod event;
pub mod git;
pub mod paths;
pub mod render;
pub mod reviewer;
pub mod routing;
pub mod runner;
pub mod skills;
pub mod store;
pub mod verdict;
pub mod version;

/// Install error reporting and tracing, then parse and dispatch the CLI.
///
/// This is the single entrypoint shared by [`main`](../src/main.rs) and by
/// integration tests that want to drive the CLI in-process. The returned
/// [`ExitCode`](std::process::ExitCode) carries the gate result: `bastion review`
/// yields a non-zero code when the aggregate verdict is `block`.
///
/// # Errors
///
/// Returns any error bubbled up from a command handler, already enriched with
/// `color_eyre` context for display.
pub async fn run() -> color_eyre::Result<std::process::ExitCode> {
    install()?;
    cli::run().await
}

/// Configure `color_eyre` panic/error reporting and a `tracing` subscriber.
///
/// Tracing defaults to `info` and is overridable via `RUST_LOG`. Logs go to
/// stderr so they never corrupt the JSONL event stream on stdout.
///
/// # Errors
///
/// Returns an error if `color_eyre` has already installed its hooks.
fn install() -> color_eyre::Result<()> {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::{EnvFilter, Layer, fmt};

    color_eyre::install()?;

    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,bastion=info"));
    tracing_subscriber::registry()
        .with(
            fmt::layer()
                .with_writer(std::io::stderr)
                .with_filter(env_filter),
        )
        .init();

    Ok(())
}

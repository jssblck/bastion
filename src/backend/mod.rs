//! The backend execution boundary.
//!
//! Bastion does not run its own agent loop; it translates a reviewer's execution
//! profile into a backend's native config and shells out to that backend's CLI
//! (`docs/DESIGN.md`). This module defines the contract — [`Backend`], what it is
//! handed ([`ReviewRequest`]) and what it must return ([`ReviewOutcome`]) — plus
//! the concrete backends and the dispatch that picks one for a reviewer.
//!
//! The subprocess boundary lives behind [`command::CommandRunner`] so backends can
//! be driven against a fake executable in tests, with no real agent or network.

pub mod claude_code;
pub mod command;

use std::path::Path;

use color_eyre::eyre::Result;

use crate::event::RunId;
use crate::reviewer::{self, Reviewer};
use crate::verdict::{Decision, Usage, Verdict};

use self::claude_code::ClaudeCodeBackend;
use self::command::SystemCommandRunner;

/// Everything a backend is handed to run one reviewer.
///
/// The runner gives every reviewer a full checkout at the changeset head plus the
/// request metadata; the prompt, not the runner, scopes attention.
#[derive(Debug)]
pub struct ReviewRequest<'a> {
    /// The reviewer to execute.
    pub reviewer: &'a Reviewer,
    /// The run this review belongs to.
    pub run: &'a RunId,
    /// The repository root the backend operates within.
    pub repo_root: &'a Path,
    /// The base branch the changeset is computed against.
    pub base: &'a str,
}

/// What a backend returns for one reviewer.
#[derive(Debug, Clone)]
pub struct ReviewOutcome {
    /// The structured verdict.
    pub verdict: Verdict,
    /// Token and cost accounting, when the backend reports it.
    pub usage: Option<Usage>,
    /// The full session transcript, saved to disk but never streamed.
    pub transcript: Option<String>,
}

/// A backend capable of executing a reviewer and returning a structured verdict.
///
/// Implementors translate the reviewer's execution profile into their native
/// configuration and capture the agent's structured output. The trait is kept
/// deliberately small and stable: sibling backends (Codex, Pi) implement the same
/// signature, and [`dispatch`] selects between them by [`reviewer::Backend`].
#[allow(
    async_fn_in_trait,
    reason = "single-crate trait; backends are constructed and consumed internally, not across a public API boundary"
)]
pub trait Backend {
    /// Which backend this is.
    fn id(&self) -> reviewer::Backend;

    /// Execute one reviewer and return its outcome.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend fails to run or cannot produce a verdict;
    /// the runner translates that into a fail-closed `block` for gates.
    async fn review(&self, request: &ReviewRequest<'_>) -> Result<ReviewOutcome>;
}

/// A deterministic backend that always passes, for tests and local dry-runs of
/// the surrounding machinery without invoking a real agent.
#[derive(Debug, Clone, Copy, Default)]
pub struct MockBackend;

impl Backend for MockBackend {
    fn id(&self) -> reviewer::Backend {
        reviewer::Backend::Any
    }

    async fn review(&self, request: &ReviewRequest<'_>) -> Result<ReviewOutcome> {
        Ok(ReviewOutcome {
            verdict: Verdict {
                decision: Decision::Pass,
                summary: format!("mock backend approved '{}'", request.reviewer.name),
                findings: Vec::new(),
            },
            usage: None,
            transcript: Some("(mock transcript)".to_string()),
        })
    }
}

/// Run one reviewer on the backend its profile selects.
///
/// Dispatch maps [`reviewer::Backend`] to a concrete backend. `Any` defaults to
/// Claude Code for now; the named variants pin a harness. The match is the single
/// place that grows when a sibling backend lands — the [`Backend`] trait does not.
///
/// # Errors
///
/// Returns an error if the selected backend fails to run or cannot produce a
/// verdict. The runner turns that into a fail-closed block for gates.
pub async fn dispatch(request: &ReviewRequest<'_>) -> Result<ReviewOutcome> {
    match request.reviewer.backend {
        // `Any` lets Bastion choose; default to Claude Code until routing by
        // availability/subscription exists.
        reviewer::Backend::Any | reviewer::Backend::ClaudeCode => {
            ClaudeCodeBackend::new(SystemCommandRunner)
                .review(request)
                .await
        }
        // Sibling backends implement the same trait; wire them here as they land.
        reviewer::Backend::Codex => {
            color_eyre::eyre::bail!(
                "the codex backend is not yet wired in this build (reviewer '{}')",
                request.reviewer.name
            )
        }
        reviewer::Backend::Pi => {
            color_eyre::eyre::bail!(
                "the pi backend is not yet wired in this build (reviewer '{}')",
                request.reviewer.name
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reviewer::Mode;
    use std::path::PathBuf;

    fn reviewer(backend: reviewer::Backend) -> Reviewer {
        Reviewer {
            name: "demo".into(),
            trigger: vec!["**".into()],
            mode: Mode::Gate,
            backend,
            timeout: None,
            runner: None,
            env: Default::default(),
            capabilities: Default::default(),
            inputs: Default::default(),
            prompt: "p".into(),
        }
    }

    #[tokio::test]
    async fn mock_backend_passes_deterministically() {
        let reviewer = reviewer(reviewer::Backend::Any);
        let run = RunId("r-test".into());
        let root = PathBuf::from(".");
        let request = ReviewRequest {
            reviewer: &reviewer,
            run: &run,
            repo_root: &root,
            base: "main",
        };

        let outcome = MockBackend.review(&request).await.expect("mock runs");
        assert_eq!(outcome.verdict.decision, Decision::Pass);
        assert!(outcome.verdict.summary.contains("demo"));
        assert_eq!(MockBackend.id(), reviewer::Backend::Any);
    }

    #[tokio::test]
    async fn dispatch_rejects_unwired_backends() {
        let reviewer = reviewer(reviewer::Backend::Codex);
        let run = RunId("r".into());
        let root = PathBuf::from(".");
        let request = ReviewRequest {
            reviewer: &reviewer,
            run: &run,
            repo_root: &root,
            base: "main",
        };
        let err = dispatch(&request).await.unwrap_err();
        assert!(err.to_string().contains("codex backend is not yet wired"));
    }
}

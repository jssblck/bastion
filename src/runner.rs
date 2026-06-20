//! The backend execution boundary.
//!
//! Bastion does not run its own agent loop; it translates a reviewer's execution
//! profile into a backend's native config and shells out to that backend's CLI
//! (`docs/DESIGN.md`). This module defines the contract — what a backend is handed
//! and what it must return — so the runner and the rest of the system can be
//! built and tested against it.
//!
//! **Status: walking skeleton.** The trait and the deterministic [`MockBackend`]
//! exist and are tested; the real Claude Code / Codex / Pi backends and the
//! parallel, timeout-bounded runner ([`execute`]) are not yet implemented.

use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, bail};

use crate::event::RunId;
use crate::reviewer::{self, Reviewer};
use crate::verdict::{Decision, Usage, Verdict};

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
/// configuration and capture the agent's structured output.
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

/// Shared context for executing a run's reviewers.
#[derive(Debug, Clone)]
pub struct ExecContext {
    /// The run id.
    pub run: RunId,
    /// The repository root.
    pub repo_root: PathBuf,
    /// The branch under review.
    pub branch: String,
    /// The base branch.
    pub base: String,
}

/// Execute the matched reviewers for a run, in parallel with per-reviewer
/// timeouts, returning the run's event stream.
///
/// # Errors
///
/// Always returns an error in this build: real backend execution is not yet
/// implemented. Routing (which reviewers *would* run) is fully wired; this is the
/// remaining piece.
pub async fn execute(_matched: &[&Reviewer], _ctx: &ExecContext) -> Result<()> {
    bail!(
        "executing reviewers is not yet implemented in this build (walking skeleton).\n\
         routing works — the reviewers above are the ones that would run — but the \
         claude-code/codex/pi backends and the parallel runner are pending. \
         fail-closed: treating this as a blocked review."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reviewer::Mode;

    fn reviewer() -> Reviewer {
        Reviewer {
            name: "demo".into(),
            trigger: vec!["**".into()],
            mode: Mode::Gate,
            backend: reviewer::Backend::Any,
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
        let reviewer = reviewer();
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
    async fn execute_is_not_yet_implemented_and_fails_closed() {
        let ctx = ExecContext {
            run: RunId("r-x".into()),
            repo_root: PathBuf::from("."),
            branch: "feat".into(),
            base: "main".into(),
        };
        let err = execute(&[], &ctx).await.unwrap_err();
        assert!(err.to_string().contains("not yet implemented"));
    }
}

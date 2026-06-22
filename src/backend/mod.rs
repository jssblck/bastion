//! The backend execution boundary.
//!
//! Bastion does not run its own agent loop; it translates a reviewer's execution
//! profile into a backend's native config and shells out to that backend's CLI
//! (`docs/developer-guide/design.md`). This module defines the contract: [`Backend`], what it is
//! handed ([`ReviewRequest`]) and what it must return ([`ReviewOutcome`]), plus
//! the concrete backends and the dispatch that picks one for a reviewer.
//!
//! The subprocess boundary lives behind [`command::CommandRunner`] so backends can
//! be driven against a fake executable in tests, with no real agent or network.

pub mod claude_code;
pub mod codex;
pub mod command;

use std::collections::BTreeMap;
use std::path::Path;

use color_eyre::eyre::Result;

use crate::event::RunId;
use crate::reviewer::{self, Reviewer};
use crate::verdict::{Decision, Money, Usage, Verdict};

use self::claude_code::ClaudeCodeBackend;
use self::codex::CodexBackend;
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
/// place that grows when a sibling backend lands. The [`Backend`] trait does not.
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
        reviewer::Backend::Codex => CodexBackend::new(SystemCommandRunner).review(request).await,
        // Sibling backends implement the same trait; wire them here as they land.
        reviewer::Backend::Pi => {
            color_eyre::eyre::bail!(
                "the pi backend is not yet wired in this build (reviewer '{}')",
                request.reviewer.name
            )
        }
    }
}

/// The shared preamble that tells a reviewing agent what its changeset is and how
/// to see it, regardless of backend.
///
/// Bastion's notion of "the changeset" is whatever the working tree differs from
/// `base` by (see [`crate::git::changed_files`]): tracked edits *and* new untracked
/// files, committed or not. So the agent must be told to use `git diff {base}`
/// (two-dot: working tree vs base) plus an untracked-file scan -- not
/// `{base}...HEAD`, which shows only committed history and silently misses the
/// uncommitted work an author is iterating on in the local loop. In CI the head is
/// already committed and there are no untracked files, so the same instruction is
/// correct there too.
fn changeset_preamble(base: &str) -> String {
    format!(
        "You are reviewing a changeset computed against the base branch `{base}`. \
         Bastion defines the changeset as everything in the current working tree that \
         differs from `{base}`, which may include uncommitted edits and new, untracked \
         files -- not only committed history. To see exactly what changed, run \
         `git diff {base}` for changes to tracked files, and `git status --short` (or \
         `git ls-files --others --exclude-standard`) to find untracked files, then read \
         those files directly. Do not rely on `git diff {base}...HEAD`: it shows only \
         committed work and will miss local changes that have not been committed yet."
    )
}

/// The shared instruction that tells a reviewing agent to report *every*
/// qualifying finding in one pass, not just the first one it sees.
///
/// A verdict is internally consistent when a `block` carries at least one blocking
/// finding (see [`Verdict::is_consistent`](crate::verdict::Verdict::is_consistent)),
/// so an agent left to its own devices tends to surface one representative issue
/// and stop the moment the verdict is satisfiable. That makes the author fix one
/// thing, push, and burn another full review cycle to see the next, once per issue.
/// Appending this to every reviewer prompt, on both backends, makes a single pass
/// enumerate the complete finding set so the author can fix everything at once. It
/// changes only how completely a reviewer reports, never the gate decision: a clean
/// changeset still returns `pass` with no findings, and the reviewer's own prompt
/// still decides what counts as an issue.
pub(crate) const EXHAUSTIVE_FINDINGS_INSTRUCTION: &str = "\
    Report every issue you can identify across the whole changeset in this single \
    review pass, not just the first or most obvious one. Emit a separate finding \
    for each distinct instance, each with its own location, even when several \
    instances share a cause or fall under the same rule. Do not stop after the \
    first finding or once the verdict is already decided: when you block, a single \
    finding does not discharge the review if other issues remain. Scan every \
    changed file and list them all so the author can fix the complete set in one \
    pass. If the changeset is clean, still return a pass with no findings; do not \
    invent issues to pad the list.";

/// Replace `${key}` occurrences in `template` with values from `inputs`.
///
/// Shared by the backends so prompt interpolation is identical regardless of
/// which agent runs the reviewer. Unknown placeholders are left untouched rather
/// than erroring: the reviewer author is trusted, and a literal `${...}` in a
/// prompt is harmless.
fn interpolate(template: &str, inputs: &BTreeMap<String, String>) -> String {
    if inputs.is_empty() || !template.contains("${") {
        return template.to_string();
    }
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find('}') {
            Some(end) => {
                let key = &after[..end];
                match inputs.get(key) {
                    Some(value) => out.push_str(value),
                    None => {
                        // Unknown key: keep the literal placeholder.
                        out.push_str("${");
                        out.push_str(key);
                        out.push('}');
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                // Unterminated placeholder: emit the rest verbatim and stop.
                out.push_str("${");
                out.push_str(after);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// Convert a dollar amount to [`Money`] (exact cents), rounding to the nearest
/// cent. Negative or non-finite values clamp to zero, so a malformed cost from a
/// backend can never produce a nonsensical charge. Shared by the backends so cost
/// accounting is consistent.
fn money_from_dollars(dollars: f64) -> Money {
    if !dollars.is_finite() || dollars <= 0.0 {
        return Money::from_cents(0);
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "rounded, non-negative cents within u64 range for any realistic cost"
    )]
    let cents = (dollars * 100.0).round() as u64;
    Money::from_cents(cents)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reviewer::Mode;
    use std::path::PathBuf;

    #[test]
    fn interpolate_substitutes_known_keys_and_keeps_unknown() {
        let mut inputs = BTreeMap::new();
        inputs.insert("a".to_string(), "X".to_string());
        assert_eq!(interpolate("${a} ${b}", &inputs), "X ${b}");
        assert_eq!(interpolate("no placeholders", &inputs), "no placeholders");
        assert_eq!(interpolate("${unterminated", &inputs), "${unterminated");
        // No inputs short-circuits to the original.
        assert_eq!(interpolate("${a}", &BTreeMap::new()), "${a}");
    }

    #[test]
    fn changeset_preamble_steers_to_the_working_tree_diff() {
        let preamble = changeset_preamble("origin/main");
        // Names the base and uses the two-dot, working-tree form...
        assert!(preamble.contains("base branch `origin/main`"));
        assert!(preamble.contains("git diff origin/main"));
        // ...covers untracked files Bastion also counts as changed...
        assert!(preamble.contains("untracked"));
        // ...and explicitly warns off the committed-only three-dot form.
        assert!(preamble.contains("Do not rely on `git diff origin/main...HEAD`"));
    }

    #[test]
    fn exhaustive_findings_instruction_demands_a_complete_pass() {
        let text = EXHAUSTIVE_FINDINGS_INSTRUCTION;
        let lower = text.to_lowercase();
        // It must ask for the full set, not the first finding.
        assert!(lower.contains("every issue"));
        assert!(lower.contains("do not stop after the"));
        // And it must not weaken the gate: a clean changeset still passes clean.
        assert!(lower.contains("clean"));
        assert!(lower.contains("return a pass"));
    }

    #[test]
    fn money_from_dollars_rounds_and_clamps() {
        assert_eq!(money_from_dollars(0.21).cents(), 21);
        assert_eq!(money_from_dollars(0.215).cents(), 22);
        assert_eq!(money_from_dollars(-1.0).cents(), 0);
        assert_eq!(money_from_dollars(f64::NAN).cents(), 0);
        assert_eq!(money_from_dollars(f64::INFINITY).cents(), 0);
    }

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
        // Pi is the remaining unwired backend; Claude Code and Codex are wired.
        let reviewer = reviewer(reviewer::Backend::Pi);
        let run = RunId("r".into());
        let root = PathBuf::from(".");
        let request = ReviewRequest {
            reviewer: &reviewer,
            run: &run,
            repo_root: &root,
            base: "main",
        };
        let err = dispatch(&request).await.unwrap_err();
        assert!(err.to_string().contains("pi backend is not yet wired"));
    }
}

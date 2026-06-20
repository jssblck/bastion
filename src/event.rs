//! The run event stream.
//!
//! A run is a sequence of typed events emitted as each thing happens. The same
//! events are streamed to stdout as JSONL (`docs/LOCAL.md`) and persisted to the
//! run's `run.jsonl`; the GitHub surfaces (`docs/GITHUB.md`) mirror them one to
//! one. Verbose detail (transcripts) is deliberately kept *off* the stream and
//! saved to disk instead — hence [`ReviewerResolved::has_transcript`] rather than
//! the transcript itself.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::reviewer::{Backend, Mode};
use crate::verdict::{Decision, Finding, Money, Usage};

/// A run identifier, e.g. `r-0f3a`. Doubles as the run's directory name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RunId(pub String);

impl fmt::Display for RunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl RunId {
    /// Borrow the underlying id string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The name + mode pair announced for each reviewer in a run's opening event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewerRef {
    /// The reviewer's name.
    pub name: String,
    /// Whether it gates or advises.
    pub mode: Mode,
}

/// The gate tally carried by [`RunEvent::RunCompleted`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gates {
    /// Total number of gates triggered.
    pub total: u32,
    /// Gates that passed.
    pub passed: u32,
    /// Gates that blocked (or failed closed).
    pub blocked: u32,
}

/// One event in a run's life cycle.
///
/// Serialized with a `"type"` discriminator using the dotted names from the
/// design (`run.started`, `reviewer.started`, `reviewer.resolved`,
/// `run.completed`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum RunEvent {
    /// The set of reviewers a run will execute — the locally-rendered equivalent
    /// of a PR's pending checks appearing.
    #[serde(rename = "run.started")]
    RunStarted {
        /// The run id.
        run: RunId,
        /// The branch under review.
        branch: String,
        /// The base branch the changeset is computed against.
        base: String,
        /// Number of changed files.
        changed: u32,
        /// The reviewers that matched and will run.
        reviewers: Vec<ReviewerRef>,
    },
    /// A reviewer began executing (its spinner).
    #[serde(rename = "reviewer.started")]
    ReviewerStarted {
        /// The run id.
        run: RunId,
        /// The reviewer name.
        reviewer: String,
        /// Its mode.
        mode: Mode,
        /// The backend it is running on.
        backend: Backend,
    },
    /// A reviewer reached its conclusion, carrying the verdict and findings but
    /// not the transcript (see [`ReviewerResolved::has_transcript`]).
    #[serde(rename = "reviewer.resolved")]
    ReviewerResolved {
        /// The run id.
        run: RunId,
        /// The reviewer name.
        reviewer: String,
        /// The gate decision.
        verdict: Decision,
        /// A human-friendly summary.
        summary: String,
        /// Located findings explaining the decision.
        #[serde(default)]
        findings: Vec<Finding>,
        /// Token and cost accounting, when the backend reports it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
        /// Wall-clock duration in milliseconds.
        duration_ms: u64,
        /// Whether a transcript was saved to disk for this reviewer.
        has_transcript: bool,
    },
    /// The aggregate outcome — the local equivalent of the `bastion` check.
    #[serde(rename = "run.completed")]
    RunCompleted {
        /// The run id.
        run: RunId,
        /// The aggregate gate decision.
        verdict: Decision,
        /// The gate tally.
        gates: Gates,
        /// Total wall-clock duration in milliseconds.
        duration_ms: u64,
        /// Total cost across reviewers.
        cost_usd: Money,
    },
}

impl RunEvent {
    /// The run id this event belongs to.
    #[must_use]
    pub fn run_id(&self) -> &RunId {
        match self {
            RunEvent::RunStarted { run, .. }
            | RunEvent::ReviewerStarted { run, .. }
            | RunEvent::ReviewerResolved { run, .. }
            | RunEvent::RunCompleted { run, .. } => run,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verdict::FindingKind;

    #[test]
    fn resolved_event_matches_the_documented_jsonl_shape() {
        let event = RunEvent::ReviewerResolved {
            run: RunId("r-0f3a".into()),
            reviewer: "tenant-isolation".into(),
            verdict: Decision::Block,
            summary: "A new query path reads rows without scoping by tenant id.".into(),
            findings: vec![Finding {
                kind: FindingKind::Blocking,
                path: "src/server/db.ts".into(),
                line_start: 88,
                line_end: 91,
                detail: "scope this query by tenant_id".into(),
            }],
            usage: Some(Usage {
                tokens_in: 18204,
                tokens_out: 1560,
                cost_usd: Money::from_cents(21),
            }),
            duration_ms: 38120,
            has_transcript: true,
        };

        let line = serde_json::to_string(&event).expect("serializes");
        assert!(line.contains(r#""type":"reviewer.resolved""#));
        assert!(line.contains(r#""verdict":"block""#));
        assert!(line.contains(r#""cost_usd":0.21"#));

        let parsed: RunEvent = serde_json::from_str(&line).expect("round-trips");
        assert_eq!(parsed, event);
        assert_eq!(parsed.run_id().as_str(), "r-0f3a");
    }

    #[test]
    fn run_started_round_trips() {
        let event = RunEvent::RunStarted {
            run: RunId("r-0f3a".into()),
            branch: "feat/cart".into(),
            base: "main".into(),
            changed: 12,
            reviewers: vec![ReviewerRef {
                name: "file-responsibility".into(),
                mode: Mode::Gate,
            }],
        };
        let line = serde_json::to_string(&event).unwrap();
        assert!(line.contains(r#""type":"run.started""#));
        assert_eq!(serde_json::from_str::<RunEvent>(&line).unwrap(), event);
    }
}

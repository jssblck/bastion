//! The parallel, timeout-bounded runner.
//!
//! [`execute`] runs every matched reviewer concurrently, bounds each by its
//! `timeout`, aggregates the results per the merge gate in `docs/developer-guide/design.md`, and
//! emits the full [`RunEvent`] stream. It owns event emission and persistence so
//! [`crate::commands::review`] only has to render the stream and map the aggregate
//! verdict to an exit status.
//!
//! Aggregation is fail-closed for gates and fail-open for advisors: a gate that
//! crashes, times out, or returns an invalid verdict resolves to **block**, never
//! a silent pass; an advisor that does the same is ignored.
//!
//! The backend boundary (the [`Backend`] trait, [`ReviewRequest`]/[`ReviewOutcome`],
//! [`MockBackend`], and dispatch) lives in [`crate::backend`] and is re-exported
//! here for the call sites that predate the split.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use color_eyre::eyre::{Context, Result};
use tokio::task::JoinSet;

use crate::backend::{self, ReviewOutcome, ReviewRequest};
use crate::event::{Gates, ReviewerRef, RunEvent, RunId};
use crate::paths::Layout;
use crate::reviewer::{Mode, Reviewer};
use crate::verdict::{Decision, Money, Usage, Verdict};

// Re-exports so existing imports (`runner::Backend`, `runner::MockBackend`, ...)
// keep resolving after the backend split.
pub use crate::backend::{Backend, MockBackend};

/// A backend factory: produces the [`ReviewOutcome`] for one owned reviewer.
///
/// Production uses [`backend::dispatch`] (the real subprocess path); tests inject
/// a closure that returns canned outcomes, so the runner's concurrency,
/// timeout, aggregation, and persistence logic is exercised without any agent.
type ReviewFn = dyn Fn(OwnedRequest) -> ReviewFuture + Send + Sync + 'static;

/// A boxed, owned-future review (so it is `Send + 'static` for [`JoinSet`]).
type ReviewFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<ReviewOutcome>> + Send>>;

/// An owned review request, decoupled from any borrow so it can cross into a
/// spawned task.
#[derive(Debug, Clone)]
pub struct OwnedRequest {
    /// The reviewer to execute (owned clone).
    pub reviewer: Reviewer,
    /// The run this review belongs to.
    pub run: RunId,
    /// The repository root.
    pub repo_root: PathBuf,
    /// The base branch.
    pub base: String,
}

impl OwnedRequest {
    /// Run this request through the real backend dispatch.
    fn dispatch(self) -> ReviewFuture {
        Box::pin(async move {
            let request = ReviewRequest {
                reviewer: &self.reviewer,
                run: &self.run,
                repo_root: &self.repo_root,
                base: &self.base,
            };
            backend::dispatch(&request).await
        })
    }
}

/// Shared context for executing a run's reviewers.
///
/// Carries everything the runner needs to both execute the reviewers and persist
/// the authoritative `run.started` event, so persistence lives entirely in the
/// runner and the command only renders.
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
    /// Number of changed files (for the persisted `run.started`).
    pub changed: u32,
    /// The reviewers that matched and will run (for the persisted `run.started`).
    pub reviewers: Vec<ReviewerRef>,
}

/// How long a reviewer with no explicit `timeout` is allowed to run before it is
/// failed closed (gate) or skipped (advisor). Chosen to be generous for a heavy
/// agentic review while still bounding a hung backend.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// The fully-resolved result of one reviewer, ready to emit and persist.
struct Resolved {
    reviewer: Reviewer,
    /// The gate decision after applying fail-closed / fail-open policy.
    decision: Decision,
    summary: String,
    findings: Vec<crate::verdict::Finding>,
    usage: Option<Usage>,
    transcript: Option<String>,
    duration: Duration,
    /// Whether this reviewer's outcome counts toward the aggregate gate. Advisors
    /// never do; a failed advisor is ignored entirely.
    counts_as_gate: bool,
}

/// Execute the matched reviewers for a run using the real backends.
///
/// Runs them concurrently with per-reviewer timeouts, emits the full event stream
/// via `emit`, persists the run and per-reviewer artifacts under `layout`, and
/// returns the aggregate [`Decision`]. A `block` aggregate maps to a non-zero exit
/// in the caller.
///
/// # Errors
///
/// Returns an error only if persistence fails; backend failures are absorbed into
/// the aggregate per the fail-closed/fail-open policy and never surface as an
/// error here.
pub async fn execute(
    matched: &[&Reviewer],
    ctx: &ExecContext,
    layout: &Layout,
    emit: &mut dyn FnMut(&RunEvent),
) -> Result<Decision> {
    let exec = |req: OwnedRequest| req.dispatch();
    execute_with(matched, ctx, layout, emit, &exec).await
}

/// [`execute`] with an injectable backend factory, for tests.
///
/// `exec` produces the review future for one owned request; production passes the
/// real [`backend::dispatch`]. The rest — concurrency, timeouts, aggregation,
/// event emission, and persistence — is identical, so tests cover the real paths.
///
/// # Errors
///
/// Returns an error only if persisting the run fails.
pub async fn execute_with(
    matched: &[&Reviewer],
    ctx: &ExecContext,
    layout: &Layout,
    emit: &mut dyn FnMut(&RunEvent),
    exec: &ReviewFn,
) -> Result<Decision> {
    let run_started = Instant::now();

    // Emit `reviewer.started` for each reviewer up front (the pending-checks
    // equivalent), then launch them all concurrently. The started events are also
    // retained for persistence so `run.jsonl` is the *full* event stream the docs
    // promise, not just the resolve/completed tail.
    let mut started_events = Vec::with_capacity(matched.len());
    for reviewer in matched {
        let event = RunEvent::ReviewerStarted {
            run: ctx.run.clone(),
            reviewer: reviewer.name.clone(),
            mode: reviewer.mode,
            backend: reviewer.backend,
        };
        emit(&event);
        started_events.push(event);
    }

    let mut set: JoinSet<(usize, ReviewTaskResult)> = JoinSet::new();
    for (index, reviewer) in matched.iter().enumerate() {
        let request = OwnedRequest {
            reviewer: (*reviewer).clone(),
            run: ctx.run.clone(),
            repo_root: ctx.repo_root.clone(),
            base: ctx.base.clone(),
        };
        let timeout = reviewer.timeout.unwrap_or(DEFAULT_TIMEOUT);
        let future = exec(request);
        set.spawn(async move {
            let started = Instant::now();
            let outcome = match tokio::time::timeout(timeout, future).await {
                Ok(result) => match result {
                    Ok(outcome) => TaskOutcome::Ok(outcome),
                    Err(err) => TaskOutcome::Failed(format!("{err:#}")),
                },
                Err(_elapsed) => TaskOutcome::TimedOut,
            };
            (
                index,
                ReviewTaskResult {
                    outcome,
                    duration: started.elapsed(),
                },
            )
        });
    }

    // Collect results as they complete, then restore registry order so the
    // persisted stream is deterministic regardless of completion timing.
    let mut results: Vec<Option<ReviewTaskResult>> = (0..matched.len()).map(|_| None).collect();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((index, result)) => results[index] = Some(result),
            Err(join_err) => {
                // A panicked task: we have no index, so we cannot place it. This
                // should not happen (tasks catch their own errors), but if it
                // does, it must not silently drop a gate. Fall through; the
                // corresponding slot stays `None` and is treated as a crash below.
                tracing::error!(error = %join_err, "a reviewer task panicked");
            }
        }
    }

    // Resolve each reviewer, applying fail-closed / fail-open policy.
    let mut resolved = Vec::with_capacity(matched.len());
    for (index, reviewer) in matched.iter().enumerate() {
        resolved.push(resolve(reviewer, results[index].take()));
    }

    // Persist per-reviewer artifacts and build the resolve events. The persisted
    // stream opens with the retained `reviewer.started` events so a replay sees the
    // same sequence the live `emit` produced.
    let mut events = started_events;
    events.reserve(matched.len() + 1);
    for item in &resolved {
        persist_reviewer(layout, &ctx.run, item)
            .wrap_err_with(|| format!("persisting reviewer '{}'", item.reviewer.name))?;
        let event = RunEvent::ReviewerResolved {
            run: ctx.run.clone(),
            reviewer: item.reviewer.name.clone(),
            verdict: item.decision,
            summary: item.summary.clone(),
            findings: item.findings.clone(),
            usage: item.usage,
            duration_ms: duration_ms(item.duration),
            has_transcript: item.transcript.is_some(),
        };
        emit(&event);
        events.push(event);
    }

    // Aggregate: all gates must pass.
    let gates = tally(&resolved);
    let aggregate = if gates.blocked == 0 {
        Decision::Pass
    } else {
        Decision::Block
    };
    let cost = total_cost(&resolved);

    let completed = RunEvent::RunCompleted {
        run: ctx.run.clone(),
        verdict: aggregate,
        gates,
        duration_ms: duration_ms(run_started.elapsed()),
        cost_usd: cost,
    };
    emit(&completed);

    // Persist the full stream. The runner owns persistence, so it reconstructs the
    // authoritative `run.started` from the context and prepends it to the resolve
    // and completed events, then writes `run.jsonl` and updates `latest`.
    let mut stream = Vec::with_capacity(events.len() + 1);
    stream.extend(events);
    stream.push(completed);
    persist_run(layout, &ctx.run, ctx, &stream)?;

    Ok(aggregate)
}

/// The raw result of one reviewer task before fail-closed/open policy is applied.
struct ReviewTaskResult {
    outcome: TaskOutcome,
    duration: Duration,
}

/// What a single reviewer task produced.
enum TaskOutcome {
    /// The backend returned a verdict.
    Ok(ReviewOutcome),
    /// The backend ran but failed (bad output, crash, exec error).
    Failed(String),
    /// The reviewer exceeded its timeout.
    TimedOut,
}

/// Apply fail-closed (gate) / fail-open (advisor) policy to one reviewer's raw
/// result, yielding a fully-resolved row.
///
/// A `None` result means the task neither completed nor errored cleanly (a
/// panic); it is treated as a crash, i.e. fail-closed for a gate.
fn resolve(reviewer: &Reviewer, result: Option<ReviewTaskResult>) -> Resolved {
    let is_gate = reviewer.mode == Mode::Gate;
    match result {
        Some(ReviewTaskResult {
            outcome: TaskOutcome::Ok(outcome),
            duration,
        }) => {
            let verdict = outcome.verdict;
            // An advisor never blocks: clamp its decision to pass for aggregation,
            // but keep its findings so they still surface.
            let decision = if is_gate {
                verdict.decision
            } else {
                Decision::Pass
            };
            Resolved {
                reviewer: reviewer.clone(),
                decision,
                summary: verdict.summary,
                findings: verdict.findings,
                usage: outcome.usage,
                transcript: outcome.transcript,
                duration,
                counts_as_gate: is_gate,
            }
        }
        Some(ReviewTaskResult {
            outcome: TaskOutcome::Failed(reason),
            duration,
        }) => fail(reviewer, is_gate, &reason, duration),
        Some(ReviewTaskResult {
            outcome: TaskOutcome::TimedOut,
            duration,
        }) => fail(
            reviewer,
            is_gate,
            &format!(
                "timed out after {}s",
                reviewer.timeout.unwrap_or(DEFAULT_TIMEOUT).as_secs()
            ),
            duration,
        ),
        None => fail(
            reviewer,
            is_gate,
            "the reviewer task crashed",
            Duration::ZERO,
        ),
    }
}

/// Build the resolved row for a failed/timed-out reviewer: a gate fails closed
/// (block, with a synthetic blocking finding), an advisor fails open (pass).
fn fail(reviewer: &Reviewer, is_gate: bool, reason: &str, duration: Duration) -> Resolved {
    if is_gate {
        Resolved {
            reviewer: reviewer.clone(),
            decision: Decision::Block,
            summary: format!("{} did not produce a verdict: {reason}", reviewer.name),
            findings: vec![crate::verdict::Finding {
                kind: crate::verdict::FindingKind::Blocking,
                path: String::new(),
                line_start: 0,
                line_end: 0,
                detail: format!("reviewer failed to complete: {reason}"),
            }],
            usage: None,
            transcript: None,
            duration,
            counts_as_gate: true,
        }
    } else {
        Resolved {
            reviewer: reviewer.clone(),
            decision: Decision::Pass,
            summary: format!("{} skipped (advisor): {reason}", reviewer.name),
            findings: Vec::new(),
            usage: None,
            transcript: None,
            duration,
            counts_as_gate: false,
        }
    }
}

/// Tally the gate outcomes for the `run.completed` event.
fn tally(resolved: &[Resolved]) -> Gates {
    let mut total = 0u32;
    let mut passed = 0u32;
    let mut blocked = 0u32;
    for item in resolved {
        if !item.counts_as_gate {
            continue;
        }
        total += 1;
        if item.decision.is_block() {
            blocked += 1;
        } else {
            passed += 1;
        }
    }
    Gates {
        total,
        passed,
        blocked,
    }
}

/// Sum reported cost across all reviewers.
fn total_cost(resolved: &[Resolved]) -> Money {
    let cents = resolved
        .iter()
        .filter_map(|item| item.usage.map(|u| u.cost_usd.cents()))
        .fold(0u64, u64::saturating_add);
    Money::from_cents(cents)
}

/// Persist one reviewer's saved artifacts: transcript, raw verdict, and metadata.
fn persist_reviewer(layout: &Layout, run: &RunId, item: &Resolved) -> Result<()> {
    let dir = layout.reviewer_dir(run, &item.reviewer.name);
    std::fs::create_dir_all(&dir)
        .wrap_err_with(|| format!("creating reviewer directory {}", dir.display()))?;

    if let Some(transcript) = &item.transcript {
        let path = layout.transcript(run, &item.reviewer.name);
        std::fs::write(&path, transcript)
            .wrap_err_with(|| format!("writing {}", path.display()))?;
    }

    // The raw structured verdict, exactly as aggregated.
    let verdict = Verdict {
        decision: item.decision,
        summary: item.summary.clone(),
        findings: item.findings.clone(),
    };
    let verdict_path = layout.verdict(run, &item.reviewer.name);
    std::fs::write(
        &verdict_path,
        serde_json::to_string_pretty(&verdict).wrap_err("serializing verdict")?,
    )
    .wrap_err_with(|| format!("writing {}", verdict_path.display()))?;

    // Per-reviewer metadata: backend, timing, usage, matched trigger.
    let meta = ReviewerMeta {
        backend: item.reviewer.backend,
        mode: item.reviewer.mode,
        duration_ms: duration_ms(item.duration),
        usage: item.usage,
        trigger: item.reviewer.trigger.clone(),
    };
    let meta_path = layout.meta(run, &item.reviewer.name);
    std::fs::write(
        &meta_path,
        serde_json::to_string_pretty(&meta).wrap_err("serializing reviewer meta")?,
    )
    .wrap_err_with(|| format!("writing {}", meta_path.display()))?;

    Ok(())
}

/// Per-reviewer metadata saved alongside the transcript and verdict.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct ReviewerMeta {
    backend: crate::reviewer::Backend,
    mode: Mode,
    duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<Usage>,
    trigger: Vec<String>,
}

/// Persist the run's event stream, prepending the authoritative `run.started`.
fn persist_run(layout: &Layout, run: &RunId, ctx: &ExecContext, tail: &[RunEvent]) -> Result<()> {
    // The store writes `run.jsonl` and updates `latest`. We reconstruct the
    // opening event here so a replayed run is complete; `changed` is recorded by
    // the caller's emitted event, which is the canonical one shown on screen.
    let started = RunEvent::RunStarted {
        run: run.clone(),
        branch: ctx.branch.clone(),
        base: ctx.base.clone(),
        changed: ctx.changed,
        reviewers: ctx.reviewers.clone(),
    };
    let mut events = Vec::with_capacity(tail.len() + 1);
    events.push(started);
    events.extend_from_slice(tail);
    crate::store::write_run(layout, run, &events)
}

/// Whole-millisecond duration, saturating at `u64::MAX`.
fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reviewer::{self as rev, Capabilities};
    use crate::verdict::{Finding, FindingKind};

    fn reviewer(name: &str, mode: Mode) -> Reviewer {
        Reviewer {
            name: name.into(),
            trigger: vec!["**".into()],
            mode,
            backend: rev::Backend::ClaudeCode,
            timeout: None,
            runner: None,
            env: Default::default(),
            capabilities: Capabilities::default(),
            inputs: Default::default(),
            prompt: "p".into(),
        }
    }

    fn ctx(reviewers: &[&Reviewer]) -> ExecContext {
        ExecContext {
            run: RunId("r-exec".into()),
            repo_root: PathBuf::from("."),
            branch: "feat".into(),
            base: "main".into(),
            changed: u32::try_from(reviewers.len()).unwrap_or(0),
            reviewers: reviewers
                .iter()
                .map(|r| ReviewerRef {
                    name: r.name.clone(),
                    mode: r.mode,
                })
                .collect(),
        }
    }

    fn pass(summary: &str) -> ReviewOutcome {
        ReviewOutcome {
            verdict: Verdict {
                decision: Decision::Pass,
                summary: summary.into(),
                findings: vec![],
            },
            usage: Some(Usage {
                tokens_in: 100,
                tokens_out: 10,
                cost_usd: Money::from_cents(5),
            }),
            transcript: Some("t".into()),
        }
    }

    fn block(summary: &str) -> ReviewOutcome {
        ReviewOutcome {
            verdict: Verdict {
                decision: Decision::Block,
                summary: summary.into(),
                findings: vec![Finding {
                    kind: FindingKind::Blocking,
                    path: "a.rs".into(),
                    line_start: 1,
                    line_end: 1,
                    detail: "fix".into(),
                }],
            },
            usage: None,
            transcript: Some("t".into()),
        }
    }

    /// Drive `execute_with` with a per-reviewer outcome map keyed by name.
    async fn run_scenario(
        reviewers: &[&Reviewer],
        responses: std::collections::HashMap<String, Response>,
    ) -> (Decision, Vec<RunEvent>, Layout) {
        let tmp = tempfile::tempdir().unwrap();
        let layout = Layout::with_root(tmp.path().to_path_buf());
        // Keep the tempdir alive for the duration by leaking it into the layout's
        // lifetime via a Box; tests read the layout immediately after.
        std::mem::forget(tmp);

        let ctx = ctx(reviewers);
        let responses = std::sync::Arc::new(responses);
        let exec = move |req: OwnedRequest| -> ReviewFuture {
            let responses = responses.clone();
            Box::pin(async move {
                match responses.get(&req.reviewer.name).cloned() {
                    Some(Response::Outcome(o)) => Ok(o),
                    Some(Response::Error(msg)) => Err(color_eyre::eyre::eyre!(msg)),
                    Some(Response::Hang(d)) => {
                        tokio::time::sleep(d).await;
                        Ok(pass("late"))
                    }
                    None => Ok(pass("default")),
                }
            })
        };

        let mut events = Vec::new();
        let decision = execute_with(
            reviewers,
            &ctx,
            &layout,
            &mut |e| events.push(e.clone()),
            &exec,
        )
        .await
        .expect("execute persists");
        (decision, events, layout)
    }

    #[derive(Clone)]
    enum Response {
        Outcome(ReviewOutcome),
        Error(String),
        Hang(Duration),
    }

    fn responses(pairs: Vec<(&str, Response)>) -> std::collections::HashMap<String, Response> {
        pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
    }

    #[tokio::test]
    async fn all_gates_pass_aggregates_to_pass() {
        let g1 = reviewer("g1", Mode::Gate);
        let g2 = reviewer("g2", Mode::Gate);
        let reviewers = [&g1, &g2];
        let (decision, events, layout) = run_scenario(
            &reviewers,
            responses(vec![
                ("g1", Response::Outcome(pass("ok1"))),
                ("g2", Response::Outcome(pass("ok2"))),
            ]),
        )
        .await;

        assert_eq!(decision, Decision::Pass);
        // started events came from the runner; completed says 2/2.
        let completed = events
            .iter()
            .find_map(|e| match e {
                RunEvent::RunCompleted { gates, verdict, .. } => Some((*gates, *verdict)),
                _ => None,
            })
            .unwrap();
        assert_eq!(completed.1, Decision::Pass);
        assert_eq!(completed.0.total, 2);
        assert_eq!(completed.0.passed, 2);

        // Persisted: run.jsonl, plus per-reviewer artifacts.
        let runs = crate::store::list_runs(&layout).unwrap();
        assert_eq!(runs.len(), 1);
        assert!(layout.transcript(&RunId("r-exec".into()), "g1").exists());
        assert!(layout.verdict(&RunId("r-exec".into()), "g1").exists());
        assert!(layout.meta(&RunId("r-exec".into()), "g1").exists());
    }

    #[tokio::test]
    async fn one_blocking_gate_blocks_the_run() {
        let g1 = reviewer("g1", Mode::Gate);
        let g2 = reviewer("g2", Mode::Gate);
        let reviewers = [&g1, &g2];
        let (decision, _events, _layout) = run_scenario(
            &reviewers,
            responses(vec![
                ("g1", Response::Outcome(pass("ok"))),
                ("g2", Response::Outcome(block("bad"))),
            ]),
        )
        .await;
        assert_eq!(decision, Decision::Block);
    }

    #[tokio::test]
    async fn a_failing_gate_fails_closed() {
        let g1 = reviewer("g1", Mode::Gate);
        let reviewers = [&g1];
        let (decision, events, layout) = run_scenario(
            &reviewers,
            responses(vec![("g1", Response::Error("backend exploded".into()))]),
        )
        .await;
        assert_eq!(decision, Decision::Block);
        // The resolve event carries a block with the failure reason.
        let resolved = events
            .iter()
            .find_map(|e| match e {
                RunEvent::ReviewerResolved {
                    verdict, summary, ..
                } => Some((*verdict, summary.clone())),
                _ => None,
            })
            .unwrap();
        assert_eq!(resolved.0, Decision::Block);
        assert!(resolved.1.contains("did not produce a verdict"));
        // No transcript was saved for a crashed gate, but a verdict still was.
        assert!(layout.verdict(&RunId("r-exec".into()), "g1").exists());
        assert!(!layout.transcript(&RunId("r-exec".into()), "g1").exists());
    }

    #[tokio::test]
    async fn a_failing_advisor_is_ignored() {
        let g1 = reviewer("g1", Mode::Gate);
        let a1 = reviewer("a1", Mode::Advisor);
        let reviewers = [&g1, &a1];
        let (decision, events, _layout) = run_scenario(
            &reviewers,
            responses(vec![
                ("g1", Response::Outcome(pass("ok"))),
                ("a1", Response::Error("advisor died".into())),
            ]),
        )
        .await;
        // The failed advisor does not block.
        assert_eq!(decision, Decision::Pass);
        // The tally counts only the one gate.
        let gates = events
            .iter()
            .find_map(|e| match e {
                RunEvent::RunCompleted { gates, .. } => Some(*gates),
                _ => None,
            })
            .unwrap();
        assert_eq!(gates.total, 1);
    }

    #[tokio::test]
    async fn an_advisor_block_does_not_block_the_run() {
        // Even a clean `block` verdict from an advisor is non-blocking.
        let a1 = reviewer("a1", Mode::Advisor);
        let reviewers = [&a1];
        let (decision, _events, _layout) = run_scenario(
            &reviewers,
            responses(vec![("a1", Response::Outcome(block("advisory concern")))]),
        )
        .await;
        assert_eq!(decision, Decision::Pass);
    }

    #[tokio::test(start_paused = true)]
    async fn a_timed_out_gate_blocks() {
        let mut g1 = reviewer("g1", Mode::Gate);
        g1.timeout = Some(Duration::from_secs(1));
        let reviewers = [&g1];
        let (decision, events, _layout) = run_scenario(
            &reviewers,
            responses(vec![("g1", Response::Hang(Duration::from_secs(60)))]),
        )
        .await;
        assert_eq!(decision, Decision::Block);
        let summary = events
            .iter()
            .find_map(|e| match e {
                RunEvent::ReviewerResolved { summary, .. } => Some(summary.clone()),
                _ => None,
            })
            .unwrap();
        assert!(summary.contains("timed out"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_timed_out_advisor_is_ignored() {
        let mut a1 = reviewer("a1", Mode::Advisor);
        a1.timeout = Some(Duration::from_secs(1));
        let reviewers = [&a1];
        let (decision, _events, _layout) = run_scenario(
            &reviewers,
            responses(vec![("a1", Response::Hang(Duration::from_secs(60)))]),
        )
        .await;
        assert_eq!(decision, Decision::Pass);
    }

    #[tokio::test]
    async fn persisted_run_jsonl_is_the_full_event_stream() {
        // run.jsonl must contain the started events too, not just resolve/completed,
        // so a replay sees the same sequence the live stream emitted.
        let g1 = reviewer("g1", Mode::Gate);
        let reviewers = [&g1];
        let (_decision, _events, layout) = run_scenario(
            &reviewers,
            responses(vec![("g1", Response::Outcome(pass("ok")))]),
        )
        .await;

        let persisted = crate::store::read_run(&layout, &RunId("r-exec".into())).unwrap();
        assert!(
            matches!(persisted.first(), Some(RunEvent::RunStarted { .. })),
            "stream must open with run.started"
        );
        assert!(
            persisted
                .iter()
                .any(|e| matches!(e, RunEvent::ReviewerStarted { .. })),
            "stream must include reviewer.started"
        );
        assert!(
            persisted
                .iter()
                .any(|e| matches!(e, RunEvent::ReviewerResolved { .. })),
            "stream must include reviewer.resolved"
        );
        assert!(
            matches!(persisted.last(), Some(RunEvent::RunCompleted { .. })),
            "stream must close with run.completed"
        );
    }

    #[tokio::test]
    async fn cost_is_summed_across_reviewers() {
        let g1 = reviewer("g1", Mode::Gate);
        let g2 = reviewer("g2", Mode::Gate);
        let reviewers = [&g1, &g2];
        let (_decision, events, _layout) = run_scenario(
            &reviewers,
            responses(vec![
                ("g1", Response::Outcome(pass("a"))), // 5 cents
                ("g2", Response::Outcome(pass("b"))), // 5 cents
            ]),
        )
        .await;
        let cost = events
            .iter()
            .find_map(|e| match e {
                RunEvent::RunCompleted { cost_usd, .. } => Some(*cost_usd),
                _ => None,
            })
            .unwrap();
        assert_eq!(cost, Money::from_cents(10));
    }
}

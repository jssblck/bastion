//! Turning a finished run into GitHub surfaces: a sticky PR comment and check runs.
//!
//! This is the reporting half of the adapter. It reads the same [`RunEvent`]
//! stream the local surface renders and `run.jsonl` persists, and maps it onto two
//! GitHub surfaces described in `docs/developer-guide/github-adapter.md`:
//!
//! - **One sticky PR comment** carrying every reviewer's verdict and *all* of its
//!   findings, blocking and optional alike. Optional findings never gate, so this
//!   is the one place a reader sees them without opening the run artifact; the
//!   comment is upserted in place (matched by a hidden marker) so a re-run rewrites
//!   it rather than stacking duplicates.
//! - **A check run per reviewer plus an always-present aggregate `bastion` check**,
//!   so the PR's checks list shows exactly which reviewers ran and how each landed.
//!   A blocking gate reports `failure`; a passing gate and any advisor report
//!   `success`. Located findings ride along as check annotations.
//!
//! All the event-to-markdown and event-to-payload mapping here is pure and unit
//! tested; the only side effects are the [`GitHubApi`] calls in [`report`].

use std::fmt;

use color_eyre::eyre::{Context, Result, bail};

use crate::event::{Gates, RunEvent, RunId};
use crate::reviewer::{Backend, Mode};
use crate::verdict::{Decision, Finding, FindingKind, Money, Usage};

use super::PrContext;
use super::client::{ApiRequest, ApiResponse, GitHubApi, IssueComment};

/// The hidden HTML marker that identifies Bastion's own sticky comment, so a
/// re-run finds and rewrites it instead of posting a duplicate. Invisible in the
/// rendered comment.
pub const MARKER: &str = "<!-- bastion-report -->";

/// GitHub accepts at most 50 annotations per check-run request. We cap to that and
/// note any overflow in the check summary rather than silently dropping it.
const MAX_ANNOTATIONS: usize = 50;

/// GitHub caps a check-run annotation `message` (documented at 64KB). A single
/// oversized finding would 422 the whole report request, so we truncate the inline
/// message well under the limit; the full finding text still rides the sticky comment
/// and the reviewer check summary, so nothing is lost. Measured in characters, which
/// for any UTF-8 byte width stays comfortably below 64KB.
const MAX_ANNOTATION_MESSAGE: usize = 8000;

/// GitHub caps a check-run `output.summary` (and `output.text`) at 65535 bytes. The
/// summary embeds untrusted reviewer findings, so a single verbose finding could blow
/// the limit and 422 the whole request, failing an otherwise green job. We cap the
/// assembled summary well under that ceiling and point overflow at the sticky comment,
/// which carries the full text. Measured in characters: even all-4-byte content stays
/// under 65535 bytes.
const MAX_CHECK_SUMMARY: usize = 60000;

/// One reviewer's resolved row, distilled from the event stream.
#[derive(Debug, Clone)]
struct ReviewerRow {
    name: String,
    mode: Mode,
    backend: Option<Backend>,
    decision: Decision,
    summary: String,
    findings: Vec<Finding>,
    duration_ms: u64,
    usage: Option<Usage>,
}

impl ReviewerRow {
    /// The at-a-glance verdict word for this row. An advisor never gates, so it
    /// reads as `advisory` regardless of the decision the runner clamped to pass.
    fn verdict_word(&self) -> &'static str {
        match self.mode {
            Mode::Advisor => "advisory",
            Mode::Gate => self.decision.as_str(),
        }
    }
}

/// The whole run, distilled from its event stream into the shape both surfaces
/// render from.
#[derive(Debug, Clone, Default)]
struct RunDigest {
    branch: Option<String>,
    base: Option<String>,
    changed: u32,
    /// The reviewers `run.started` announced, with their modes. Kept so the report
    /// can detect a started gate that never produced a resolved row and fail closed.
    started: Vec<(String, Mode)>,
    /// How many `run.started` events the stream carried. A well-formed run announces
    /// its plan exactly once; anything else (zero, or a duplicated/forged plan) means
    /// the reviewer set cannot be trusted, so the aggregate must fail closed.
    started_count: u32,
    /// How many `run.completed` events the stream carried. A well-formed run reports
    /// completion exactly once.
    completed_count: u32,
    /// Set when the stream is structurally invalid: events out of order (a reviewer or
    /// completion event before the plan `run.started`, or anything after
    /// `run.completed`), or events that do not all share one run id (a spliced
    /// stream, e.g. a `reviewer.resolved` from a different run grafted onto this one).
    /// Such a stream is not a trustworthy record, so it fails closed even when the
    /// per-event tallies happen to line up. Defaults to `false` (well formed) so an
    /// empty digest is not spuriously malformed.
    malformed: bool,
    rows: Vec<ReviewerRow>,
    aggregate: Option<Decision>,
    gates: Option<Gates>,
    cost: Option<Money>,
    duration_ms: Option<u64>,
}

/// Fold an event stream into a [`RunDigest`].
///
/// `reviewer.started` carries the backend and `run.started` carries each
/// reviewer's mode; both are joined onto the `reviewer.resolved` rows by name so a
/// row knows whether it gated and what ran it.
fn digest(events: &[RunEvent]) -> RunDigest {
    let mut digest = RunDigest::default();
    let mut started: Vec<(String, Mode)> = Vec::new();
    let mut backends: Vec<(String, Backend)> = Vec::new();
    let mut run_id: Option<&RunId> = None;

    for event in events {
        // Structural ordering: `run.started` must precede every reviewer and
        // completion event, and nothing may follow `run.completed`. A stream that
        // breaks this is not a trustworthy record (it may have been reordered or
        // truncated), so mark it and fail closed regardless of how the tallies look.
        let is_start = matches!(event, RunEvent::RunStarted { .. });
        if (!is_start && digest.started_count == 0) || digest.completed_count >= 1 {
            digest.malformed = true;
        }
        // Run-id coherence: every event must belong to the same run. A stream that
        // splices a `reviewer.resolved` from another run onto this run's
        // start/complete can satisfy the row counts and tally yet never prove the
        // selected run's gate actually resolved, so a mismatch fails closed.
        match run_id {
            None => run_id = Some(event.run_id()),
            Some(seen) if seen != event.run_id() => digest.malformed = true,
            Some(_) => {}
        }
        match event {
            RunEvent::RunStarted {
                branch,
                base,
                changed,
                reviewers,
                ..
            } => {
                digest.branch = Some(branch.clone());
                digest.base = Some(base.clone());
                digest.changed = *changed;
                digest.started_count = digest.started_count.saturating_add(1);
                started = reviewers.iter().map(|r| (r.name.clone(), r.mode)).collect();
            }
            RunEvent::ReviewerStarted {
                reviewer, backend, ..
            } => backends.push((reviewer.clone(), *backend)),
            RunEvent::ReviewerResolved {
                reviewer,
                verdict,
                summary,
                findings,
                usage,
                duration_ms,
                ..
            } => {
                let mode = started
                    .iter()
                    .find(|(name, _)| name == reviewer)
                    .map_or(Mode::Gate, |(_, mode)| *mode);
                let backend = backends
                    .iter()
                    .find(|(name, _)| name == reviewer)
                    .map(|(_, backend)| *backend);
                digest.rows.push(ReviewerRow {
                    name: reviewer.clone(),
                    mode,
                    backend,
                    decision: *verdict,
                    summary: summary.clone(),
                    findings: findings.clone(),
                    duration_ms: *duration_ms,
                    usage: *usage,
                });
            }
            RunEvent::RunCompleted {
                verdict,
                gates,
                duration_ms,
                cost_usd,
                ..
            } => {
                digest.completed_count = digest.completed_count.saturating_add(1);
                digest.aggregate = Some(*verdict);
                digest.gates = Some(*gates);
                digest.duration_ms = Some(*duration_ms);
                digest.cost = Some(*cost_usd);
            }
        }
    }
    digest.started = started;
    digest
}

/// Whether a resolved gate row must be treated as blocking, computed fail-closed.
///
/// A gate row blocks if it decided to block, or if it is internally inconsistent: a
/// `pass` that nonetheless carries a blocking finding, which mirrors
/// [`crate::verdict::Verdict::is_consistent`] and can never be trusted as a pass.
/// Advisors never gate, so they never block.
fn gate_row_blocks(row: &ReviewerRow) -> bool {
    row.mode == Mode::Gate
        && (row.decision == Decision::Block
            || row.findings.iter().any(|f| f.kind == FindingKind::Blocking))
}

/// Whether a resolved row's recorded decision is internally consistent with its
/// findings, mirroring [`crate::verdict::Verdict::is_consistent`]: a `block` must
/// carry at least one blocking finding, and a `pass` must carry none.
///
/// This holds for every mode, not just gates: an advisor row that records `pass`
/// while carrying a blocking finding is just as self-contradictory as a gate one, and
/// a persisted run with any such row has been corrupted or hand-edited. Replay must
/// reject it the same way the backend parser rejects an inconsistent live verdict,
/// rather than publishing a check off a contradictory record.
fn row_is_consistent(row: &ReviewerRow) -> bool {
    let has_blocking = row.findings.iter().any(|f| f.kind == FindingKind::Blocking);
    match row.decision {
        Decision::Pass => !has_blocking,
        Decision::Block => has_blocking,
    }
}

/// Whether the replayed run is structurally complete, well-ordered, and internally
/// consistent enough to be trusted at all, independent of whether it passed.
///
/// The report replays a persisted (and therefore untrusted) `run.jsonl`, which can be
/// truncated, reordered, or hand-edited. Before any verdict it carries can be
/// believed, the stream must prove itself a complete and self-consistent record: it
/// announces its plan exactly once (one `run.started`), completes exactly once (one
/// `run.completed`), is in a valid event order, produced a resolved gate row for
/// every gate the plan announced, and carries a recorded gate tally that agrees with
/// the rows. A stream that fails any of these cannot rule out omitted or dropped
/// gates, or has a record that contradicts itself, so neither the aggregate nor any
/// per-reviewer check may report success off it. This is the shared trust gate both
/// [`is_clean_pass`] and the per-reviewer checks build on.
fn is_well_formed_run(digest: &RunDigest) -> bool {
    if digest.malformed {
        return false;
    }
    // The plan is announced once and the run completes once. A stream missing its
    // `run.started` cannot prove which reviewers were meant to run (omitted gates
    // cannot be ruled out); one missing its `run.completed` is truncated; a
    // duplicated either is not a single coherent record.
    if digest.started_count != 1 || digest.completed_count != 1 {
        return false;
    }
    // No resolved row may contradict itself (a block with no blocking finding, or a
    // pass that carries one). Such a row is a corrupted record, untrustworthy for any
    // mode, so the whole run fails closed rather than publishing a check off it.
    if !digest.rows.iter().all(row_is_consistent) {
        return false;
    }
    // Every gate the plan announced produced a matching resolved gate row.
    let resolved_gates = digest.rows.iter().filter(|r| r.mode == Mode::Gate).count();
    let started_gates = digest
        .started
        .iter()
        .filter(|(_, mode)| *mode == Mode::Gate)
        .count();
    if started_gates != resolved_gates {
        return false;
    }
    for (name, mode) in &digest.started {
        if *mode == Mode::Gate
            && !digest
                .rows
                .iter()
                .any(|r| &r.name == name && r.mode == Mode::Gate)
        {
            return false;
        }
    }
    // The recorded tally, when present, must agree with the rows on every field.
    // Deriving `passed` from the rows (resolved gates minus blocked ones) and
    // requiring all three of total/passed/blocked to match closes the gap where an
    // internally inconsistent tally (say total=1, passed=0, blocked=0) would still
    // be trusted; it also implies the `total == passed + blocked` invariant. This is
    // a property of the record, not of the pass decision, so it lives here where both
    // the aggregate and the per-reviewer checks honor it.
    if let Some(gates) = digest.gates {
        let resolved = u32::try_from(resolved_gates).unwrap_or(u32::MAX);
        let blocked = u32::try_from(digest.rows.iter().filter(|r| gate_row_blocks(r)).count())
            .unwrap_or(u32::MAX);
        let passed = resolved.saturating_sub(blocked);
        if gates.total != resolved || gates.blocked != blocked || gates.passed != passed {
            return false;
        }
    }
    true
}

/// Whether the replayed run is a clean pass the aggregate check may report as
/// `success`, computed fail-closed from the reviewer rows rather than by trusting
/// the recorded `run.completed` verdict.
///
/// Builds on [`is_well_formed_run`]: the run must first be a complete, well-ordered,
/// internally consistent record, and then the rows must independently agree it
/// passed: the recorded aggregate is `pass` and no gate row blocks or contradicts
/// itself. Any inconsistency fails closed.
fn is_clean_pass(digest: &RunDigest) -> bool {
    if !is_well_formed_run(digest) {
        return false;
    }
    if digest.aggregate != Some(Decision::Pass) {
        return false;
    }
    // No gate row may block or contradict itself.
    if digest.rows.iter().any(gate_row_blocks) {
        return false;
    }
    true
}

// ---------------------------------------------------------------------------
// Sticky PR comment
// ---------------------------------------------------------------------------

/// Render the sticky PR comment body (Markdown), led by the hidden [`MARKER`].
fn comment_body(digest: &RunDigest) -> String {
    let mut out = String::new();
    out.push_str(MARKER);
    out.push('\n');
    out.push_str("## Bastion review\n\n");
    out.push_str(&status_line(digest));
    out.push_str("\n\n");

    if digest.rows.is_empty() {
        out.push_str("No reviewers were triggered by this change.\n");
        out.push_str(&footer());
        return out;
    }

    out.push_str(&reviewer_table(digest));

    let with_findings: Vec<&ReviewerRow> = digest
        .rows
        .iter()
        .filter(|r| !r.findings.is_empty())
        .collect();
    if !with_findings.is_empty() {
        out.push_str("\n### Findings\n");
        for row in with_findings {
            out.push_str(&format!("\n#### `{}` ({})\n", row.name, row.mode.as_str()));
            for finding in &row.findings {
                out.push_str(&finding_bullet(finding));
            }
        }
    }

    out.push_str(&footer());
    out
}

/// The one-line headline: the aggregate decision plus the gate tally and run cost.
fn status_line(digest: &RunDigest) -> String {
    let reviewers = digest.rows.len();
    let (passed, total) = digest.gates.map_or((0, 0), |g| (g.passed, g.total));
    let timing = digest
        .duration_ms
        .map(|ms| format!(", {}s", ms / 1000))
        .unwrap_or_default();
    let cost = digest
        .cost
        .filter(|c| c.cents() > 0)
        .map(|c| format!(", {c}"))
        .unwrap_or_default();

    // Fail closed: only call it passed when the rows independently agree, never on
    // the recorded verdict alone.
    let headline = if is_clean_pass(digest) {
        if total == 0 {
            "**Passed.** No gates were triggered.".to_string()
        } else {
            format!("**Passed.** All {total} gate(s) passed.")
        }
    } else {
        match digest.aggregate {
            Some(Decision::Pass) => {
                "**Blocked.** The recorded run is internally inconsistent; failing closed."
                    .to_string()
            }
            Some(Decision::Block) => {
                format!("**Blocked.** {passed} of {total} gate(s) passed.")
            }
            None => "**Incomplete.** The run did not finish.".to_string(),
        }
    };
    format!("{headline} {reviewers} reviewer(s) ran{timing}{cost}.")
}

/// The reviewer summary table.
fn reviewer_table(digest: &RunDigest) -> String {
    let mut out =
        String::from("| Reviewer | Mode | Verdict | Summary |\n| --- | --- | --- | --- |\n");
    for row in &digest.rows {
        out.push_str(&format!(
            "| `{}` | {} | {} | {} |\n",
            row.name,
            row.mode.as_str(),
            row.verdict_word(),
            escape_cell(&row.summary),
        ));
    }
    out
}

/// One finding rendered as a Markdown bullet. A located finding cites its path and
/// line range; a synthetic finding (the fail-closed reviewer-crash marker, which
/// has no path) is rendered without a location.
fn finding_bullet(finding: &Finding) -> String {
    let kind = match finding.kind {
        FindingKind::Blocking => "blocking",
        FindingKind::Optional => "optional",
    };
    if finding.path.is_empty() {
        format!("- **{kind}**: {}\n", finding.detail.trim())
    } else {
        format!(
            "- **{kind}** `{}`: {}\n",
            location(&finding.path, finding.line_start, finding.line_end),
            finding.detail.trim(),
        )
    }
}

/// `path:line` or `path:start-end` for a finding's location.
fn location(path: &str, start: u32, end: u32) -> String {
    if start == end {
        format!("{path}:{start}")
    } else {
        format!("{path}:{start}-{end}")
    }
}

/// Neutralize Markdown table-breaking characters in a free-text cell: pipes would
/// start a new column and newlines would end the row.
fn escape_cell(text: &str) -> String {
    text.replace('|', "\\|").replace(['\n', '\r'], " ")
}

/// The trailing note, identical on every comment.
fn footer() -> String {
    "\n<sub>Posted by Bastion. Full transcripts are attached to the workflow run as an artifact.</sub>\n".to_string()
}

// ---------------------------------------------------------------------------
// Check runs
// ---------------------------------------------------------------------------

/// A check-run conclusion, limited to the three the adapter emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conclusion {
    /// A passing gate, or any advisor.
    Success,
    /// A blocking gate (or the aggregate when any gate blocked).
    Failure,
    /// Reserved for non-gating states; unused today but kept explicit.
    Neutral,
}

impl Conclusion {
    fn as_str(self) -> &'static str {
        match self {
            Conclusion::Success => "success",
            Conclusion::Failure => "failure",
            Conclusion::Neutral => "neutral",
        }
    }
}

/// A check-run annotation: a located finding pinned to the diff.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Annotation {
    path: String,
    start_line: u32,
    end_line: u32,
    level: &'static str,
    message: String,
}

/// A fully-resolved check run ready to POST.
#[derive(Debug, Clone)]
struct CheckRun {
    name: String,
    head_sha: String,
    conclusion: Conclusion,
    title: String,
    summary: String,
    annotations: Vec<Annotation>,
}

/// Build the per-reviewer check runs plus the aggregate `bastion` check.
///
/// The aggregate is always present so it can serve as the single stable required
/// check, even when zero reviewers matched (a trivial pass).
fn check_runs(ctx: &PrContext, digest: &RunDigest) -> Vec<CheckRun> {
    // A per-reviewer gate may only conclude success when the run as a whole is a
    // complete, well-ordered record. Otherwise a truncated or reordered stream (one
    // that fails the aggregate) could still publish a green check for an individual
    // passing row, which the replay path has not proven trustworthy.
    let run_ok = is_well_formed_run(digest);
    let mut checks: Vec<CheckRun> = digest
        .rows
        .iter()
        .map(|row| reviewer_check(ctx, row, run_ok))
        .collect();
    checks.push(aggregate_check(ctx, digest));
    checks
}

/// The check run for one reviewer.
///
/// `run_ok` is whether the whole run is a structurally complete, well-ordered,
/// internally consistent record ([`is_well_formed_run`]); no check may publish a
/// green success off a run that is not.
fn reviewer_check(ctx: &PrContext, row: &ReviewerRow, run_ok: bool) -> CheckRun {
    // Fail closed: a gate that blocks, that passed while carrying a blocking finding
    // (self-contradictory), or that sits in an incomplete/reordered run concludes
    // failure. An advisor never gates (so it never concludes failure), but it must
    // not publish a green success off an untrustworthy run either: in that case it
    // concludes `neutral`, which stays non-blocking yet does not claim a clean pass.
    let blocks = gate_row_blocks(row);
    let (conclusion, decision_word) = match row.mode {
        Mode::Advisor if !run_ok => (Conclusion::Neutral, "Advisory (unverified)"),
        Mode::Advisor => (Conclusion::Success, "Advisory"),
        Mode::Gate if blocks => (Conclusion::Failure, "Blocked"),
        Mode::Gate if !run_ok => (Conclusion::Failure, "Unverified"),
        Mode::Gate => (Conclusion::Success, "Passed"),
    };
    let title = format!("{decision_word}: {}", truncate(&row.summary, 110));

    let annotations = annotations_for(&row.findings);
    let summary = cap_check_summary(reviewer_check_summary(row, &annotations));

    CheckRun {
        name: format!("bastion / {}", row.name),
        head_sha: ctx.head_sha.clone(),
        conclusion,
        title,
        summary,
        annotations,
    }
}

/// The aggregate `bastion` check, reflecting the whole-run gate.
fn aggregate_check(ctx: &PrContext, digest: &RunDigest) -> CheckRun {
    // Fail closed: the aggregate concludes success only when the rows independently
    // agree it is a clean pass. A recorded pass that the rows contradict, a run that
    // never completed, or a recorded block all conclude failure, never a silent pass.
    let clean = is_clean_pass(digest);
    let conclusion = if clean {
        Conclusion::Success
    } else {
        Conclusion::Failure
    };
    let (passed, total) = digest.gates.map_or((0, 0), |g| (g.passed, g.total));
    let title = if clean {
        if total == 0 {
            "No gates triggered".to_string()
        } else {
            format!("{passed}/{total} gates passed")
        }
    } else {
        match digest.aggregate {
            Some(Decision::Pass) => "Blocked: recorded run is internally inconsistent".to_string(),
            Some(Decision::Block) => format!("Blocked: {passed}/{total} gates passed"),
            None => "Incomplete run".to_string(),
        }
    };

    let mut summary = String::new();
    summary.push_str(&status_line(digest));
    summary.push_str("\n\n");
    // When the recorded run claims a pass but the rows disagree, explain the
    // fail-closed override so a reader is not confused by a green-looking run.
    if !clean && digest.aggregate == Some(Decision::Pass) {
        summary.push_str(
            "> Note: the recorded run reported a pass, but its reviewer rows are internally \
             inconsistent (a missing or self-contradictory gate, or a tally that disagrees with \
             the rows). Failing the aggregate closed.\n\n",
        );
    }
    if digest.rows.is_empty() {
        summary.push_str("No reviewers were triggered by this change.\n");
    } else {
        summary.push_str(&reviewer_table(digest));
    }

    CheckRun {
        name: "bastion".to_string(),
        head_sha: ctx.head_sha.clone(),
        conclusion,
        title,
        summary: cap_check_summary(summary),
        annotations: Vec::new(),
    }
}

/// The Markdown body of a per-reviewer check run: a small metadata block, the
/// reviewer's own summary, and its findings.
fn reviewer_check_summary(row: &ReviewerRow, annotations: &[Annotation]) -> String {
    let backend = row.backend.map_or("unknown", Backend::as_str);
    let mut out = format!(
        "- Mode: {}\n- Agent: {backend}\n- Verdict: {}\n- Duration: {}s\n",
        row.mode.as_str(),
        row.verdict_word(),
        row.duration_ms / 1000,
    );
    if let Some(usage) = row.usage {
        out.push_str(&format!(
            "- Tokens: {} in, {} out ({})\n",
            usage.tokens_in, usage.tokens_out, usage.cost_usd,
        ));
    }
    out.push('\n');
    out.push_str(&row.summary);
    out.push('\n');

    if !row.findings.is_empty() {
        out.push_str("\n**Findings**\n\n");
        for finding in &row.findings {
            out.push_str(&finding_bullet(finding));
        }
    }
    // Annotations are capped per request; if findings overflowed the cap, say so.
    let located = row.findings.iter().filter(|f| is_locatable(f)).count();
    if located > annotations.len() {
        out.push_str(&format!(
            "\n_{} more located finding(s) are listed above but not pinned to the diff (annotation cap)._\n",
            located - annotations.len(),
        ));
    }
    out
}

/// Whether a finding can become a check annotation: it needs a real path and a
/// first line that is at least 1 (GitHub rejects line 0). The synthetic
/// reviewer-crash finding has neither.
fn is_locatable(finding: &Finding) -> bool {
    !finding.path.is_empty() && finding.line_start >= 1
}

/// The annotation `message` for a finding, truncated to [`MAX_ANNOTATION_MESSAGE`]
/// so a single long finding cannot 422 the whole report request. When cut, it points
/// the reader to the sticky comment, which always carries the full finding text.
fn annotation_message(detail: &str) -> String {
    let detail = detail.trim();
    if detail.chars().count() <= MAX_ANNOTATION_MESSAGE {
        return detail.to_string();
    }
    let kept: String = detail.chars().take(MAX_ANNOTATION_MESSAGE).collect();
    format!(
        "{}\n\n(truncated; see the Bastion comment for the full finding.)",
        kept.trim_end()
    )
}

/// Cap an assembled check-run summary at [`MAX_CHECK_SUMMARY`] so an untrusted,
/// verbose finding cannot push `output.summary` past GitHub's 65535-byte limit and
/// 422 the request. When cut, it points the reader at the sticky comment, which
/// carries every finding in full.
fn cap_check_summary(summary: String) -> String {
    if summary.chars().count() <= MAX_CHECK_SUMMARY {
        return summary;
    }
    let kept: String = summary.chars().take(MAX_CHECK_SUMMARY).collect();
    format!(
        "{}\n\n(truncated; see the Bastion comment for the full findings.)\n",
        kept.trim_end()
    )
}

/// Map a reviewer's locatable findings to check annotations, capped at
/// [`MAX_ANNOTATIONS`] in count and [`MAX_ANNOTATION_MESSAGE`] per message.
fn annotations_for(findings: &[Finding]) -> Vec<Annotation> {
    findings
        .iter()
        .filter(|f| is_locatable(f))
        .take(MAX_ANNOTATIONS)
        .map(|f| Annotation {
            path: f.path.clone(),
            start_line: f.line_start,
            // GitHub requires end_line >= start_line.
            end_line: f.line_end.max(f.line_start),
            level: match f.kind {
                FindingKind::Blocking => "failure",
                FindingKind::Optional => "warning",
            },
            message: annotation_message(&f.detail),
        })
        .collect()
}

/// Truncate `text` to `max` characters, adding an ellipsis marker when cut. Kept
/// ASCII (`...`) to match the house style.
fn truncate(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let kept: String = trimmed.chars().take(max.saturating_sub(3)).collect();
    format!("{}...", kept.trim_end())
}

// ---------------------------------------------------------------------------
// Request construction
// ---------------------------------------------------------------------------

/// `GET` the PR's issue comments (to find an existing sticky comment).
fn comment_list_request(ctx: &PrContext) -> ApiRequest {
    ApiRequest::get(format!(
        "/repos/{}/{}/issues/{}/comments?per_page=100",
        ctx.owner, ctx.repo, ctx.pr
    ))
}

/// `POST` a new issue comment.
fn comment_create_request(ctx: &PrContext, body: &str) -> ApiRequest {
    ApiRequest::post(
        format!(
            "/repos/{}/{}/issues/{}/comments",
            ctx.owner, ctx.repo, ctx.pr
        ),
        serde_json::json!({ "body": body }),
    )
}

/// `PATCH` an existing issue comment in place.
fn comment_update_request(ctx: &PrContext, comment_id: u64, body: &str) -> ApiRequest {
    ApiRequest::patch(
        format!(
            "/repos/{}/{}/issues/comments/{}",
            ctx.owner, ctx.repo, comment_id
        ),
        serde_json::json!({ "body": body }),
    )
}

/// `POST` a completed check run.
fn check_run_request(ctx: &PrContext, check: &CheckRun) -> ApiRequest {
    let annotations: Vec<serde_json::Value> = check
        .annotations
        .iter()
        .map(|a| {
            serde_json::json!({
                "path": a.path,
                "start_line": a.start_line,
                "end_line": a.end_line,
                "annotation_level": a.level,
                "message": a.message,
            })
        })
        .collect();
    ApiRequest::post(
        format!("/repos/{}/{}/check-runs", ctx.owner, ctx.repo),
        serde_json::json!({
            "name": check.name,
            "head_sha": check.head_sha,
            "status": "completed",
            "conclusion": check.conclusion.as_str(),
            "output": {
                "title": check.title,
                "summary": check.summary,
                "annotations": annotations,
            },
        }),
    )
}

// ---------------------------------------------------------------------------
// Orchestration
// ---------------------------------------------------------------------------

/// What the report did to the sticky comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentAction {
    /// Posted a new comment.
    Created,
    /// Updated the existing sticky comment in place.
    Updated(u64),
}

/// A short account of what the report posted, for the CLI to print.
#[derive(Debug, Clone, Copy)]
pub struct ReportSummary {
    /// What happened to the sticky comment.
    pub comment: CommentAction,
    /// How many check runs were created (per reviewer plus the aggregate).
    pub checks: usize,
}

impl fmt::Display for ReportSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let comment = match self.comment {
            CommentAction::Created => "posted a new PR comment".to_string(),
            CommentAction::Updated(id) => format!("updated PR comment {id}"),
        };
        write!(f, "{comment}; created {} check run(s)", self.checks)
    }
}

/// Post a finished run's results to its pull request.
///
/// Upserts the sticky comment, then creates a check run per reviewer plus the
/// aggregate `bastion` check. Any non-2xx response aborts with a legible error.
///
/// # Errors
///
/// Returns an error if a GitHub request fails to send or returns a non-2xx status.
pub async fn report<A: GitHubApi + ?Sized>(
    api: &A,
    ctx: &PrContext,
    events: &[RunEvent],
) -> Result<ReportSummary> {
    let digest = digest(events);

    let body = comment_body(&digest);
    let comment = upsert_comment(api, ctx, &body).await?;

    let checks = check_runs(ctx, &digest);
    for check in &checks {
        send_checked(api, &check_run_request(ctx, check)).await?;
    }

    Ok(ReportSummary {
        comment,
        checks: checks.len(),
    })
}

/// Create the sticky comment, or update it in place if one already exists.
async fn upsert_comment<A: GitHubApi + ?Sized>(
    api: &A,
    ctx: &PrContext,
    body: &str,
) -> Result<CommentAction> {
    let listed = send_checked(api, &comment_list_request(ctx)).await?;
    match find_marker_comment(&listed.body)? {
        Some(id) => {
            send_checked(api, &comment_update_request(ctx, id, body)).await?;
            Ok(CommentAction::Updated(id))
        }
        None => {
            send_checked(api, &comment_create_request(ctx, body)).await?;
            Ok(CommentAction::Created)
        }
    }
}

/// Find the id of Bastion's own sticky comment in a comment-list response, by its
/// hidden [`MARKER`].
///
/// Fails closed on a malformed list body rather than collapsing a parse error into
/// "no existing comment": treating an unexpected response shape as "none found"
/// would post a fresh comment on every run, stacking duplicates. A body Bastion
/// cannot parse is an error to surface, not a silent create.
///
/// # Errors
///
/// Returns an error if the response body is not the expected array of comments.
fn find_marker_comment(list_body: &serde_json::Value) -> Result<Option<u64>> {
    let comments: Vec<IssueComment> = serde_json::from_value(list_body.clone())
        .wrap_err("parsing the PR comment list from GitHub")?;
    Ok(comments
        .into_iter()
        .find(|c| c.body.contains(MARKER))
        .map(|c| c.id))
}

/// Send a request and treat any non-2xx status as an error, surfacing GitHub's
/// own message. The fail-closed posture: a reporting call that GitHub rejected is
/// a real failure, not something to swallow.
async fn send_checked<A: GitHubApi + ?Sized>(api: &A, req: &ApiRequest) -> Result<ApiResponse> {
    let resp = api.send(req).await?;
    if !resp.is_success() {
        bail!(
            "GitHub {} {} returned {}: {}",
            req.method.as_str(),
            req.path,
            resp.status,
            resp.error_message().unwrap_or("(no message)"),
        );
    }
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{ReviewerRef, RunId};
    use crate::github::client::test_support::RecordingClient;
    use crate::github::client::{ApiResponse, Method};

    fn ctx() -> PrContext {
        PrContext {
            owner: "acme".into(),
            repo: "app".into(),
            pr: 12,
            head_sha: "deadbeef".into(),
        }
    }

    fn finding(kind: FindingKind, path: &str, start: u32, end: u32, detail: &str) -> Finding {
        Finding {
            kind,
            path: path.into(),
            line_start: start,
            line_end: end,
            detail: detail.into(),
        }
    }

    /// A representative run: one blocking gate (with a blocking and an optional
    /// finding), one passing gate, one advisor with an optional finding.
    fn sample_events() -> Vec<RunEvent> {
        let run = RunId("r-1".into());
        vec![
            RunEvent::RunStarted {
                run: run.clone(),
                branch: "feat/cart".into(),
                base: "main".into(),
                changed: 3,
                reviewers: vec![
                    ReviewerRef {
                        name: "tenant-isolation".into(),
                        mode: Mode::Gate,
                    },
                    ReviewerRef {
                        name: "file-responsibility".into(),
                        mode: Mode::Gate,
                    },
                    ReviewerRef {
                        name: "style".into(),
                        mode: Mode::Advisor,
                    },
                ],
            },
            RunEvent::ReviewerStarted {
                run: run.clone(),
                reviewer: "tenant-isolation".into(),
                mode: Mode::Gate,
                backend: Backend::Codex,
            },
            RunEvent::ReviewerResolved {
                run: run.clone(),
                reviewer: "tenant-isolation".into(),
                verdict: Decision::Block,
                summary: "A query reads rows without scoping by tenant id.".into(),
                findings: vec![
                    finding(
                        FindingKind::Blocking,
                        "src/db.ts",
                        88,
                        91,
                        "scope by tenant_id",
                    ),
                    finding(
                        FindingKind::Optional,
                        "src/db.ts",
                        10,
                        10,
                        "consider an index",
                    ),
                ],
                usage: Some(Usage {
                    tokens_in: 1820,
                    tokens_out: 156,
                    cost_usd: Money::from_cents(21),
                }),
                duration_ms: 38_000,
                has_transcript: true,
            },
            RunEvent::ReviewerResolved {
                run: run.clone(),
                reviewer: "file-responsibility".into(),
                verdict: Decision::Pass,
                summary: "Responsibilities look well separated.".into(),
                findings: vec![],
                usage: None,
                duration_ms: 12_000,
                has_transcript: true,
            },
            RunEvent::ReviewerResolved {
                run: run.clone(),
                reviewer: "style".into(),
                verdict: Decision::Pass,
                summary: "A couple of nits.".into(),
                findings: vec![finding(
                    FindingKind::Optional,
                    "src/x.ts",
                    4,
                    4,
                    "rename foo",
                )],
                usage: None,
                duration_ms: 5_000,
                has_transcript: true,
            },
            RunEvent::RunCompleted {
                run,
                verdict: Decision::Block,
                gates: Gates {
                    total: 2,
                    passed: 1,
                    blocked: 1,
                },
                duration_ms: 40_000,
                cost_usd: Money::from_cents(21),
            },
        ]
    }

    #[test]
    fn comment_surfaces_every_finding_including_optional() {
        let body = comment_body(&digest(&sample_events()));
        // Marker for in-place upsert, and the headline.
        assert!(body.starts_with(MARKER));
        assert!(body.contains("**Blocked.** 1 of 2 gate(s) passed."));
        // The table lists all three reviewers with their verdict words.
        assert!(body.contains("| `tenant-isolation` | gate | block |"));
        assert!(body.contains("| `style` | advisor | advisory |"));
        // Both a blocking and an optional finding are rendered, with locations...
        assert!(body.contains("- **blocking** `src/db.ts:88-91`: scope by tenant_id"));
        assert!(body.contains("- **optional** `src/db.ts:10`: consider an index"));
        // ...including the advisor's optional finding, which never gates.
        assert!(body.contains("- **optional** `src/x.ts:4`: rename foo"));
        // No em dashes leaked into generated prose.
        assert!(!body.contains('\u{2014}') && !body.contains('\u{2013}'));
    }

    #[test]
    fn comment_handles_zero_reviewers() {
        let events = vec![
            RunEvent::RunStarted {
                run: RunId("r".into()),
                branch: "b".into(),
                base: "main".into(),
                changed: 0,
                reviewers: vec![],
            },
            RunEvent::RunCompleted {
                run: RunId("r".into()),
                verdict: Decision::Pass,
                gates: Gates {
                    total: 0,
                    passed: 0,
                    blocked: 0,
                },
                duration_ms: 0,
                cost_usd: Money::from_cents(0),
            },
        ];
        let body = comment_body(&digest(&events));
        assert!(body.contains("No gates were triggered."));
        assert!(body.contains("No reviewers were triggered"));
    }

    #[test]
    fn comment_cell_escaping_keeps_the_table_intact() {
        // A summary with a pipe and a newline must not break the row.
        let cell = escape_cell("a | b\nc");
        assert_eq!(cell, "a \\| b c");
    }

    #[test]
    fn check_runs_map_gate_and_advisor_conclusions() {
        let checks = check_runs(&ctx(), &digest(&sample_events()));
        // Per reviewer plus the aggregate.
        assert_eq!(checks.len(), 4);

        let by_name = |name: &str| checks.iter().find(|c| c.name == name).unwrap().clone();

        let blocked = by_name("bastion / tenant-isolation");
        assert_eq!(blocked.conclusion, Conclusion::Failure);
        assert!(blocked.title.starts_with("Blocked:"));
        // Its blocking finding becomes a failure annotation; the optional one a warning.
        assert_eq!(blocked.annotations.len(), 2);
        assert_eq!(blocked.annotations[0].level, "failure");
        assert_eq!(blocked.annotations[1].level, "warning");
        assert_eq!(blocked.head_sha, "deadbeef");

        // The advisor, even with a finding, concludes success and never gates.
        let advisor = by_name("bastion / style");
        assert_eq!(advisor.conclusion, Conclusion::Success);
        assert!(advisor.title.starts_with("Advisory:"));

        // The aggregate reflects the blocked run and carries no annotations.
        let aggregate = by_name("bastion");
        assert_eq!(aggregate.conclusion, Conclusion::Failure);
        assert!(aggregate.annotations.is_empty());
        assert!(aggregate.title.contains("1/2"));
    }

    #[test]
    fn aggregate_fails_closed_on_an_incomplete_run() {
        // A stream with no run.completed: the aggregate must not read as a pass.
        let events = vec![RunEvent::RunStarted {
            run: RunId("r".into()),
            branch: "b".into(),
            base: "main".into(),
            changed: 1,
            reviewers: vec![],
        }];
        let checks = check_runs(&ctx(), &digest(&events));
        let aggregate = checks.iter().find(|c| c.name == "bastion").unwrap();
        assert_eq!(aggregate.conclusion, Conclusion::Failure);
    }

    /// Build a one-gate run: it starts `gate_count` gates, resolves `rows` of them,
    /// and records `completed` (verdict + tally). Used to forge inconsistent runs.
    fn forged_run(
        started_gates: &[&str],
        rows: Vec<RunEvent>,
        completed: (Decision, Gates),
    ) -> Vec<RunEvent> {
        let run = RunId("r-forge".into());
        let mut events = vec![RunEvent::RunStarted {
            run: run.clone(),
            branch: "feat".into(),
            base: "main".into(),
            changed: 1,
            reviewers: started_gates
                .iter()
                .map(|name| ReviewerRef {
                    name: (*name).into(),
                    mode: Mode::Gate,
                })
                .collect(),
        }];
        events.extend(rows);
        events.push(RunEvent::RunCompleted {
            run,
            verdict: completed.0,
            gates: completed.1,
            duration_ms: 1000,
            cost_usd: Money::from_cents(0),
        });
        events
    }

    fn gate_resolved(name: &str, verdict: Decision, findings: Vec<Finding>) -> RunEvent {
        RunEvent::ReviewerResolved {
            run: RunId("r-forge".into()),
            reviewer: name.into(),
            verdict,
            summary: format!("{name} summary"),
            findings,
            usage: None,
            duration_ms: 1000,
            has_transcript: true,
        }
    }

    #[test]
    fn clean_pass_with_gates_concludes_success() {
        // A genuine pass: every started gate resolved pass with no blocking finding,
        // and the tally agrees. The aggregate is the one case that may go green.
        let events = forged_run(
            &["g1"],
            vec![gate_resolved("g1", Decision::Pass, vec![])],
            (
                Decision::Pass,
                Gates {
                    total: 1,
                    passed: 1,
                    blocked: 0,
                },
            ),
        );
        let digest = digest(&events);
        assert!(is_clean_pass(&digest));
        let checks = check_runs(&ctx(), &digest);
        assert_eq!(
            checks
                .iter()
                .find(|c| c.name == "bastion")
                .unwrap()
                .conclusion,
            Conclusion::Success
        );
        assert_eq!(
            checks
                .iter()
                .find(|c| c.name == "bastion / g1")
                .unwrap()
                .conclusion,
            Conclusion::Success
        );
    }

    #[test]
    fn aggregate_fails_closed_on_a_pass_with_a_blocking_finding() {
        // The dangerous case: a replayed run records a pass, but a gate row passed
        // while carrying a blocking finding (self-contradictory). The aggregate must
        // not publish success, and the contradictory reviewer check must fail too.
        let events = forged_run(
            &["g1"],
            vec![gate_resolved(
                "g1",
                Decision::Pass,
                vec![finding(FindingKind::Blocking, "src/a.rs", 1, 1, "leak")],
            )],
            (
                Decision::Pass,
                Gates {
                    total: 1,
                    passed: 1,
                    blocked: 0,
                },
            ),
        );
        let digest = digest(&events);
        assert!(!is_clean_pass(&digest));
        let checks = check_runs(&ctx(), &digest);
        let aggregate = checks.iter().find(|c| c.name == "bastion").unwrap();
        assert_eq!(aggregate.conclusion, Conclusion::Failure);
        assert!(aggregate.title.contains("internally inconsistent"));
        assert!(aggregate.summary.contains("Failing the aggregate closed"));
        assert_eq!(
            checks
                .iter()
                .find(|c| c.name == "bastion / g1")
                .unwrap()
                .conclusion,
            Conclusion::Failure
        );
        // The comment headline also fails closed rather than claiming a pass.
        assert!(
            comment_body(&digest)
                .contains("**Blocked.** The recorded run is internally inconsistent")
        );
    }

    #[test]
    fn aggregate_fails_closed_on_a_missing_gate() {
        // The run announced two gates but only one resolved; the recorded pass is not
        // trustworthy because a gate is missing entirely.
        let events = forged_run(
            &["g1", "g2"],
            vec![gate_resolved("g1", Decision::Pass, vec![])],
            (
                Decision::Pass,
                Gates {
                    total: 2,
                    passed: 2,
                    blocked: 0,
                },
            ),
        );
        let digest = digest(&events);
        assert!(!is_clean_pass(&digest));
        let checks = check_runs(&ctx(), &digest);
        assert_eq!(
            checks
                .iter()
                .find(|c| c.name == "bastion")
                .unwrap()
                .conclusion,
            Conclusion::Failure
        );
    }

    #[test]
    fn aggregate_fails_closed_on_a_tally_mismatch() {
        // Every started gate resolved cleanly, but the recorded tally lies about how
        // many gates there were. A tally that disagrees with the rows fails closed.
        let events = forged_run(
            &["g1"],
            vec![gate_resolved("g1", Decision::Pass, vec![])],
            (
                Decision::Pass,
                Gates {
                    total: 2,
                    passed: 2,
                    blocked: 0,
                },
            ),
        );
        let digest = digest(&events);
        assert!(!is_clean_pass(&digest));
        assert_eq!(
            check_runs(&ctx(), &digest)
                .iter()
                .find(|c| c.name == "bastion")
                .unwrap()
                .conclusion,
            Conclusion::Failure
        );
    }

    #[test]
    fn aggregate_fails_closed_on_a_passed_count_mismatch() {
        // The total and blocked fields agree with the single resolved gate, but the
        // recorded `passed` is internally impossible (total=1, blocked=0 implies
        // passed=1, yet the tally claims 0). Validating `passed` against the rows
        // closes this gap, so the aggregate must not go green.
        let events = forged_run(
            &["g1"],
            vec![gate_resolved("g1", Decision::Pass, vec![])],
            (
                Decision::Pass,
                Gates {
                    total: 1,
                    passed: 0,
                    blocked: 0,
                },
            ),
        );
        let digest = digest(&events);
        assert!(!is_clean_pass(&digest));
        assert_eq!(
            check_runs(&ctx(), &digest)
                .iter()
                .find(|c| c.name == "bastion")
                .unwrap()
                .conclusion,
            Conclusion::Failure
        );
    }

    #[test]
    fn trivial_pass_with_a_plan_but_no_reviewers_concludes_success() {
        // The legitimate zero-reviewer run: the plan was announced (one run.started
        // with no reviewers) and completed clean. This must stay a green aggregate;
        // the missing-plan guard below must not mistake it for a malformed stream.
        let events = vec![
            RunEvent::RunStarted {
                run: RunId("r-forge".into()),
                branch: "feat".into(),
                base: "main".into(),
                changed: 1,
                reviewers: vec![],
            },
            RunEvent::RunCompleted {
                run: RunId("r-forge".into()),
                verdict: Decision::Pass,
                gates: Gates {
                    total: 0,
                    passed: 0,
                    blocked: 0,
                },
                duration_ms: 1000,
                cost_usd: Money::from_cents(0),
            },
        ];
        let digest = digest(&events);
        assert!(is_clean_pass(&digest));
        assert_eq!(
            check_runs(&ctx(), &digest)
                .iter()
                .find(|c| c.name == "bastion")
                .unwrap()
                .conclusion,
            Conclusion::Success
        );
    }

    #[test]
    fn aggregate_fails_closed_on_a_missing_run_plan() {
        // A forged stream with only a passing run.completed and a zero-gate tally, but
        // no run.started to announce the reviewer plan. Without the plan, omitted gates
        // cannot be ruled out, so success must not be representable.
        let events = vec![RunEvent::RunCompleted {
            run: RunId("r-forge".into()),
            verdict: Decision::Pass,
            gates: Gates {
                total: 0,
                passed: 0,
                blocked: 0,
            },
            duration_ms: 1000,
            cost_usd: Money::from_cents(0),
        }];
        let digest = digest(&events);
        assert!(!is_clean_pass(&digest));
        assert_eq!(
            check_runs(&ctx(), &digest)
                .iter()
                .find(|c| c.name == "bastion")
                .unwrap()
                .conclusion,
            Conclusion::Failure
        );
    }

    #[test]
    fn aggregate_fails_closed_on_a_reordered_stream() {
        // A resolved gate row that arrives *before* the plan announces it. The
        // per-event tallies line up (one started gate, one resolved gate, a matching
        // pass tally), but the event order is invalid, so the stream is not a
        // trustworthy record and success must not be representable.
        let events = vec![
            gate_resolved("g1", Decision::Pass, vec![]),
            RunEvent::RunStarted {
                run: RunId("r-forge".into()),
                branch: "feat".into(),
                base: "main".into(),
                changed: 1,
                reviewers: vec![ReviewerRef {
                    name: "g1".into(),
                    mode: Mode::Gate,
                }],
            },
            RunEvent::RunCompleted {
                run: RunId("r-forge".into()),
                verdict: Decision::Pass,
                gates: Gates {
                    total: 1,
                    passed: 1,
                    blocked: 0,
                },
                duration_ms: 1000,
                cost_usd: Money::from_cents(0),
            },
        ];
        let digest = digest(&events);
        assert!(digest.malformed);
        assert!(!is_clean_pass(&digest));
        assert_eq!(
            check_runs(&ctx(), &digest)
                .iter()
                .find(|c| c.name == "bastion")
                .unwrap()
                .conclusion,
            Conclusion::Failure
        );
    }

    #[test]
    fn per_reviewer_gate_fails_closed_on_a_truncated_run() {
        // A passing gate row but no run.completed: the run is truncated. The aggregate
        // fails closed, and the per-reviewer gate check must not publish success
        // either, since the run as a whole was never proven complete.
        let events = vec![
            RunEvent::RunStarted {
                run: RunId("r-forge".into()),
                branch: "feat".into(),
                base: "main".into(),
                changed: 1,
                reviewers: vec![ReviewerRef {
                    name: "g1".into(),
                    mode: Mode::Gate,
                }],
            },
            gate_resolved("g1", Decision::Pass, vec![]),
        ];
        let digest = digest(&events);
        let checks = check_runs(&ctx(), &digest);
        assert_eq!(
            checks
                .iter()
                .find(|c| c.name == "bastion / g1")
                .unwrap()
                .conclusion,
            Conclusion::Failure
        );
        assert_eq!(
            checks
                .iter()
                .find(|c| c.name == "bastion")
                .unwrap()
                .conclusion,
            Conclusion::Failure
        );
    }

    #[test]
    fn per_reviewer_gate_fails_closed_on_an_inconsistent_tally() {
        // A clean passing gate row, structurally complete, but the recorded
        // run.completed tally is internally impossible (total=1, blocked=0 implies
        // passed=1, yet it claims 0). The record contradicts itself, so the
        // per-reviewer gate check must fail closed too, not just the aggregate.
        let events = forged_run(
            &["g1"],
            vec![gate_resolved("g1", Decision::Pass, vec![])],
            (
                Decision::Pass,
                Gates {
                    total: 1,
                    passed: 0,
                    blocked: 0,
                },
            ),
        );
        let digest = digest(&events);
        let checks = check_runs(&ctx(), &digest);
        assert_eq!(
            checks
                .iter()
                .find(|c| c.name == "bastion / g1")
                .unwrap()
                .conclusion,
            Conclusion::Failure
        );
        assert_eq!(
            checks
                .iter()
                .find(|c| c.name == "bastion")
                .unwrap()
                .conclusion,
            Conclusion::Failure
        );
    }

    #[test]
    fn advisor_concludes_neutral_not_success_on_an_untrustworthy_run() {
        // An advisor in a truncated run (no run.completed). Advisors never gate, so it
        // must not conclude failure, but it must not publish a green success off an
        // unproven run either: the conclusion is neutral.
        let events = vec![
            RunEvent::RunStarted {
                run: RunId("r-forge".into()),
                branch: "feat".into(),
                base: "main".into(),
                changed: 1,
                reviewers: vec![ReviewerRef {
                    name: "a1".into(),
                    mode: Mode::Advisor,
                }],
            },
            gate_resolved("a1", Decision::Pass, vec![]),
        ];
        let digest = digest(&events);
        assert_eq!(
            check_runs(&ctx(), &digest)
                .iter()
                .find(|c| c.name == "bastion / a1")
                .unwrap()
                .conclusion,
            Conclusion::Neutral
        );
    }

    #[test]
    fn oversized_annotation_message_is_truncated_with_a_pointer() {
        // A finding longer than the per-message cap would 422 the whole report
        // request; the annotation message is truncated and points at the comment,
        // while a short finding passes through unchanged.
        let long = "x".repeat(MAX_ANNOTATION_MESSAGE + 100);
        let big = finding(FindingKind::Optional, "src/a.rs", 1, 1, &long);
        let annotated = annotations_for(std::slice::from_ref(&big));
        assert_eq!(annotated.len(), 1);
        assert!(annotated[0].message.chars().count() <= MAX_ANNOTATION_MESSAGE + 80);
        assert!(
            annotated[0]
                .message
                .contains("(truncated; see the Bastion comment")
        );

        let small = finding(FindingKind::Optional, "src/a.rs", 2, 2, "nit");
        assert_eq!(
            annotations_for(std::slice::from_ref(&small))[0].message,
            "nit"
        );
    }

    #[test]
    fn aggregate_fails_closed_on_a_spliced_run_id() {
        // run.started/run.completed belong to run A, but a passing reviewer.resolved is
        // grafted from run B. The counts and recorded tally line up, but the row does
        // not belong to this run, so the selected run's gate was never proven to
        // resolve: fail closed.
        let events = vec![
            RunEvent::RunStarted {
                run: RunId("r-A".into()),
                branch: "feat".into(),
                base: "main".into(),
                changed: 1,
                reviewers: vec![ReviewerRef {
                    name: "g1".into(),
                    mode: Mode::Gate,
                }],
            },
            RunEvent::ReviewerResolved {
                run: RunId("r-B".into()),
                reviewer: "g1".into(),
                verdict: Decision::Pass,
                summary: "ok".into(),
                findings: vec![],
                usage: None,
                duration_ms: 1000,
                has_transcript: true,
            },
            RunEvent::RunCompleted {
                run: RunId("r-A".into()),
                verdict: Decision::Pass,
                gates: Gates {
                    total: 1,
                    passed: 1,
                    blocked: 0,
                },
                duration_ms: 1000,
                cost_usd: Money::from_cents(0),
            },
        ];
        let digest = digest(&events);
        assert!(digest.malformed);
        assert!(!is_clean_pass(&digest));
        assert_eq!(
            check_runs(&ctx(), &digest)
                .iter()
                .find(|c| c.name == "bastion")
                .unwrap()
                .conclusion,
            Conclusion::Failure
        );
    }

    #[test]
    fn oversized_check_summary_is_capped_with_a_pointer() {
        // A reviewer carrying an enormous finding: the per-reviewer check summary must
        // stay under GitHub's output.summary limit, with a pointer to the comment.
        let huge = "y".repeat(MAX_CHECK_SUMMARY + 5000);
        let events = forged_run(
            &["g1"],
            vec![gate_resolved(
                "g1",
                Decision::Block,
                vec![finding(FindingKind::Blocking, "src/a.rs", 1, 1, &huge)],
            )],
            (
                Decision::Block,
                Gates {
                    total: 1,
                    passed: 0,
                    blocked: 1,
                },
            ),
        );
        let digest = digest(&events);
        let checks = check_runs(&ctx(), &digest);
        let g1 = checks.iter().find(|c| c.name == "bastion / g1").unwrap();
        assert!(g1.summary.chars().count() <= MAX_CHECK_SUMMARY + 80);
        assert!(g1.summary.contains("truncated; see the Bastion comment"));
    }

    #[test]
    fn fails_closed_on_a_block_row_without_a_blocking_finding() {
        // A gate that recorded `block` but carries no blocking finding is self-
        // contradictory (mirrors Verdict::is_consistent). The run is treated as
        // malformed, not as a trustworthy blocked gate.
        let events = forged_run(
            &["g1"],
            vec![gate_resolved("g1", Decision::Block, vec![])],
            (
                Decision::Block,
                Gates {
                    total: 1,
                    passed: 0,
                    blocked: 1,
                },
            ),
        );
        let digest = digest(&events);
        assert!(!is_well_formed_run(&digest));
        assert!(!is_clean_pass(&digest));
        assert_eq!(
            check_runs(&ctx(), &digest)
                .iter()
                .find(|c| c.name == "bastion")
                .unwrap()
                .conclusion,
            Conclusion::Failure
        );
    }

    #[test]
    fn advisor_with_a_pass_and_blocking_finding_fails_closed() {
        // An advisor row that records pass while carrying a blocking finding is self-
        // contradictory. Advisors never gate, but the run is malformed, so the advisor
        // check must conclude neutral rather than a green success.
        let events = vec![
            RunEvent::RunStarted {
                run: RunId("r-forge".into()),
                branch: "feat".into(),
                base: "main".into(),
                changed: 1,
                reviewers: vec![ReviewerRef {
                    name: "a1".into(),
                    mode: Mode::Advisor,
                }],
            },
            RunEvent::ReviewerResolved {
                run: RunId("r-forge".into()),
                reviewer: "a1".into(),
                verdict: Decision::Pass,
                summary: "x".into(),
                findings: vec![finding(FindingKind::Blocking, "src/a.rs", 1, 1, "leak")],
                usage: None,
                duration_ms: 1000,
                has_transcript: true,
            },
            RunEvent::RunCompleted {
                run: RunId("r-forge".into()),
                verdict: Decision::Pass,
                gates: Gates {
                    total: 0,
                    passed: 0,
                    blocked: 0,
                },
                duration_ms: 1000,
                cost_usd: Money::from_cents(0),
            },
        ];
        let digest = digest(&events);
        assert!(!is_well_formed_run(&digest));
        assert_eq!(
            check_runs(&ctx(), &digest)
                .iter()
                .find(|c| c.name == "bastion / a1")
                .unwrap()
                .conclusion,
            Conclusion::Neutral
        );
    }

    #[test]
    fn synthetic_crash_finding_is_not_annotated() {
        // The runner's fail-closed marker has an empty path and line 0; it must be
        // rendered in prose but never sent as an annotation (GitHub rejects line 0).
        let crash = finding(
            FindingKind::Blocking,
            "",
            0,
            0,
            "reviewer failed to complete",
        );
        assert!(!is_locatable(&crash));
        assert!(annotations_for(std::slice::from_ref(&crash)).is_empty());
        assert!(finding_bullet(&crash).contains("- **blocking**: reviewer failed to complete"));
    }

    #[test]
    fn annotations_cap_at_the_limit_and_the_summary_notes_the_overflow() {
        // GitHub accepts at most MAX_ANNOTATIONS annotations per request. With more
        // locatable findings than the cap, annotations_for must stop at the cap and
        // the reviewer-check summary must say how many located findings went unpinned.
        let overflow = 5;
        let findings: Vec<Finding> = (0..MAX_ANNOTATIONS + overflow)
            .map(|i| {
                let line = u32::try_from(i + 1).unwrap();
                finding(FindingKind::Optional, "src/big.rs", line, line, "nit")
            })
            .collect();

        let annotations = annotations_for(&findings);
        assert_eq!(annotations.len(), MAX_ANNOTATIONS);

        let row = ReviewerRow {
            name: "style".into(),
            mode: Mode::Advisor,
            backend: Some(Backend::Codex),
            decision: Decision::Pass,
            summary: "many nits".into(),
            findings,
            duration_ms: 1000,
            usage: None,
        };
        let summary = reviewer_check_summary(&row, &annotations);
        assert!(summary.contains(&format!(
            "{overflow} more located finding(s) are listed above but not pinned to the diff"
        )));
    }

    #[test]
    fn request_builders_target_the_right_endpoints() {
        let ctx = ctx();
        assert_eq!(
            comment_list_request(&ctx).path,
            "/repos/acme/app/issues/12/comments?per_page=100"
        );
        let create = comment_create_request(&ctx, "hi");
        assert_eq!(create.method, Method::Post);
        assert_eq!(create.path, "/repos/acme/app/issues/12/comments");
        assert_eq!(create.body.unwrap()["body"], "hi");

        let update = comment_update_request(&ctx, 7, "ho");
        assert_eq!(update.method, Method::Patch);
        assert_eq!(update.path, "/repos/acme/app/issues/comments/7");

        let check = CheckRun {
            name: "bastion".into(),
            head_sha: "sha".into(),
            conclusion: Conclusion::Failure,
            title: "t".into(),
            summary: "s".into(),
            annotations: vec![],
        };
        let req = check_run_request(&ctx, &check);
        assert_eq!(req.path, "/repos/acme/app/check-runs");
        let body = req.body.unwrap();
        assert_eq!(body["conclusion"], "failure");
        assert_eq!(body["status"], "completed");
        assert_eq!(body["head_sha"], "sha");
    }

    #[test]
    fn find_marker_comment_matches_only_bastions_own() {
        let list = serde_json::json!([
            {"id": 1, "body": "a human comment"},
            {"id": 2, "body": format!("{MARKER}\n## Bastion review")},
        ]);
        assert_eq!(find_marker_comment(&list).unwrap(), Some(2));

        let none = serde_json::json!([{"id": 1, "body": "no marker here"}]);
        assert_eq!(find_marker_comment(&none).unwrap(), None);

        // A malformed body (not the expected array) fails closed rather than
        // reporting "none found", which would post a duplicate comment.
        let malformed = serde_json::json!({"message": "Not Found"});
        assert!(find_marker_comment(&malformed).is_err());
    }

    #[tokio::test]
    async fn report_creates_a_comment_then_posts_checks() {
        // No existing comment: the list returns empty, so the report POSTs a new one.
        let api = RecordingClient::with_responder(|req| {
            if req.method == Method::Get {
                ApiResponse {
                    status: 200,
                    body: serde_json::json!([]),
                }
            } else {
                ApiResponse {
                    status: 201,
                    body: serde_json::json!({"id": 555}),
                }
            }
        });
        let summary = report(&api, &ctx(), &sample_events())
            .await
            .expect("reports");
        assert_eq!(summary.comment, CommentAction::Created);
        assert_eq!(summary.checks, 4);

        let calls = api.calls();
        // GET list, POST comment, then 4 POST check-runs.
        assert_eq!(calls[0].method, Method::Get);
        assert_eq!(calls[1].method, Method::Post);
        assert!(calls[1].path.ends_with("/issues/12/comments"));
        let check_calls = calls
            .iter()
            .filter(|c| c.path.ends_with("/check-runs"))
            .count();
        assert_eq!(check_calls, 4);
        // The created comment body carries the marker and the optional finding.
        let body = calls[1].body.as_ref().unwrap()["body"].as_str().unwrap();
        assert!(body.contains(MARKER));
        assert!(body.contains("rename foo"));
    }

    #[tokio::test]
    async fn report_updates_an_existing_comment_in_place() {
        // The list returns Bastion's own comment, so the report PATCHes it.
        let api = RecordingClient::with_responder(|req| match req.method {
            Method::Get => ApiResponse {
                status: 200,
                body: serde_json::json!([{"id": 909, "body": format!("{MARKER} old")}]),
            },
            _ => ApiResponse {
                status: 200,
                body: serde_json::Value::Null,
            },
        });
        let summary = report(&api, &ctx(), &sample_events())
            .await
            .expect("reports");
        assert_eq!(summary.comment, CommentAction::Updated(909));

        let calls = api.calls();
        // The second call is a PATCH to the existing comment, not a POST.
        assert_eq!(calls[1].method, Method::Patch);
        assert!(calls[1].path.ends_with("/issues/comments/909"));
    }

    #[tokio::test]
    async fn report_fails_closed_on_a_rejected_request() {
        // GitHub rejects the comment list: the report errors rather than pressing on.
        let api = RecordingClient::with_responder(|_| ApiResponse {
            status: 403,
            body: serde_json::json!({"message": "Resource not accessible by integration"}),
        });
        let err = report(&api, &ctx(), &sample_events()).await.unwrap_err();
        assert!(err.to_string().contains("returned 403"));
        assert!(err.to_string().contains("Resource not accessible"));
    }

    #[test]
    fn truncate_caps_and_marks_overflow() {
        assert_eq!(truncate("short", 110), "short");
        let long = "x".repeat(200);
        let cut = truncate(&long, 110);
        assert_eq!(cut.chars().count(), 110);
        assert!(cut.ends_with("..."));
    }
}

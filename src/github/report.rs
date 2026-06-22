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

use color_eyre::eyre::{Result, bail};

use crate::event::{Gates, RunEvent};
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
    let mut modes: Vec<(String, Mode)> = Vec::new();
    let mut backends: Vec<(String, Backend)> = Vec::new();

    for event in events {
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
                modes = reviewers.iter().map(|r| (r.name.clone(), r.mode)).collect();
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
                let mode = modes
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
                digest.aggregate = Some(*verdict);
                digest.gates = Some(*gates);
                digest.duration_ms = Some(*duration_ms);
                digest.cost = Some(*cost_usd);
            }
        }
    }
    digest
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

    let headline = match digest.aggregate {
        Some(Decision::Block) => {
            format!("**Blocked.** {} of {total} gate(s) passed.", passed)
        }
        Some(Decision::Pass) if total == 0 => "**Passed.** No gates were triggered.".to_string(),
        Some(Decision::Pass) => format!("**Passed.** All {total} gate(s) passed."),
        None => "**Incomplete.** The run did not finish.".to_string(),
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
    let mut checks: Vec<CheckRun> = digest
        .rows
        .iter()
        .map(|row| reviewer_check(ctx, row))
        .collect();
    checks.push(aggregate_check(ctx, digest));
    checks
}

/// The check run for one reviewer.
fn reviewer_check(ctx: &PrContext, row: &ReviewerRow) -> CheckRun {
    let conclusion = match row.mode {
        // Advisors comment but never gate, so they always conclude success.
        Mode::Advisor => Conclusion::Success,
        Mode::Gate => match row.decision {
            Decision::Pass => Conclusion::Success,
            Decision::Block => Conclusion::Failure,
        },
    };

    let decision_word = match (row.mode, row.decision) {
        (Mode::Advisor, _) => "Advisory",
        (Mode::Gate, Decision::Pass) => "Passed",
        (Mode::Gate, Decision::Block) => "Blocked",
    };
    let title = format!("{decision_word}: {}", truncate(&row.summary, 110));

    let annotations = annotations_for(&row.findings);
    let summary = reviewer_check_summary(row, &annotations);

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
    // Fail closed if the run never completed: a missing aggregate is treated as a
    // block, never a silent pass.
    let conclusion = match digest.aggregate {
        Some(Decision::Pass) => Conclusion::Success,
        Some(Decision::Block) | None => Conclusion::Failure,
    };
    let (passed, total) = digest.gates.map_or((0, 0), |g| (g.passed, g.total));
    let title = match digest.aggregate {
        Some(Decision::Pass) if total == 0 => "No gates triggered".to_string(),
        Some(Decision::Pass) => format!("{passed}/{total} gates passed"),
        Some(Decision::Block) => format!("Blocked: {}/{total} gates passed", passed),
        None => "Incomplete run".to_string(),
    };

    let mut summary = String::new();
    summary.push_str(&status_line(digest));
    summary.push_str("\n\n");
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
        summary,
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

/// Map a reviewer's locatable findings to check annotations, capped at
/// [`MAX_ANNOTATIONS`].
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
            message: f.detail.clone(),
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
    match find_marker_comment(&listed.body) {
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
fn find_marker_comment(list_body: &serde_json::Value) -> Option<u64> {
    let comments: Vec<IssueComment> = serde_json::from_value(list_body.clone()).ok()?;
    comments
        .into_iter()
        .find(|c| c.body.contains(MARKER))
        .map(|c| c.id)
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
        assert_eq!(find_marker_comment(&list), Some(2));

        let none = serde_json::json!([{"id": 1, "body": "no marker here"}]);
        assert_eq!(find_marker_comment(&none), None);
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

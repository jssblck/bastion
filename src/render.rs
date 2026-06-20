//! Turning run events and summaries into output.
//!
//! Two audiences, two formats. [`Format::Human`] renders readable progress for a
//! person watching; [`Format::Jsonl`] emits one JSON object per line for an agent
//! to parse as it arrives. The JSONL form is exactly the persisted event shape,
//! so on-screen output and `run.jsonl` never disagree.

use std::io::{self, Write};

use crate::event::RunEvent;
use crate::store::RunSummary;
use crate::verdict::{Decision, Finding, FindingKind};

/// The output format for streamed and replayed run data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
#[clap(rename_all = "lowercase")]
pub enum Format {
    /// Human-readable progress.
    #[default]
    Human,
    /// One JSON object per line.
    Jsonl,
}

/// Write a single run event in the chosen format.
///
/// # Errors
///
/// Returns an error if writing to `out` fails or the event cannot be serialized.
pub fn write_event<W: Write>(out: &mut W, format: Format, event: &RunEvent) -> io::Result<()> {
    match format {
        Format::Jsonl => writeln!(
            out,
            "{}",
            serde_json::to_string(event).map_err(io::Error::other)?
        ),
        Format::Human => write_event_human(out, event),
    }
}

fn write_event_human<W: Write>(out: &mut W, event: &RunEvent) -> io::Result<()> {
    match event {
        RunEvent::RunStarted {
            run,
            branch,
            base,
            changed,
            reviewers,
        } => writeln!(
            out,
            "run {run}: {branch} vs {base} — {changed} file(s) changed, {} reviewer(s) triggered",
            reviewers.len()
        ),
        RunEvent::ReviewerStarted {
            reviewer,
            mode,
            backend,
            ..
        } => {
            writeln!(
                out,
                "  .. {reviewer} ({}, {})",
                mode.as_str(),
                backend.as_str()
            )
        }
        RunEvent::ReviewerResolved {
            reviewer,
            verdict,
            summary,
            findings,
            duration_ms,
            ..
        } => {
            writeln!(
                out,
                "  {} {reviewer}: {summary} ({}s)",
                marker(*verdict),
                duration_ms / 1000
            )?;
            for finding in findings {
                write_finding(out, finding)?;
            }
            Ok(())
        }
        RunEvent::RunCompleted {
            verdict,
            gates,
            duration_ms,
            cost_usd,
            ..
        } => writeln!(
            out,
            "{} run complete — {}/{} gates passed ({}s, {cost_usd})",
            marker(*verdict),
            gates.passed,
            gates.total,
            duration_ms / 1000
        ),
    }
}

fn write_finding<W: Write>(out: &mut W, finding: &Finding) -> io::Result<()> {
    let tag = match finding.kind {
        FindingKind::Blocking => "blocking",
        FindingKind::Optional => "optional",
    };
    writeln!(
        out,
        "      [{tag}] {}:{}-{}: {}",
        finding.path, finding.line_start, finding.line_end, finding.detail
    )
}

/// Write a list of run summaries in the chosen format.
///
/// # Errors
///
/// Returns an error if writing to `out` fails.
pub fn write_runs<W: Write>(out: &mut W, format: Format, runs: &[RunSummary]) -> io::Result<()> {
    match format {
        Format::Jsonl => {
            for run in runs {
                writeln!(
                    out,
                    "{}",
                    serde_json::to_string(run).map_err(io::Error::other)?
                )?;
            }
            Ok(())
        }
        Format::Human => {
            if runs.is_empty() {
                return writeln!(out, "no runs recorded");
            }
            for run in runs {
                let verdict = run.verdict.map_or("(incomplete)", Decision::as_str);
                let branch = run.branch.as_deref().unwrap_or("(unknown)");
                writeln!(
                    out,
                    "{}  {verdict:<5}  {branch}  {} reviewer(s)",
                    run.run, run.reviewers
                )?;
            }
            Ok(())
        }
    }
}

fn marker(decision: Decision) -> &'static str {
    match decision {
        Decision::Pass => "PASS ",
        Decision::Block => "BLOCK",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::RunId;

    fn resolved() -> RunEvent {
        RunEvent::ReviewerResolved {
            run: RunId("r-1".into()),
            reviewer: "tenant-isolation".into(),
            verdict: Decision::Block,
            summary: "missing tenant scope".into(),
            findings: vec![Finding {
                kind: FindingKind::Blocking,
                path: "src/db.ts".into(),
                line_start: 10,
                line_end: 10,
                detail: "scope by tenant_id".into(),
            }],
            usage: None,
            duration_ms: 4200,
            has_transcript: true,
        }
    }

    #[test]
    fn human_format_is_readable() {
        let mut buf = Vec::new();
        write_event(&mut buf, Format::Human, &resolved()).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(text.contains("BLOCK tenant-isolation: missing tenant scope"));
        assert!(text.contains("[blocking] src/db.ts:10-10: scope by tenant_id"));
    }

    #[test]
    fn jsonl_format_is_the_event_shape() {
        let mut buf = Vec::new();
        write_event(&mut buf, Format::Jsonl, &resolved()).unwrap();
        let line = String::from_utf8(buf).unwrap();
        assert!(line.contains(r#""type":"reviewer.resolved""#));
        let parsed: RunEvent = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(parsed, resolved());
    }
}

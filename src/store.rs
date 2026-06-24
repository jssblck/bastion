//! Reading, writing, and pruning persisted runs under the [`Layout`].
//!
//! The run is always persisted as JSONL regardless of how it was displayed, so a
//! run can be replayed or inspected after the fact. These functions are the
//! local equivalent of the GitHub run-summary page: they read back what a past
//! run recorded without re-running it.

use std::time::{Duration, SystemTime};

use color_eyre::eyre::{Context, Result, bail, eyre};
use serde::{Deserialize, Serialize};

use crate::event::{RunEvent, RunId};
use crate::paths::Layout;
use crate::verdict::Decision;

/// A one-line description of a persisted run, for `bastion runs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunSummary {
    /// The run id.
    pub run: RunId,
    /// The branch under review, if recorded.
    pub branch: Option<String>,
    /// The base branch, if recorded.
    pub base: Option<String>,
    /// The aggregate decision, if the run completed.
    pub verdict: Option<Decision>,
    /// Number of reviewers the run triggered.
    pub reviewers: u32,
}

/// Persist a run's full event stream and update the `latest` pointer.
///
/// # Errors
///
/// Returns an error if the data directory cannot be created or written.
pub fn write_run(layout: &Layout, id: &RunId, events: &[RunEvent]) -> Result<()> {
    let dir = layout.run_dir(id);
    std::fs::create_dir_all(&dir)
        .wrap_err_with(|| format!("creating run directory {}", dir.display()))?;

    let mut body = String::new();
    for event in events {
        body.push_str(&serde_json::to_string(event).wrap_err("serializing run event")?);
        body.push('\n');
    }
    let jsonl = layout.run_jsonl(id);
    std::fs::write(&jsonl, body).wrap_err_with(|| format!("writing {}", jsonl.display()))?;

    std::fs::write(layout.latest_pointer(), id.as_str()).wrap_err("updating latest run pointer")?;
    Ok(())
}

/// Read a run's full event stream.
///
/// # Errors
///
/// Returns an error if the run does not exist or its `run.jsonl` is malformed.
pub fn read_run(layout: &Layout, id: &RunId) -> Result<Vec<RunEvent>> {
    let jsonl = layout.run_jsonl(id);
    let text = std::fs::read_to_string(&jsonl)
        .wrap_err_with(|| format!("no such run '{id}' (expected {})", jsonl.display()))?;
    let mut events = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let event = serde_json::from_str(line)
            .wrap_err_with(|| format!("{}:{}: malformed run event", jsonl.display(), i + 1))?;
        events.push(event);
    }
    Ok(events)
}

/// Resolve an optional run id to a concrete one, defaulting to the latest run.
///
/// # Errors
///
/// Returns an error if no id is given and there is no recorded latest run, or if
/// the named run does not exist.
pub fn resolve_run(layout: &Layout, id: Option<&str>) -> Result<RunId> {
    let run = match id {
        Some(explicit) => RunId(explicit.to_string()),
        None => {
            let pointer = layout.latest_pointer();
            let latest = std::fs::read_to_string(&pointer)
                .map_err(|_| eyre!("no runs recorded yet; run `bastion review` first"))?;
            RunId(latest.trim().to_string())
        }
    };
    if !layout.run_dir(&run).is_dir() {
        bail!("no such run '{run}'");
    }
    Ok(run)
}

/// List recorded runs, most recent first (by directory modification time).
///
/// # Errors
///
/// Returns an error if the runs directory cannot be read. A missing runs
/// directory is treated as an empty list.
pub fn list_runs(layout: &Layout) -> Result<Vec<RunSummary>> {
    let mut runs = collect_runs(layout)?;
    runs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.cmp(&a.0)));
    Ok(runs
        .into_iter()
        .map(|(id, _)| summarize(layout, &id))
        .collect())
}

/// Prune persisted runs, keeping the `keep` most recent and/or removing any
/// older than `older_than`. Returns the ids that were removed.
///
/// # Errors
///
/// Returns an error if a run directory cannot be removed.
pub fn prune(
    layout: &Layout,
    keep: Option<usize>,
    older_than: Option<Duration>,
) -> Result<Vec<RunId>> {
    let mut runs = collect_runs(layout)?;
    runs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.cmp(&a.0)));

    let now = SystemTime::now();
    let mut removed = Vec::new();
    for (index, (id, modified)) in runs.iter().enumerate() {
        let beyond_keep = keep.is_some_and(|k| index >= k);
        let too_old = older_than.is_some_and(|max_age| {
            now.duration_since(*modified)
                .map(|age| age > max_age)
                .unwrap_or(false)
        });
        if beyond_keep || too_old {
            let dir = layout.run_dir(id);
            std::fs::remove_dir_all(&dir)
                .wrap_err_with(|| format!("removing run {}", dir.display()))?;
            removed.push(id.clone());
        }
    }
    Ok(removed)
}

/// Gather `(RunId, modified-time)` for every run directory.
fn collect_runs(layout: &Layout) -> Result<Vec<(RunId, SystemTime)>> {
    let runs_dir = layout.runs_dir();
    let entries = match std::fs::read_dir(&runs_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).wrap_err_with(|| format!("reading {}", runs_dir.display())),
    };

    let mut runs = Vec::new();
    for entry in entries {
        let entry = entry.wrap_err("reading runs directory entry")?;
        let meta = entry.metadata().wrap_err("reading run metadata")?;
        if !meta.is_dir() {
            continue; // skips the `latest` pointer file
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        runs.push((RunId(name), modified));
    }
    Ok(runs)
}

/// Build a [`RunSummary`] from a run's recorded events.
///
/// A run whose `run.jsonl` is missing or malformed degrades to a summary with
/// only its id, rather than failing the whole listing.
fn summarize(layout: &Layout, id: &RunId) -> RunSummary {
    let events = read_run(layout, id).unwrap_or_default();
    let mut summary = RunSummary {
        run: id.clone(),
        branch: None,
        base: None,
        verdict: None,
        reviewers: 0,
    };
    for event in events {
        match event {
            RunEvent::RunStarted {
                branch,
                base,
                reviewers,
                ..
            } => {
                summary.branch = Some(branch);
                summary.base = Some(base);
                summary.reviewers = u32::try_from(reviewers.len()).unwrap_or(u32::MAX);
            }
            RunEvent::RunCompleted { verdict, .. } => summary.verdict = Some(verdict),
            _ => {}
        }
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Gates, ReviewerRef};
    use crate::reviewer::Mode;
    use crate::verdict::Money;

    fn sample_events(id: &str) -> Vec<RunEvent> {
        vec![
            RunEvent::RunStarted {
                run: RunId(id.into()),
                branch: "feat/x".into(),
                base: "main".into(),
                changed: 3,
                reviewers: vec![ReviewerRef {
                    name: "r1".into(),
                    mode: Mode::Gate,
                }],
            },
            RunEvent::RunCompleted {
                run: RunId(id.into()),
                verdict: Decision::Pass,
                gates: Gates {
                    total: 1,
                    passed: 1,
                    blocked: 0,
                },
                duration_ms: 100,
                tokens_in: 0,
                tokens_out: 0,
                cache_read: 0,
                cost_usd: Money::from_cents(5),
            },
        ]
    }

    #[test]
    fn writes_reads_and_summarizes_a_run() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = Layout::with_root(tmp.path().to_path_buf());
        let id = RunId("r-0001".into());

        write_run(&layout, &id, &sample_events("r-0001")).unwrap();

        let events = read_run(&layout, &id).unwrap();
        assert_eq!(events.len(), 2);

        let resolved = resolve_run(&layout, None).unwrap();
        assert_eq!(resolved, id);

        let summaries = list_runs(&layout).unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].branch.as_deref(), Some("feat/x"));
        assert_eq!(summaries[0].verdict, Some(Decision::Pass));
        assert_eq!(summaries[0].reviewers, 1);
    }

    #[test]
    fn prune_keeps_the_most_recent_n() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = Layout::with_root(tmp.path().to_path_buf());
        for id in ["r-0001", "r-0002", "r-0003"] {
            write_run(&layout, &RunId(id.into()), &sample_events(id)).unwrap();
        }
        let removed = prune(&layout, Some(2), None).unwrap();
        assert_eq!(removed.len(), 1);
        assert_eq!(list_runs(&layout).unwrap().len(), 2);
    }

    #[test]
    fn prune_older_than_zero_removes_everything() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = Layout::with_root(tmp.path().to_path_buf());
        write_run(&layout, &RunId("r-0001".into()), &sample_events("r-0001")).unwrap();
        let removed = prune(&layout, None, Some(Duration::from_secs(0))).unwrap();
        assert_eq!(removed.len(), 1);
        assert!(list_runs(&layout).unwrap().is_empty());
    }

    #[test]
    fn resolve_run_errors_when_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = Layout::with_root(tmp.path().to_path_buf());
        assert!(resolve_run(&layout, None).is_err());
    }
}

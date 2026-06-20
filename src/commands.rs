//! Command handlers.
//!
//! Each function implements one CLI subcommand. The read-back commands
//! (`transcript`, `show`, `runs`, `clean`) are fully functional over saved runs;
//! `review` does real config discovery, git-based change detection, and routing,
//! then hands off to the (stubbed) [`crate::runner`] to actually execute
//! reviewers. `codeowners` is pure generation.

use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

use color_eyre::eyre::{Context, Result};

use crate::config::{self, Config};
use crate::event::{ReviewerRef, RunEvent, RunId};
use crate::git;
use crate::paths::Layout;
use crate::render::{self, Format};
use crate::routing::Router;
use crate::runner::{self, ExecContext};
use crate::store;
use crate::verdict::{Decision, Money};

/// `bastion review`: route and run the triggered reviewers, gating the result.
///
/// Computes the changed files against `base`, selects matching reviewers, and
/// emits a `run.started` plan. With zero matches the run is a trivial pass
/// (mirroring the always-present `bastion` check in CI). Otherwise it hands off
/// to the runner, which is not yet implemented and fails closed.
///
/// `cwd` is the directory to resolve the repository and config from — the process
/// working directory in normal use, but explicit so the handler is testable.
///
/// # Errors
///
/// Returns an error if the repository, config, or git queries fail, or — in this
/// build — when any reviewer would need to execute.
pub async fn review(layout: &Layout, cwd: &Path, base: &str, format: Format) -> Result<()> {
    let repo_root = git::repo_root(cwd)?;
    let branch = git::current_branch(&repo_root)?;
    let config = Config::discover(&repo_root)?;
    let changed = git::changed_files(&repo_root, base)?;

    let router = Router::compile(&config.reviewers)?;
    let matched = router.matched(&changed);
    let run = local_run_id(&repo_root);

    let stdout = io::stdout();
    let mut out = stdout.lock();

    let started = RunEvent::RunStarted {
        run: run.clone(),
        branch: branch.clone(),
        base: base.to_string(),
        changed: u32::try_from(changed.len()).unwrap_or(u32::MAX),
        reviewers: matched
            .iter()
            .map(|r| ReviewerRef {
                name: r.name.clone(),
                mode: r.mode,
            })
            .collect(),
    };
    render::write_event(&mut out, format, &started)?;

    if matched.is_empty() {
        // No reviewer triggered: a trivial, honest pass. Persist it so the run is
        // inspectable afterward, exactly like an executed run.
        let completed = RunEvent::RunCompleted {
            run: run.clone(),
            verdict: Decision::Pass,
            gates: crate::event::Gates {
                total: 0,
                passed: 0,
                blocked: 0,
            },
            duration_ms: 0,
            cost_usd: Money::from_cents(0),
        };
        render::write_event(&mut out, format, &completed)?;
        store::write_run(layout, &run, &[started, completed])?;
        return Ok(());
    }

    let ctx = ExecContext {
        run,
        repo_root,
        branch,
        base: base.to_string(),
    };
    runner::execute(&matched, &ctx).await
}

/// `bastion transcript [<run>] <reviewer>`: print a saved session transcript.
///
/// # Errors
///
/// Returns an error if the run or transcript does not exist.
pub fn transcript(layout: &Layout, run: Option<&str>, reviewer: &str) -> Result<()> {
    let run = store::resolve_run(layout, run)?;
    let path = layout.transcript(&run, reviewer);
    let text = std::fs::read_to_string(&path).wrap_err_with(|| {
        format!(
            "no saved transcript for reviewer '{reviewer}' in run '{run}' ({})",
            path.display()
        )
    })?;
    io::stdout()
        .write_all(text.as_bytes())
        .wrap_err("writing transcript")?;
    Ok(())
}

/// `bastion show [<run>]`: re-emit a past run's verdicts and findings.
///
/// # Errors
///
/// Returns an error if the run does not exist or cannot be read.
pub fn show(layout: &Layout, run: Option<&str>, format: Format) -> Result<()> {
    let run = store::resolve_run(layout, run)?;
    let events = store::read_run(layout, &run)?;

    let stdout = io::stdout();
    let mut out = stdout.lock();
    for event in &events {
        if matches!(
            event,
            RunEvent::ReviewerResolved { .. } | RunEvent::RunCompleted { .. }
        ) {
            render::write_event(&mut out, format, event)?;
        }
    }
    Ok(())
}

/// `bastion runs`: list recorded runs.
///
/// # Errors
///
/// Returns an error if the runs directory cannot be read.
pub fn runs(layout: &Layout, format: Format) -> Result<()> {
    let runs = store::list_runs(layout)?;
    let stdout = io::stdout();
    let mut out = stdout.lock();
    render::write_runs(&mut out, format, &runs).wrap_err("rendering runs")?;
    Ok(())
}

/// `bastion clean`: prune saved runs.
///
/// # Errors
///
/// Returns an error if a run cannot be removed.
pub fn clean(layout: &Layout, keep: Option<usize>, older_than: Option<Duration>) -> Result<()> {
    let keep = if keep.is_none() && older_than.is_none() {
        Some(default_keep())
    } else {
        keep
    };
    let removed = store::prune(layout, keep, older_than)?;
    println!("removed {} run(s)", removed.len());
    for id in &removed {
        println!("  {id}");
    }
    Ok(())
}

/// `bastion github codeowners`: print a CODEOWNERS block for the reviewer-policy paths.
///
/// Covers the reviewer registry, the Bastion workflow, and the CODEOWNERS file
/// itself, so any PR touching review policy requires a human review.
///
/// # Errors
///
/// Returns an error if writing to stdout fails.
pub fn codeowners(owners: &[String]) -> Result<()> {
    print!("{}", codeowners_block(owners));
    Ok(())
}

/// Render the CODEOWNERS block assigning `owners` to the reviewer-policy paths.
fn codeowners_block(owners: &[String]) -> String {
    let owners = owners.join(" ");
    format!(
        "# Bastion reviewer policy — changes here require human review.\n\
         # Generated by `bastion github codeowners`; commit this into .github/CODEOWNERS (or your CODEOWNERS).\n\
         /{config_dir}/ {owners}\n\
         /.github/workflows/bastion.yml {owners}\n\
         /CODEOWNERS {owners}\n\
         /.github/CODEOWNERS {owners}\n",
        config_dir = config::CONFIG_DIR,
    )
}

/// How many runs to keep when `bastion clean` is given no arguments.
fn default_keep() -> usize {
    20
}

/// Build a run id for a local run from the short HEAD sha, falling back to a
/// fixed local marker when git can't supply one.
fn local_run_id(repo_root: &std::path::Path) -> RunId {
    let short = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(repo_root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    match short {
        Some(sha) => RunId(format!("r-{sha}")),
        None => RunId("r-local".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codeowners_block_covers_policy_paths() {
        let block = codeowners_block(&["@acme/platform".into(), "@jess".into()]);
        assert!(block.contains("/bastion/ @acme/platform @jess"));
        assert!(block.contains("/.github/workflows/bastion.yml @acme/platform @jess"));
        assert!(block.contains("/CODEOWNERS @acme/platform @jess"));
        assert!(block.contains("require human review"));
    }

    /// Run `git` with deterministic identity/config in `dir`.
    fn git(dir: &Path, args: &[&str]) {
        let isolate = [
            "-c",
            "user.email=t@bastion.dev",
            "-c",
            "user.name=T",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ];
        let status = std::process::Command::new("git")
            .args(isolate)
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    #[tokio::test]
    async fn review_with_no_matching_reviewers_is_a_persisted_pass() {
        let repo = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let dir = repo.path();

        // A registry whose reviewers only trigger on docs, committed so it is not
        // itself a pending change.
        std::fs::create_dir_all(dir.join("bastion")).unwrap();
        std::fs::write(
            dir.join("bastion/reviewers.yaml"),
            "reviewers:\n  - name: docs-only\n    trigger: [docs/**]\n    mode: gate\n    prompt: p\n",
        )
        .unwrap();
        git(dir, &["init"]);
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", "base"]);

        // Change a source file that no reviewer triggers on.
        std::fs::write(dir.join("main.rs"), "fn main() {}\n").unwrap();

        let layout = Layout::with_root(data.path().to_path_buf());
        review(&layout, dir, "main", Format::Jsonl)
            .await
            .expect("zero-match review passes");

        // The pass was persisted and is now inspectable.
        let runs = store::list_runs(&layout).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].verdict, Some(Decision::Pass));
        assert_eq!(runs[0].reviewers, 0);
    }
}

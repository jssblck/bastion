//! Command handlers.
//!
//! Each function implements one CLI subcommand. The read-back commands
//! (`transcript`, `show`, `runs`, `clean`) are fully functional over saved runs;
//! `review` does real config discovery, git-based change detection, and routing,
//! then hands off to the [`crate::runner`] to execute the matched reviewers. The
//! runner owns event emission and persistence; this handler renders the stream and
//! reports the aggregate decision so the CLI can set the exit status.
//! `codeowners` is pure generation.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use color_eyre::eyre::{Context, Result};

use crate::config::{self, Config};
use crate::event::{ReviewerRef, RunEvent, RunId};
use crate::git;
use crate::paths::Layout;
use crate::render::{self, Format};
use crate::routing::Router;
use crate::runner::{self, ExecContext};
use crate::skills;
use crate::store;
use crate::verdict::{Decision, Money};

/// `bastion review`: route and run the triggered reviewers, gating the result.
///
/// Computes the changed files against `base`, selects matching reviewers, emits a
/// `run.started` plan, and hands off to the runner to execute them concurrently.
/// With zero matches the run is a trivial pass (mirroring the always-present
/// `bastion` check in CI). Returns the aggregate [`Decision`] so the caller can
/// map `block` to a non-zero exit status.
///
/// The runner owns event emission for the per-reviewer and completion events and
/// persists the full run; this handler renders the `run.started` event and the
/// events the runner streams back.
///
/// `cwd` is the directory to resolve the repository and config from — the process
/// working directory in normal use, but explicit so the handler is testable.
///
/// # Errors
///
/// Returns an error if the repository, config, git queries, or persistence fail.
/// A blocked review is *not* an error: it returns `Ok(Decision::Block)`.
pub async fn review(layout: &Layout, cwd: &Path, base: &str, format: Format) -> Result<Decision> {
    let repo_root = git::repo_root(cwd)?;
    let branch = git::current_branch(&repo_root)?;
    let config = Config::discover(&repo_root)?;
    let changed = git::changed_files(&repo_root, base)?;

    let router = Router::compile(&config.reviewers)?;
    let matched = router.matched(&changed);
    let run = local_run_id(&repo_root);
    let reviewer_refs: Vec<ReviewerRef> = matched
        .iter()
        .map(|r| ReviewerRef {
            name: r.name.clone(),
            mode: r.mode,
        })
        .collect();
    let changed_count = u32::try_from(changed.len()).unwrap_or(u32::MAX);

    let stdout = io::stdout();
    let mut out = stdout.lock();

    let started = RunEvent::RunStarted {
        run: run.clone(),
        branch: branch.clone(),
        base: base.to_string(),
        changed: changed_count,
        reviewers: reviewer_refs.clone(),
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
        return Ok(Decision::Pass);
    }

    let ctx = ExecContext {
        run,
        repo_root,
        branch,
        base: base.to_string(),
        changed: changed_count,
        reviewers: reviewer_refs,
    };

    // The runner streams the per-reviewer and completion events; render each as it
    // lands. Rendering failures must not be swallowed, so capture the first.
    let mut render_err: Option<io::Error> = None;
    let aggregate = {
        let out = &mut out;
        runner::execute(&matched, &ctx, layout, &mut |event| {
            if render_err.is_none()
                && let Err(err) = render::write_event(out, format, event)
            {
                render_err = Some(err);
            }
        })
        .await?
    };
    if let Some(err) = render_err {
        return Err(err).wrap_err("rendering run events");
    }

    Ok(aggregate)
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

/// `bastion skills install`: write the bundled agent skills into the repository.
///
/// Resolves the repository root from `cwd`, writes each bundled skill into every
/// target directory (the defaults, or the `--dir` overrides), and prints what it
/// did. Existing files that differ are left untouched unless `force` is set, so a
/// local edit is never clobbered silently.
///
/// # Errors
///
/// Returns an error if a skill directory cannot be created or a file cannot be
/// read or written, or if writing the summary to stdout fails.
pub fn skills_install(cwd: &Path, dirs: &[PathBuf], force: bool) -> Result<()> {
    let root = skills_root(cwd);
    let targets = resolve_skill_dirs(dirs);
    let outcomes = skills::install(&root, &targets, force)?;

    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut skipped = 0usize;
    for outcome in &outcomes {
        let label = match outcome.status {
            skills::Installed::Created => "created",
            skills::Installed::Updated => "updated",
            skills::Installed::Unchanged => "unchanged",
            skills::Installed::Skipped => {
                skipped += 1;
                "skipped (exists)"
            }
        };
        writeln!(out, "  {label}: {}", relative_to(&root, &outcome.path))?;
    }
    if skipped > 0 {
        writeln!(
            out,
            "\n{skipped} file(s) already existed and were left as-is; re-run with --force to overwrite."
        )?;
    } else {
        writeln!(
            out,
            "\nCommit these files so your agents discover them on checkout."
        )?;
    }
    Ok(())
}

/// `bastion skills check`: verify the installed skills match this binary's
/// embedded source.
///
/// Prints one line per skill file and returns whether every one is up to date.
/// Returns `Ok(false)` when any file is missing or has drifted (a hand edit, or a
/// stale install left behind after the skill source changed), so the caller can
/// exit non-zero: a CI step can run this to fail when the checked-in skills fall
/// out of sync with the source.
///
/// # Errors
///
/// Returns an error if a skill file exists but cannot be read, or if writing the
/// summary to stdout fails.
pub fn skills_check(cwd: &Path, dirs: &[PathBuf]) -> Result<bool> {
    let root = skills_root(cwd);
    let targets = resolve_skill_dirs(dirs);
    let outcomes = skills::check(&root, &targets)?;

    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut current = true;
    for outcome in &outcomes {
        let label = match outcome.status {
            skills::Checked::UpToDate => "up to date",
            skills::Checked::Drifted => {
                current = false;
                "drifted"
            }
            skills::Checked::Missing => {
                current = false;
                "missing"
            }
        };
        writeln!(out, "  {label}: {}", relative_to(&root, &outcome.path))?;
    }
    if !current {
        writeln!(
            out,
            "\nChecked-in skills are out of sync; run `bastion skills install` to refresh them."
        )?;
    }
    Ok(current)
}

/// `bastion skills list`: show the skills bundled into this binary.
///
/// # Errors
///
/// Returns an error if writing to stdout fails.
pub fn skills_list() -> Result<()> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    writeln!(
        out,
        "Skills bundled in bastion {}:",
        crate::version::VERSION
    )?;
    for skill in skills::BUNDLED {
        writeln!(out, "  {} - {}", skill.slug, skill.summary)?;
    }
    writeln!(
        out,
        "\nInstall them with `bastion skills install` (default targets: {}).",
        skills::DEFAULT_DIRS.join(", ")
    )?;
    Ok(())
}

/// The repository root to install skills into: the git toplevel containing `cwd`,
/// or `cwd` itself when it is not inside a repo, so first-time setup still works.
fn skills_root(cwd: &Path) -> PathBuf {
    git::repo_root(cwd).unwrap_or_else(|_| cwd.to_path_buf())
}

/// The requested skill directories, falling back to the documented defaults when
/// none were passed.
fn resolve_skill_dirs(dirs: &[PathBuf]) -> Vec<PathBuf> {
    if dirs.is_empty() {
        skills::default_dirs()
    } else {
        dirs.to_vec()
    }
}

/// Display `path` relative to `root` for tidy output, falling back to the full
/// path when it lies outside `root`. Separators are normalized to `/` so the
/// output is consistent across platforms and matches the docs, rather than mixing
/// the `/` in a default like `.claude/skills` with Windows' `\`.
fn relative_to(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
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
fn local_run_id(repo_root: &Path) -> RunId {
    match git::short_head(repo_root) {
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
        let decision = review(&layout, dir, "main", Format::Jsonl)
            .await
            .expect("zero-match review passes");
        assert_eq!(decision, Decision::Pass);

        // The pass was persisted and is now inspectable.
        let runs = store::list_runs(&layout).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].verdict, Some(Decision::Pass));
        assert_eq!(runs[0].reviewers, 0);
    }
}

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

use color_eyre::eyre::{Context, Result, eyre};

use crate::config::Config;
use crate::context::ReviewContext;
use crate::event::{ReviewerRef, RunEvent, RunId};
use crate::git;
use crate::paths::Layout;
use crate::render::{self, Format};
use crate::reviewer::{Mode, ModelId};
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
/// `cwd` is the directory to resolve the repository and config from: the process
/// working directory in normal use, but explicit so the handler is testable.
///
/// `github` carries the `owner/name` slug and PR number when the review runs against a
/// pull request, so the reviewers get its description and discussion as context. It is
/// best effort: a failure to reach GitHub is logged and the review proceeds on the
/// local context (commit messages and prior findings) alone.
///
/// # Errors
///
/// Returns an error if the repository, config, git queries, or persistence fail.
/// A blocked review is *not* an error: it returns `Ok(Decision::Block)`.
pub async fn review(
    layout: &Layout,
    cwd: &Path,
    base: &str,
    format: Format,
    github: Option<GithubSource>,
) -> Result<Decision> {
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
            tokens_in: 0,
            tokens_out: 0,
            cache_read: 0,
            cost_usd: Money::from_cents(0),
        };
        render::write_event(&mut out, format, &completed)?;
        store::write_run(layout, &run, &[started, completed])?;
        return Ok(Decision::Pass);
    }

    // Assemble the review context: the author's stated intent (the PR body, or this
    // branch's commit messages locally), this branch's prior findings recalled from the
    // run store, and the surrounding discussion (GitHub only). Empty when nothing
    // applies, which leaves every reviewer's prompt exactly as it was.
    let mut context = ReviewContext {
        intent: git::commit_messages(&repo_root, base),
        comments: Vec::new(),
        prior_findings: store::prior_findings(layout, &branch, &run),
    };
    if let Some(source) = github.as_ref() {
        match gather_github_context(source).await {
            Ok(gathered) => {
                // A PR body is a better statement of intent than the commit messages,
                // so it wins when present; the discussion is GitHub-only.
                if gathered.intent.is_some() {
                    context.intent = gathered.intent;
                }
                context.comments = gathered.comments;
            }
            Err(err) => {
                eprintln!("bastion review: continuing without GitHub context ({err:#})");
            }
        }
    }

    let ctx = ExecContext {
        run,
        repo_root,
        branch,
        base: base.to_string(),
        changed: changed_count,
        reviewers: reviewer_refs,
        context,
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

/// The pull request a review is running against, so its description and discussion can
/// be gathered as reviewer context. Present only when `bastion review` is given a PR.
#[derive(Debug, Clone)]
pub struct GithubSource {
    /// The `owner/name` repository slug (from `--repo` / `$GITHUB_REPOSITORY`).
    pub repo: String,
    /// The pull request number.
    pub pr: u64,
}

/// Gather a pull request's intent and discussion over a real GitHub client.
///
/// Splits the slug, builds the REST client from the environment (the same token the
/// report step uses), and delegates to the GitHub context producer. Surfaced as an
/// error the caller logs and recovers from, never one that fails the review.
async fn gather_github_context(
    source: &GithubSource,
) -> Result<crate::github::context::GatheredContext> {
    let (owner, name) = source
        .repo
        .split_once('/')
        .filter(|(owner, name)| !owner.is_empty() && !name.is_empty() && !name.contains('/'))
        .ok_or_else(|| eyre!("expected an 'owner/name' repository, got '{}'", source.repo))?;
    let client = crate::github::client::RestClient::from_env()?;
    crate::github::context::gather(&client, owner, name, source.pr).await
}

/// `bastion validate`: parse the reviewer registry and report any problems.
///
/// Loads the registry (the explicit `file`, or the one discovered by walking up
/// from `cwd`) through the same [`Config`] path `bastion review` uses, so it
/// surfaces exactly the errors a real review would hit at load time: malformed
/// YAML, an unknown field, a duplicate reviewer name, or a model pinned under
/// `backend: any`. On success it prints a one-line summary and the parsed
/// reviewers and returns `Ok`; on any problem it returns the error, which
/// `color_eyre` renders before the process exits non-zero, so the command doubles
/// as a CI lint and a cheap local check that never spends a model call.
///
/// # Errors
///
/// Returns an error if no registry is found, the file cannot be read, or it fails
/// to parse or validate.
pub fn validate(cwd: &Path, file: Option<&Path>) -> Result<()> {
    let (path, config) = match file {
        Some(file) => (file.to_path_buf(), Config::load(file)?),
        None => {
            // Resolve from the repo root when we are inside one (so the command
            // works from any subdirectory, like `review`), falling back to `cwd`
            // when git cannot tell us, which keeps a not-yet-initialized repo
            // working. `discover_located` warns on the deprecated location, gives
            // the clear "no registry found" error, and hands back the path it
            // loaded, so the summary reports exactly the file that was parsed.
            let root = git::repo_root(cwd).unwrap_or_else(|_| cwd.to_path_buf());
            let (found, config) = Config::discover_located(&root)?;
            (found.path, config)
        }
    };

    let gates = config
        .reviewers
        .iter()
        .filter(|r| r.mode == Mode::Gate)
        .count();
    let advisors = config.reviewers.len() - gates;

    let stdout = io::stdout();
    let mut out = stdout.lock();
    writeln!(
        out,
        "{} is valid: {} reviewer(s), {gates} gate(s), {advisors} advisor(s).",
        path.display(),
        config.reviewers.len(),
    )?;
    for reviewer in &config.reviewers {
        let model = reviewer.model.as_ref().map_or("default", ModelId::as_str);
        writeln!(
            out,
            "  - {} ({}, backend: {}, model: {model})",
            reviewer.name,
            reviewer.mode.as_str(),
            reviewer.backend.as_str(),
        )?;
    }
    Ok(())
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
    io::stdout()
        .write_all(crate::github::codeowners::block(owners).as_bytes())
        .wrap_err("writing CODEOWNERS block")?;
    Ok(())
}

/// `bastion github report`: post a finished run's results to its pull request.
///
/// Reads the persisted run (the latest, or `run` when given), builds the GitHub
/// client from the Actions environment (`GITHUB_TOKEN`, `GITHUB_API_URL`), and
/// upserts the sticky PR comment plus a check run per reviewer and the aggregate
/// `bastion` check. The run is already persisted by `bastion review`, so this is a
/// pure read-and-post step that can run after the gate has decided.
///
/// `slug` is the `owner/name` repository, `pr` the pull request number, and `sha`
/// the head commit the check runs attach to (all supplied by the workflow from the
/// pull-request event).
///
/// # Errors
///
/// Returns an error if the run cannot be read, the client cannot be built (e.g. a
/// missing token), or a GitHub request fails. A missing run is reported as a
/// non-fatal notice (so a report step running after an infrastructure failure does
/// not pile a second, confusing error on top of the real one).
pub async fn github_report(
    layout: &Layout,
    slug: &str,
    pr: u64,
    sha: &str,
    run: Option<&str>,
) -> Result<()> {
    let ctx = crate::github::PrContext::new(slug, pr, sha)?;

    let run = match store::resolve_run(layout, run) {
        Ok(run) => run,
        Err(err) => {
            // No run to report: surface it as a notice and stop, rather than failing
            // the step on top of whatever already went wrong upstream.
            eprintln!("bastion github report: nothing to report ({err:#})");
            return Ok(());
        }
    };
    let events = store::read_run(layout, &run)?;

    let client = crate::github::client::RestClient::from_env()?;
    let summary = crate::github::report::report(&client, &ctx, &events).await?;

    writeln!(
        io::stdout(),
        "Reported run {run} to {slug}#{pr}: {summary}."
    )
    .wrap_err("writing report summary")?;
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

    #[test]
    fn validate_accepts_a_well_formed_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".bastion.yaml");
        std::fs::write(
            &path,
            "reviewers:\n  - name: a\n    trigger: [src/**]\n    mode: gate\n    prompt: p\n",
        )
        .unwrap();
        validate(tmp.path(), Some(&path)).expect("a well-formed file validates");
    }

    #[test]
    fn validate_reports_a_duplicate_name() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".bastion.yaml");
        std::fs::write(
            &path,
            "reviewers:\n  - name: dup\n    trigger: [a]\n    mode: gate\n    prompt: p\n  - name: dup\n    trigger: [b]\n    mode: gate\n    prompt: p\n",
        )
        .unwrap();
        let err = validate(tmp.path(), Some(&path)).unwrap_err();
        assert!(
            format!("{err:#}").contains("duplicate reviewer name"),
            "error should name the duplicate, got: {err:#}"
        );
    }

    #[test]
    fn validate_reports_an_unknown_field() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".bastion.yaml");
        std::fs::write(
            &path,
            "reviewers:\n  - name: typo\n    trigger: [src/**]\n    mode: gate\n    bakend: codex\n    prompt: p\n",
        )
        .unwrap();
        let err = validate(tmp.path(), Some(&path)).unwrap_err();
        assert!(
            format!("{err:#}").contains("unknown field `bakend`"),
            "validate should reject an unknown field, got: {err:#}"
        );
    }

    #[test]
    fn validate_reports_a_model_under_backend_any() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".bastion.yaml");
        std::fs::write(
            &path,
            "reviewers:\n  - name: stray\n    trigger: [src/**]\n    mode: gate\n    model: gpt-5\n    prompt: p\n",
        )
        .unwrap();
        let err = validate(tmp.path(), Some(&path)).unwrap_err();
        assert!(format!("{err:#}").contains("backend: any"), "got: {err:#}");
    }

    #[test]
    fn validate_discovers_from_the_directory_when_no_file_is_given() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join(".bastion.yaml"),
            "reviewers:\n  - name: a\n    trigger: [x]\n    mode: advisor\n    prompt: p\n",
        )
        .unwrap();
        validate(tmp.path(), None).expect("discovered registry validates");
    }

    #[test]
    fn validate_errors_clearly_when_no_registry_is_found() {
        let tmp = tempfile::tempdir().unwrap();
        let err = validate(tmp.path(), None).unwrap_err();
        assert!(
            format!("{err:#}").contains("no reviewer registry found"),
            "got: {err:#}"
        );
    }

    #[tokio::test]
    async fn review_with_no_matching_reviewers_is_a_persisted_pass() {
        let repo = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let dir = repo.path();

        // A registry whose reviewers only trigger on docs, committed so it is not
        // itself a pending change.
        std::fs::write(
            dir.join(".bastion.yaml"),
            "reviewers:\n  - name: docs-only\n    trigger: [docs/**]\n    mode: gate\n    prompt: p\n",
        )
        .unwrap();
        git(dir, &["init"]);
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", "base"]);

        // Change a source file that no reviewer triggers on.
        std::fs::write(dir.join("main.rs"), "fn main() {}\n").unwrap();

        let layout = Layout::with_root(data.path().to_path_buf());
        let decision = review(&layout, dir, "main", Format::Jsonl, None)
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

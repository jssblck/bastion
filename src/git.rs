//! The handful of git queries the CLI needs.
//!
//! Bastion does not own your VCS any more than it owns your CI; it just reads the
//! current branch and the set of files changed against a base. These shell out to
//! the `git` binary, the same one the surrounding workflow already uses.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use color_eyre::eyre::{Context, Result, bail};

/// Run `git` with `args` in `cwd`, returning trimmed stdout on success.
fn run_git(cwd: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .wrap_err("failed to invoke git; is it installed and on PATH?")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(String::from_utf8(output.stdout)
        .wrap_err("git produced non-UTF-8 output")?
        .trim()
        .to_string())
}

/// The repository root containing `cwd`.
///
/// # Errors
///
/// Returns an error if `cwd` is not inside a git working tree.
pub fn repo_root(cwd: &Path) -> Result<PathBuf> {
    let root = run_git(cwd, &["rev-parse", "--show-toplevel"])?;
    Ok(PathBuf::from(root))
}

/// The current branch name, or `HEAD` when detached.
///
/// # Errors
///
/// Returns an error if `git` fails.
pub fn current_branch(cwd: &Path) -> Result<String> {
    run_git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"])
}

/// The set of files changed in the working tree relative to `base`.
///
/// This is the union of tracked changes against `base` and untracked,
/// non-ignored files, i.e. everything a PR from `base` would introduce,
/// including edits not yet committed. Paths are repository-relative and sorted.
///
/// # Errors
///
/// Returns an error if `git` fails (e.g. `base` does not resolve).
pub fn changed_files(cwd: &Path, base: &str) -> Result<Vec<String>> {
    let mut files = BTreeSet::new();

    let tracked = run_git(cwd, &["diff", "--name-only", base])?;
    files.extend(
        tracked
            .lines()
            .map(str::to_string)
            .filter(|l| !l.is_empty()),
    );

    let untracked = run_git(cwd, &["ls-files", "--others", "--exclude-standard"])?;
    files.extend(
        untracked
            .lines()
            .map(str::to_string)
            .filter(|l| !l.is_empty()),
    );

    Ok(files.into_iter().collect())
}

/// The short commit hash of `HEAD`, or `None` when git cannot supply one (for
/// example a repository with no commits yet).
///
/// Used to key a local run by the changeset head; callers fall back to a fixed
/// marker when it is absent.
#[must_use]
pub fn short_head(cwd: &Path) -> Option<String> {
    run_git(cwd, &["rev-parse", "--short", "HEAD"])
        .ok()
        .filter(|sha| !sha.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// git config flags that make a temp repo deterministic regardless of the
    /// developer's global git configuration.
    const ISOLATE: &[&str] = &[
        "-c",
        "user.email=test@bastion.dev",
        "-c",
        "user.name=Bastion Test",
        "-c",
        "commit.gpgsign=false",
        "-c",
        "init.defaultBranch=main",
    ];

    fn git(cwd: &Path, args: &[&str]) {
        let full: Vec<&str> = ISOLATE
            .iter()
            .copied()
            .chain(args.iter().copied())
            .collect();
        run_git(cwd, &full).unwrap_or_else(|e| panic!("git {args:?} failed: {e}"));
    }

    #[test]
    fn changed_files_reports_tracked_edits_and_untracked_additions() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        git(dir, &["init"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(dir, &["add", "a.txt"]);
        git(dir, &["commit", "-m", "base"]);

        // Dirty the working tree: edit a tracked file, add an untracked one.
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(dir.join("b.txt"), "new\n").unwrap();

        let changed = changed_files(dir, "main").expect("diff against main");
        assert!(changed.contains(&"a.txt".to_string()), "got {changed:?}");
        assert!(changed.contains(&"b.txt".to_string()), "got {changed:?}");

        assert_eq!(current_branch(dir).unwrap(), "main");
        assert_eq!(
            repo_root(dir).unwrap().canonicalize().unwrap(),
            dir.canonicalize().unwrap()
        );
    }

    #[test]
    fn short_head_reports_a_hash_after_a_commit_and_none_before() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git(dir, &["init"]);

        // No commits yet: HEAD does not resolve.
        assert!(short_head(dir).is_none());

        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(dir, &["add", "a.txt"]);
        git(dir, &["commit", "-m", "base"]);

        let sha = short_head(dir).expect("a commit exists");
        assert!(!sha.is_empty());
        // A short hash is a handful of hex characters with no whitespace.
        assert!(sha.chars().all(|c| c.is_ascii_hexdigit()), "got {sha:?}");
    }
}

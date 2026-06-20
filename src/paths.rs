//! The on-disk layout of Bastion's per-user data directory.
//!
//! Every run is persisted under a platform data directory so it can be inspected
//! or replayed after the fact (`docs/LOCAL.md`):
//!
//! ```text
//! <data-dir>/
//!   runs/
//!     r-0f3a/
//!       run.jsonl                 # the full event stream
//!       reviewers/
//!         tenant-isolation/
//!           transcript.jsonl      # the full agent session
//!           verdict.json          # the raw structured verdict
//!           meta.json             # backend, timing, usage, matched trigger
//!     latest                      # pointer to the most recent run
//! ```

use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, eyre};

use crate::event::RunId;

/// Environment variable that overrides the data directory (used in tests and for
/// callers who want runs stored somewhere specific).
pub const DATA_DIR_ENV: &str = "BASTION_DATA_DIR";

/// Resolves paths within Bastion's data directory.
#[derive(Debug, Clone)]
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    /// Resolve the data directory by platform convention, honoring
    /// [`DATA_DIR_ENV`] when set:
    ///
    /// - Linux: `$XDG_DATA_HOME/bastion` (default `~/.local/share/bastion`)
    /// - macOS: `~/Library/Application Support/bastion`
    /// - Windows: `%APPDATA%\bastion`
    ///
    /// # Errors
    ///
    /// Returns an error if no home directory can be determined and no override is
    /// set.
    pub fn resolve() -> Result<Self> {
        if let Some(over) = std::env::var_os(DATA_DIR_ENV).filter(|v| !v.is_empty()) {
            return Ok(Self::with_root(PathBuf::from(over)));
        }
        let base = directories::BaseDirs::new()
            .ok_or_else(|| eyre!("could not determine a home directory; set {DATA_DIR_ENV}"))?;
        Ok(Self::with_root(base.data_dir().join("bastion")))
    }

    /// Build a layout rooted at an explicit path.
    #[must_use]
    pub fn with_root(root: PathBuf) -> Self {
        Self { root }
    }

    /// The data directory root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The directory holding all runs (`<root>/runs`).
    #[must_use]
    pub fn runs_dir(&self) -> PathBuf {
        self.root.join("runs")
    }

    /// The directory for a single run (`<root>/runs/<id>`).
    #[must_use]
    pub fn run_dir(&self, id: &RunId) -> PathBuf {
        self.runs_dir().join(id.as_str())
    }

    /// The full event stream for a run (`.../run.jsonl`).
    #[must_use]
    pub fn run_jsonl(&self, id: &RunId) -> PathBuf {
        self.run_dir(id).join("run.jsonl")
    }

    /// A single reviewer's directory within a run.
    #[must_use]
    pub fn reviewer_dir(&self, id: &RunId, reviewer: &str) -> PathBuf {
        self.run_dir(id).join("reviewers").join(reviewer)
    }

    /// A reviewer's saved session transcript.
    #[must_use]
    pub fn transcript(&self, id: &RunId, reviewer: &str) -> PathBuf {
        self.reviewer_dir(id, reviewer).join("transcript.jsonl")
    }

    /// The pointer file recording the most recent run id (`<root>/runs/latest`).
    ///
    /// A plain file holding the id is used rather than a symlink, since symlink
    /// creation is not universally available on Windows.
    #[must_use]
    pub fn latest_pointer(&self) -> PathBuf {
        self.runs_dir().join("latest")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_override_takes_precedence() {
        let tmp = tempfile::tempdir().unwrap();
        // Safety: single-threaded test; no other thread reads the environment here.
        unsafe { std::env::set_var(DATA_DIR_ENV, tmp.path()) };
        let layout = Layout::resolve().expect("resolves from override");
        assert_eq!(layout.root(), tmp.path());
        unsafe { std::env::remove_var(DATA_DIR_ENV) };
    }

    #[test]
    fn layout_paths_compose_as_documented() {
        let layout = Layout::with_root(PathBuf::from("/data"));
        let id = RunId("r-0f3a".into());
        assert!(layout.run_jsonl(&id).ends_with("runs/r-0f3a/run.jsonl"));
        assert!(
            layout
                .transcript(&id, "tenant-isolation")
                .ends_with("runs/r-0f3a/reviewers/tenant-isolation/transcript.jsonl")
        );
        assert!(layout.latest_pointer().ends_with("runs/latest"));
    }
}

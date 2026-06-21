//! The subprocess boundary, behind an injectable seam.
//!
//! Backends shell out to an agent CLI (`claude`, `codex`, ...). To keep that
//! testable without the real binary or a network, the actual process spawn lives
//! behind [`CommandRunner`]: production uses [`SystemCommandRunner`] (a real
//! `tokio` child process), while tests inject a runner that drives a fake
//! executable or canned output. The trait is the one place that touches the OS,
//! so everything above it is deterministic.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use color_eyre::eyre::{Context, Result};

/// A fully-specified invocation of an agent CLI.
///
/// This is the parsed, proof-carrying form a backend hands to a [`CommandRunner`]:
/// the program, its arguments, the working directory, and the environment
/// overlay are all resolved, so the runner only has to spawn it.
#[derive(Debug, Clone)]
pub struct CommandSpec {
    /// The program to execute (e.g. the `claude` binary path).
    pub program: OsString,
    /// The arguments, in order.
    pub args: Vec<OsString>,
    /// The working directory to run in (the repository checkout).
    pub cwd: PathBuf,
    /// Environment variables to set for the child, layered over the parent's.
    pub env: BTreeMap<String, String>,
}

impl CommandSpec {
    /// Start a spec for `program` running in `cwd`, with no args or env overlay.
    pub fn new(program: impl Into<OsString>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: cwd.into(),
            env: BTreeMap::new(),
        }
    }

    /// Append one argument.
    pub fn arg(&mut self, arg: impl Into<OsString>) -> &mut Self {
        self.args.push(arg.into());
        self
    }
}

/// The captured result of running a [`CommandSpec`] to completion.
#[derive(Debug, Clone)]
pub struct CommandOutput {
    /// The process exit code, or `None` if it was killed by a signal.
    pub code: Option<i32>,
    /// Captured standard output.
    pub stdout: String,
    /// Captured standard error.
    pub stderr: String,
}

impl CommandOutput {
    /// Whether the process exited successfully (code 0).
    #[must_use]
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// The seam over process execution: run a [`CommandSpec`] and capture its output.
///
/// Production wires this to a real child process; tests drive a fake executable
/// or canned responses through the same interface, so backends never special-case
/// being under test.
#[allow(
    async_fn_in_trait,
    reason = "single-crate trait consumed internally, not across a public API boundary"
)]
pub trait CommandRunner: Send + Sync {
    /// Run the command to completion and return its captured output.
    ///
    /// # Errors
    ///
    /// Returns an error if the process cannot be spawned (e.g. the program is not
    /// on `PATH`) or its output cannot be captured. A non-zero exit is *not* an
    /// error here — it is reported via [`CommandOutput::code`] so the caller can
    /// decide what it means.
    async fn run(&self, spec: &CommandSpec) -> Result<CommandOutput>;
}

/// A [`CommandRunner`] that spawns a real child process via `tokio`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    async fn run(&self, spec: &CommandSpec) -> Result<CommandOutput> {
        let mut command = tokio::process::Command::new(&spec.program);
        command
            .args(&spec.args)
            .current_dir(&spec.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // The runner bounds each reviewer with `tokio::time::timeout`; on
            // timeout it drops this future. Without `kill_on_drop`, the agent
            // subprocess would keep running detached -- still using tools, mutating
            // the checkout, and burning tokens after Bastion has already failed the
            // reviewer closed. Killing the child on drop makes the timeout real.
            .kill_on_drop(true);
        for (key, value) in &spec.env {
            command.env(key, value);
        }

        let output = command.output().await.wrap_err_with(|| {
            format!(
                "failed to spawn '{}'; is it installed and on PATH?",
                spec.program.to_string_lossy()
            )
        })?;

        Ok(CommandOutput {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// Resolve the program path for a backend CLI, honoring an environment override.
///
/// Each backend has a default program name (e.g. `claude`) found on `PATH`; the
/// `override_env` variable lets a deployment or a test point at a specific binary
/// or a fake script instead.
#[must_use]
pub fn resolve_program(default: &str, override_env: &str) -> OsString {
    match std::env::var_os(override_env).filter(|v| !v.is_empty()) {
        Some(path) => path,
        None => OsString::from(default),
    }
}

/// Whether a program resolves to something runnable: an existing file when it
/// looks like a path, otherwise assumed present on `PATH`.
///
/// Backends use this to detect-and-skip when the real CLI is absent, so tests on
/// machines without the agent installed do not spuriously fail.
#[must_use]
pub fn program_is_available(program: &Path) -> bool {
    // A bare command name (no separators) is assumed to be on PATH; we cannot
    // cheaply prove otherwise without running it. A path-like program must exist.
    let looks_like_path =
        program.components().nth(1).is_some_and(|_| true) || program.is_absolute();
    if looks_like_path {
        program.exists()
    } else {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_program_prefers_override() {
        // Safety: single-threaded test; no other thread reads the environment here.
        unsafe { std::env::set_var("BASTION_TEST_PROG_OVERRIDE", "/opt/fake/claude") };
        let resolved = resolve_program("claude", "BASTION_TEST_PROG_OVERRIDE");
        assert_eq!(resolved, OsString::from("/opt/fake/claude"));
        unsafe { std::env::remove_var("BASTION_TEST_PROG_OVERRIDE") };
    }

    #[test]
    fn resolve_program_falls_back_to_default() {
        unsafe { std::env::remove_var("BASTION_TEST_PROG_MISSING") };
        let resolved = resolve_program("claude", "BASTION_TEST_PROG_MISSING");
        assert_eq!(resolved, OsString::from("claude"));
    }

    #[test]
    fn bare_command_is_assumed_available() {
        assert!(program_is_available(Path::new("claude")));
    }

    #[test]
    fn missing_path_program_is_unavailable() {
        assert!(!program_is_available(Path::new("/no/such/bin/claude")));
    }
}

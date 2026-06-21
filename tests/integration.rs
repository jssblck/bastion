//! End-to-end integration suite.
//!
//! Everything else in this crate is exercised by inline `#[cfg(test)]` modules
//! over real pure functions and the injectable backend seam. This file is the
//! missing top: it drives the *real compiled `bastion` binary* (via
//! `CARGO_BIN_EXE_bastion`) as a black box, each scenario in its own isolated
//! environment -- a throwaway `git` repository, a private `BASTION_DATA_DIR`, and
//! a compiled fake agent standing in for the heavyweight Claude Code / Codex
//! subprocesses the real backends shell out to.
//!
//! The fake agent ([`FAKE_AGENT_SRC`]) is compiled once with `rustc` and pointed
//! at through `BASTION_CLAUDE_BIN` / `BASTION_CODEX_BIN`, so the binary takes the
//! genuine subprocess path: real spawn, real stdin/argv, real stdout capture, real
//! parse, real fail-closed/fail-open aggregation, real persistence. The fake reads
//! per-reviewer `env` (which Bastion propagates into the child) to choose how to
//! behave -- pass, block, return malformed output, crash, or hang.
//!
//! Crucially, the fake is also a *contract checker*: before it emits anything it
//! validates the invocation it received (the structured-output flags, the piped
//! prompt, the resume/session identifiers on a reprompt) and exits non-zero on any
//! mismatch. A backend that stopped passing the schema, dropped the prompt, or
//! botched a session resume would therefore turn these green tests red, even
//! though the assertions never look at the argv directly.
//!
//! Scenarios that need a toolchain we cannot guarantee (no `rustc`, no `git`)
//! detect-and-skip rather than fail locally, mirroring the existing backend tests;
//! in CI (where `CI` is set) the tools must be present so the suite cannot silently
//! become a no-op.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use bastion::event::{Gates, RunEvent};
use bastion::paths::Layout;
use bastion::store::{self, RunSummary};
use bastion::verdict::{Decision, Finding, FindingKind, Money, Usage, Verdict};

// ---------------------------------------------------------------------------
// The fake agent: a deterministic, contract-checking stand-in for claude/codex.
// ---------------------------------------------------------------------------

/// Source for a tiny native program that emulates both agent CLIs.
///
/// It detects which protocol it is being driven as from its argv (Codex is
/// invoked with an `exec` subcommand; Claude with `--output-format`), detects a
/// reprompt turn (Codex `resume` / Claude `--resume`), validates the invocation
/// against the contract each backend promises, and then chooses its output from
/// the `FAKE_BEHAVIOR` environment variable Bastion propagates from the reviewer:
///
/// - `pass`             -- a consistent passing verdict (the default).
/// - `block`            -- a consistent blocking verdict with one blocking finding.
/// - `inconsistent`     -- a `block` with no blocking finding (an internally
///   inconsistent verdict the backend must reject, then reprompt, then fail closed).
/// - `malformed`        -- output with no parseable verdict, on every turn (drives
///   the backend's single reprompt, then a fail-closed block for a gate).
/// - `reprompt-recover` -- malformed on the first turn, valid on the resumed turn
///   (exercises the reprompt-and-recover path through the real subprocess).
/// - `crash`            -- exit non-zero (an execution failure -> fail closed).
/// - `slow`             -- sleep `FAKE_SLEEP_MS` then pass (pair with a short
///   reviewer `timeout` to force a timeout; or a generous one to test concurrency).
///
/// Extra knobs, all read from the propagated environment: `FAKE_COST_CENTS`
/// (default 5), `FAKE_TOKENS_IN`/`FAKE_TOKENS_OUT`, `FAKE_SUMMARY`,
/// `FAKE_EXPECT_PROMPT_CONTAINS` (assert the delivered prompt contains a marker --
/// used to verify `${...}` interpolation end to end), and `FAKE_MARKER_FILE`
/// (written only *after* the sleep, so a killed-on-timeout child never writes it).
const FAKE_AGENT_SRC: &str = r##"
use std::io::Read;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn has(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a.as_str() == flag)
}

fn fail(reason: &str) -> ! {
    eprintln!("fake agent contract violation: {reason}");
    std::process::exit(3);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let is_codex = has(&args, "exec");
    let is_reprompt = has(&args, "resume") || has(&args, "--resume");

    // Drain stdin so a parent piping a prompt over it never blocks on a full pipe.
    let mut stdin = String::new();
    let _ = std::io::stdin().read_to_string(&mut stdin);

    // The prompt is delivered over stdin by Codex (the trailing `-`) and over argv
    // by Claude (`-p <prompt>`). Recover whichever applies so the contract checks
    // can confirm it actually arrived.
    let prompt = if is_codex {
        stdin.clone()
    } else {
        let mut found = String::new();
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            if arg == "-p" {
                if let Some(value) = iter.next() {
                    found = value.clone();
                }
                break;
            }
        }
        found
    };

    // --- contract checks: the invocation must match what the backend promises ---
    if is_codex {
        // `exec` is already implied by is_codex; assert the rest of the contract.
        if !has(&args, "--json") {
            fail("codex: missing `--json`");
        }
        if !has(&args, "--dangerously-bypass-approvals-and-sandbox") {
            fail("codex: missing `--dangerously-bypass-approvals-and-sandbox` (unattended mode)");
        }
        if args.last().map(String::as_str) != Some("-") {
            fail("codex: prompt is not read from stdin (last arg is not `-`)");
        }
        if stdin.trim().is_empty() {
            fail("codex: no prompt was piped over stdin");
        }
        if is_reprompt && !has(&args, "th-fake") {
            fail("codex: reprompt did not resume the reported thread id `th-fake`");
        }
    } else {
        for flag in ["--output-format", "--json-schema", "--permission-mode", "bypassPermissions", "-p"] {
            if !has(&args, flag) {
                fail(&format!("claude: missing `{flag}`"));
            }
        }
        if is_reprompt && !has(&args, "s-fake") {
            fail("claude: reprompt did not resume the reported session id `s-fake`");
        }
    }

    // Every turn carries the verdict schema instruction; the first turn also
    // carries the changeset preamble. (A reprompt re-sends only the schema ask.)
    if !prompt.contains("verdict") {
        fail("prompt did not carry the verdict schema instruction");
    }
    if !is_reprompt {
        if !prompt.contains("changeset") {
            fail("first-turn prompt did not carry the changeset preamble");
        }
        if let Ok(expected) = std::env::var("FAKE_EXPECT_PROMPT_CONTAINS") {
            if !expected.is_empty() && !prompt.contains(&expected) {
                fail(&format!("prompt did not contain expected marker `{expected}` (interpolation?)"));
            }
        }
    }

    let behavior = env_or("FAKE_BEHAVIOR", "pass");

    let sleep_ms: u64 = env_or("FAKE_SLEEP_MS", "0").parse().unwrap_or(0);
    if sleep_ms > 0 {
        std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
    }

    // Written only after the sleep: a child killed on timeout never reaches here,
    // which is how the timeout test proves `kill_on_drop` actually fires.
    if let Ok(marker) = std::env::var("FAKE_MARKER_FILE") {
        if !marker.is_empty() {
            let _ = std::fs::write(&marker, "alive");
        }
    }

    if behavior == "crash" {
        eprintln!("fake agent: simulated crash");
        std::process::exit(7);
    }

    // reprompt-recover is malformed on the first turn and valid once resumed.
    let effective = if behavior == "reprompt-recover" {
        if is_reprompt { "pass" } else { "malformed" }
    } else {
        behavior.as_str()
    };

    let cost_cents: u64 = env_or("FAKE_COST_CENTS", "5").parse().unwrap_or(5);
    let tin: u64 = env_or("FAKE_TOKENS_IN", "100").parse().unwrap_or(100);
    let tout: u64 = env_or("FAKE_TOKENS_OUT", "10").parse().unwrap_or(10);
    let dollars = format!("{}.{:02}", cost_cents / 100, cost_cents % 100);
    let summary = env_or("FAKE_SUMMARY", "fake reviewer verdict");

    if is_codex {
        emit_codex(effective, &summary, &dollars, tin, tout);
    } else {
        emit_claude(effective, &summary, &dollars, tin, tout);
    }
}

fn emit_codex(behavior: &str, summary: &str, dollars: &str, tin: u64, tout: u64) {
    // The verdict travels inside a JSON-lines `agent_message` as a fenced YAML
    // block. The `\n` below are JSON string escapes: serde_json decodes them into
    // real newlines when the backend parses each event line, so the fenced block
    // splits correctly. (Emitting literal newlines here would be invalid JSON.)
    println!("{}", r#"{"type":"thread.started","thread_id":"th-fake"}"#);

    let mut text = String::new();
    match behavior {
        "block" => {
            text.push_str(r#"```yaml\nverdict: block\nsummary: "#);
            text.push_str(summary);
            text.push_str(r#"\nfindings:\n  - kind: blocking\n    path: src/extra.rs\n    line_start: 1\n    line_end: 1\n    detail: simulated blocking finding\n```"#);
        }
        "inconsistent" => {
            // A `block` with no blocking finding: internally inconsistent.
            text.push_str(r#"```yaml\nverdict: block\nsummary: "#);
            text.push_str(summary);
            text.push_str(r#"\nfindings: []\n```"#);
        }
        "malformed" => {
            text.push_str("I looked at the changeset but I will not give a verdict.");
        }
        _ => {
            text.push_str(r#"```yaml\nverdict: pass\nsummary: "#);
            text.push_str(summary);
            text.push_str(r#"\nfindings: []\n```"#);
        }
    }

    let mut line = String::new();
    line.push_str(r#"{"type":"item.completed","item":{"type":"agent_message","text":""#);
    line.push_str(&text);
    line.push_str(r#""}}"#);
    println!("{}", line);

    let mut usage = String::new();
    usage.push_str(r#"{"type":"turn.completed","usage":{"input_tokens":"#);
    usage.push_str(&tin.to_string());
    usage.push_str(r#","output_tokens":"#);
    usage.push_str(&tout.to_string());
    usage.push_str(r#","cost_usd":"#);
    usage.push_str(dollars);
    usage.push_str(r#"}}"#);
    println!("{}", usage);
}

fn emit_claude(behavior: &str, summary: &str, dollars: &str, tin: u64, tout: u64) {
    let mut body = String::new();
    match behavior {
        "malformed" => {
            body.push_str(r#"{"session_id":"s-fake","result":"I reviewed it but I forgot the schema."}"#);
        }
        "block" => {
            body.push_str(r#"{"session_id":"s-fake","total_cost_usd":"#);
            body.push_str(dollars);
            body.push_str(r#","usage":{"input_tokens":"#);
            body.push_str(&tin.to_string());
            body.push_str(r#","output_tokens":"#);
            body.push_str(&tout.to_string());
            body.push_str(r#"},"structured_output":{"verdict":"block","summary":""#);
            body.push_str(summary);
            body.push_str(r#"","findings":[{"kind":"blocking","path":"src/extra.rs","line_start":1,"line_end":1,"detail":"simulated blocking finding"}]}}"#);
        }
        "inconsistent" => {
            body.push_str(r#"{"session_id":"s-fake","structured_output":{"verdict":"block","summary":""#);
            body.push_str(summary);
            body.push_str(r#"","findings":[]}}"#);
        }
        _ => {
            body.push_str(r#"{"session_id":"s-fake","total_cost_usd":"#);
            body.push_str(dollars);
            body.push_str(r#","usage":{"input_tokens":"#);
            body.push_str(&tin.to_string());
            body.push_str(r#","output_tokens":"#);
            body.push_str(&tout.to_string());
            body.push_str(r#"},"structured_output":{"verdict":"pass","summary":""#);
            body.push_str(summary);
            body.push_str(r#"","findings":[]}}"#);
        }
    }
    print!("{}", body);
}
"##;

/// Compile the fake agent once per test process and cache its path.
///
/// The executable lives in a leaked temp directory so it survives for the whole
/// process (all scenarios share the one binary). Returns `None` -- so callers
/// detect-and-skip -- when no usable `rustc` is on `PATH`.
fn fake_agent() -> Option<&'static Path> {
    static FAKE: OnceLock<Option<PathBuf>> = OnceLock::new();
    FAKE.get_or_init(build_fake_agent).as_deref()
}

fn build_fake_agent() -> Option<PathBuf> {
    let dir = tempfile::tempdir().ok()?;
    // Leak the handle so the compiled binary outlives this call for the process.
    let dir: &'static tempfile::TempDir = Box::leak(Box::new(dir));

    let src = dir.path().join("fake_agent.rs");
    std::fs::write(&src, FAKE_AGENT_SRC).ok()?;

    let exe = dir.path().join(if cfg!(windows) {
        "fake-agent.exe"
    } else {
        "fake-agent"
    });

    let output = Command::new("rustc")
        .arg(&src)
        .arg("--edition")
        .arg("2021")
        .arg("-O")
        .arg("-o")
        .arg(&exe)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .ok()?;

    if output.status.success() && exe.exists() {
        Some(exe)
    } else {
        eprintln!(
            "could not build the fake agent with rustc:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        None
    }
}

/// Whether `git` is on `PATH` (scenarios stand up throwaway repos with it).
fn git_available() -> bool {
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        Command::new("git")
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

/// The set of tools a scenario needs; `None` means skip this run.
///
/// In CI (where `CI` is set) the tools must be present: a silently-skipped suite
/// is worse than a red one, because it would let the whole end-to-end layer rot
/// undetected. Locally, missing tools just skip.
fn tooling() -> Option<&'static Path> {
    let in_ci = std::env::var_os("CI").is_some();
    if !git_available() {
        assert!(
            !in_ci,
            "git must be available to run the integration suite in CI"
        );
        eprintln!("skipping integration scenario: git is not available");
        return None;
    }
    match fake_agent() {
        Some(path) => Some(path),
        None => {
            assert!(
                !in_ci,
                "rustc must be available to build the fake agent for the integration suite in CI"
            );
            eprintln!("skipping integration scenario: no usable rustc to build the fake agent");
            None
        }
    }
}

/// `git` settings that make a throwaway repo deterministic regardless of the
/// developer's global configuration (identity, signing, default branch).
const GIT_ISOLATE: &[&str] = &[
    "-c",
    "user.email=test@bastion.dev",
    "-c",
    "user.name=Bastion Test",
    "-c",
    "commit.gpgsign=false",
    "-c",
    "init.defaultBranch=main",
];

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(GIT_ISOLATE)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap_or_else(|e| panic!("git {args:?} failed to launch: {e}"));
    assert!(status.success(), "git {args:?} exited unsuccessfully");
}

// ---------------------------------------------------------------------------
// Building a reviewer registry.
// ---------------------------------------------------------------------------

/// A reviewer to write into a scenario's `reviewers.yaml`. All reviewers trigger
/// on `src/**/*.rs`, which the test repo always dirties, so each one runs.
struct Reviewer {
    name: &'static str,
    backend: &'static str,
    mode: &'static str,
    /// `FAKE_BEHAVIOR` and any other env the fake (or test) wants propagated.
    env: Vec<(&'static str, &'static str)>,
    /// `${name}` inputs interpolated into the prompt before the agent sees it.
    inputs: Vec<(&'static str, &'static str)>,
    /// A human-form `timeout` (e.g. `"500ms"`), when the reviewer needs one.
    timeout: Option<&'static str>,
    /// The prompt body (defaults to a generic instruction).
    prompt: Option<&'static str>,
}

impl Reviewer {
    fn new(name: &'static str, backend: &'static str, mode: &'static str) -> Self {
        Self {
            name,
            backend,
            mode,
            env: Vec::new(),
            inputs: Vec::new(),
            timeout: None,
            prompt: None,
        }
    }

    fn behavior(mut self, behavior: &'static str) -> Self {
        self.env.push(("FAKE_BEHAVIOR", behavior));
        self
    }

    fn env(mut self, key: &'static str, value: &'static str) -> Self {
        self.env.push((key, value));
        self
    }

    fn input(mut self, key: &'static str, value: &'static str) -> Self {
        self.inputs.push((key, value));
        self
    }

    fn prompt(mut self, prompt: &'static str) -> Self {
        self.prompt = Some(prompt);
        self
    }

    fn timeout(mut self, timeout: &'static str) -> Self {
        self.timeout = Some(timeout);
        self
    }

    fn to_yaml(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!("  - name: {}\n", self.name));
        s.push_str("    trigger: [src/**/*.rs]\n");
        s.push_str(&format!("    mode: {}\n", self.mode));
        s.push_str(&format!("    backend: {}\n", self.backend));
        if let Some(timeout) = self.timeout {
            s.push_str(&format!("    timeout: {timeout}\n"));
        }
        if !self.env.is_empty() {
            s.push_str("    env:\n");
            for (key, value) in &self.env {
                // Single-quote so values with path separators or spaces stay literal.
                s.push_str(&format!("      {key}: '{value}'\n"));
            }
        }
        if !self.inputs.is_empty() {
            s.push_str("    inputs:\n");
            for (key, value) in &self.inputs {
                s.push_str(&format!("      {key}: '{value}'\n"));
            }
        }
        let prompt = self.prompt.unwrap_or("review the changeset");
        s.push_str(&format!("    prompt: '{prompt}'\n"));
        s
    }
}

fn registry(reviewers: &[Reviewer]) -> String {
    let mut yaml = String::from("reviewers:\n");
    for reviewer in reviewers {
        yaml.push_str(&reviewer.to_yaml());
    }
    yaml
}

// ---------------------------------------------------------------------------
// An isolated repo + data directory, and the binary under test.
// ---------------------------------------------------------------------------

/// The compiled `bastion` binary Cargo built for this test.
fn bastion_bin() -> &'static str {
    env!("CARGO_BIN_EXE_bastion")
}

/// One throwaway environment: a git repo with a committed base and a dirty working
/// tree, plus a private data directory for run history.
struct TestRepo {
    repo: tempfile::TempDir,
    data: tempfile::TempDir,
}

impl TestRepo {
    /// Stand up a repo whose `bastion/reviewers.yaml` is `registry_yaml`, with a
    /// committed `src/lib.rs` base and an uncommitted changeset on top (an edit
    /// plus a new file), so a `review --base main` always has files to route.
    fn new(registry_yaml: &str) -> Self {
        Self::build(Some(registry_yaml))
    }

    /// A repo with no reviewer registry at all (for the discovery error path).
    fn without_registry() -> Self {
        Self::build(None)
    }

    fn build(registry_yaml: Option<&str>) -> Self {
        let repo = tempfile::tempdir().expect("repo tempdir");
        let dir = repo.path();

        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/lib.rs"), "pub fn base() {}\n").unwrap();
        std::fs::write(dir.join("README.md"), "scenario repo\n").unwrap();
        if let Some(yaml) = registry_yaml {
            std::fs::create_dir_all(dir.join("bastion")).unwrap();
            std::fs::write(dir.join("bastion/reviewers.yaml"), yaml).unwrap();
        }

        git(dir, &["init"]);
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", "base"]);

        // Dirty the tree: edit a tracked source file and add an untracked one. Both
        // match `src/**/*.rs`, so every reviewer in the registry triggers.
        Self::dirty(dir);

        let data = tempfile::tempdir().expect("data tempdir");
        Self { repo, data }
    }

    /// Re-dirty the working tree (used between runs to keep a changeset present).
    fn dirty(dir: &Path) {
        std::fs::write(
            dir.join("src/lib.rs"),
            "pub fn base() {}\npub fn added() {}\n",
        )
        .unwrap();
        std::fs::write(dir.join("src/extra.rs"), "pub fn extra() {}\n").unwrap();
    }

    fn path(&self) -> &Path {
        self.repo.path()
    }

    /// Commit the current working tree, advancing HEAD (and the run id).
    fn commit_all(&self, message: &str) {
        git(self.path(), &["add", "."]);
        git(self.path(), &["commit", "-m", message]);
    }

    /// Run `bastion <args>` in this repo with the fake agent wired in for both
    /// backends, this repo's private data directory, and any `extra_env` (which
    /// Bastion inherits and propagates to the agent child).
    fn run(&self, fake: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
        let mut command = Command::new(bastion_bin());
        command
            .args(args)
            .current_dir(self.repo.path())
            .env("BASTION_DATA_DIR", self.data.path())
            .env("BASTION_CLAUDE_BIN", fake)
            .env("BASTION_CODEX_BIN", fake)
            // Keep stdout pure JSONL; route library logging away from the stream.
            .env("RUST_LOG", "error")
            .stdin(Stdio::null());
        for (key, value) in extra_env {
            command.env(key, value);
        }
        command.output().expect("bastion binary runs")
    }

    /// Run `bastion review --base <base> --format jsonl` and parse the stream.
    fn review_base(&self, fake: &Path, base: &str, extra_env: &[(&str, &str)]) -> ReviewRun {
        let output = self.run(
            fake,
            &["review", "--base", base, "--format", "jsonl"],
            extra_env,
        );
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let events = parse_events(&stdout, &stderr);
        ReviewRun {
            code: output.status.code(),
            events,
            stderr,
        }
    }

    fn review(&self, fake: &Path) -> ReviewRun {
        self.review_base(fake, "main", &[])
    }

    /// A [`Layout`] over this repo's data directory, for asserting on what was
    /// persisted using the crate's real store API.
    fn layout(&self) -> Layout {
        Layout::with_root(self.data.path().to_path_buf())
    }
}

/// Parse a JSONL event stream into typed [`RunEvent`]s, failing loudly (with the
/// process's stderr) if any line is not a valid event -- a stray write to stdout
/// would corrupt the contract this whole system depends on.
fn parse_events(stdout: &str, stderr: &str) -> Vec<RunEvent> {
    stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<RunEvent>(line).unwrap_or_else(|e| {
                panic!("stdout line is not a valid run event: {e}\nline: {line}\nstderr:\n{stderr}")
            })
        })
        .collect()
}

/// The dotted wire name of a run event, for asserting on stream ordering.
fn event_kind(event: &RunEvent) -> &'static str {
    match event {
        RunEvent::RunStarted { .. } => "run.started",
        RunEvent::ReviewerStarted { .. } => "reviewer.started",
        RunEvent::ReviewerResolved { .. } => "reviewer.resolved",
        RunEvent::RunCompleted { .. } => "run.completed",
        // `RunEvent` is `#[non_exhaustive]`; a new variant should surface here.
        _ => "unknown",
    }
}

/// The parsed result of one `bastion review`.
struct ReviewRun {
    code: Option<i32>,
    events: Vec<RunEvent>,
    stderr: String,
}

impl ReviewRun {
    fn exited_zero(&self) -> bool {
        self.code == Some(0)
    }

    /// The aggregate decision and gate tally from the closing `run.completed`.
    fn completed(&self) -> (Decision, Gates, Money) {
        for event in &self.events {
            if let RunEvent::RunCompleted {
                verdict,
                gates,
                cost_usd,
                ..
            } = event
            {
                return (*verdict, *gates, *cost_usd);
            }
        }
        panic!("no run.completed in stream; stderr:\n{}", self.stderr);
    }

    /// The resolved verdict, summary, findings, and usage for one reviewer.
    fn resolved(&self, name: &str) -> (Decision, String, Vec<Finding>, Option<Usage>) {
        for event in &self.events {
            if let RunEvent::ReviewerResolved {
                reviewer,
                verdict,
                summary,
                findings,
                usage,
                ..
            } = event
                && reviewer == name
            {
                return (*verdict, summary.clone(), findings.clone(), *usage);
            }
        }
        panic!(
            "no reviewer.resolved for '{name}'; stderr:\n{}",
            self.stderr
        );
    }

    fn resolved_count(&self) -> usize {
        self.events
            .iter()
            .filter(|e| matches!(e, RunEvent::ReviewerResolved { .. }))
            .count()
    }

    fn started_count(&self) -> usize {
        self.events
            .iter()
            .filter(|e| matches!(e, RunEvent::ReviewerStarted { .. }))
            .count()
    }
}

// ---------------------------------------------------------------------------
// Core aggregation scenarios (jsonl).
// ---------------------------------------------------------------------------

/// All gates pass across both real backends -> the binary exits zero, reports a
/// clean aggregate, and persists an inspectable run.
#[test]
fn all_gates_pass_across_both_backends() {
    let Some(fake) = tooling() else { return };

    let repo = TestRepo::new(&registry(&[
        Reviewer::new("claude-gate", "claude-code", "gate").behavior("pass"),
        Reviewer::new("codex-gate", "codex", "gate").behavior("pass"),
        Reviewer::new("default-gate", "any", "gate").behavior("pass"),
    ]));
    let run = repo.review(fake);

    assert!(run.exited_zero(), "stderr:\n{}", run.stderr);
    let (decision, gates, _cost) = run.completed();
    assert_eq!(decision, Decision::Pass);
    assert_eq!(gates.total, 3);
    assert_eq!(gates.passed, 3);
    assert_eq!(gates.blocked, 0);

    assert_eq!(run.started_count(), 3);
    assert_eq!(run.resolved_count(), 3);

    let runs = store::list_runs(&repo.layout()).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].verdict, Some(Decision::Pass));
    assert_eq!(runs[0].reviewers, 3);
}

/// A single blocking gate makes the binary exit non-zero (so an agent loop and CI
/// agree the gate failed), carries its findings, and does not stop the other
/// reviewers from resolving.
#[test]
fn a_blocking_gate_makes_the_binary_exit_nonzero() {
    let Some(fake) = tooling() else { return };

    let repo = TestRepo::new(&registry(&[
        Reviewer::new("ok-gate", "codex", "gate").behavior("pass"),
        Reviewer::new("bad-gate", "claude-code", "gate").behavior("block"),
    ]));
    let run = repo.review(fake);

    assert_eq!(run.code, Some(1), "a blocked review must exit 1");
    let (decision, gates, _cost) = run.completed();
    assert_eq!(decision, Decision::Block);
    assert_eq!(gates.total, 2);
    assert_eq!(gates.passed, 1);
    assert_eq!(gates.blocked, 1);

    let (verdict, _summary, findings, _usage) = run.resolved("bad-gate");
    assert_eq!(verdict, Decision::Block);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].kind, FindingKind::Blocking);
    assert_eq!(findings[0].path, "src/extra.rs");

    assert_eq!(run.resolved("ok-gate").0, Decision::Pass);
}

/// A gate whose backend crashes (non-zero exit) fails closed.
#[test]
fn a_crashing_gate_fails_closed() {
    let Some(fake) = tooling() else { return };

    let repo = TestRepo::new(&registry(&[
        Reviewer::new("crash-gate", "codex", "gate").behavior("crash")
    ]));
    let run = repo.review(fake);

    assert_eq!(run.code, Some(1));
    let (decision, gates, _cost) = run.completed();
    assert_eq!(decision, Decision::Block);
    assert_eq!(gates.blocked, 1);

    let (verdict, summary, findings, _usage) = run.resolved("crash-gate");
    assert_eq!(verdict, Decision::Block);
    assert!(
        summary.contains("did not produce a verdict"),
        "summary was {summary:?}"
    );
    assert!(!findings.is_empty());
}

/// A gate that hangs past its timeout fails closed, AND the hung child is actually
/// killed -- the runner's `kill_on_drop` is what makes a timeout real, so a child
/// that kept running (still using tools / burning tokens) would be a silent bug.
#[test]
fn a_timed_out_gate_fails_closed_and_kills_the_child() {
    let Some(fake) = tooling() else { return };

    let marker = tempfile::tempdir().unwrap();
    let marker_path = marker.path().join("agent-alive.txt");
    let marker_arg = marker_path.to_string_lossy().into_owned();

    let repo = TestRepo::new(&registry(&[Reviewer::new("slow-gate", "codex", "gate")
        .behavior("slow")
        .env("FAKE_SLEEP_MS", "1500")
        .timeout("300ms")]));

    let started = Instant::now();
    // FAKE_MARKER_FILE rides Bastion's environment and is inherited by the child;
    // the fake writes it only after its 1500ms sleep.
    let run = repo.review_base(fake, "main", &[("FAKE_MARKER_FILE", &marker_arg)]);
    let elapsed = started.elapsed();

    assert_eq!(run.code, Some(1));
    assert_eq!(run.completed().0, Decision::Block);
    let (verdict, summary, _findings, _usage) = run.resolved("slow-gate");
    assert_eq!(verdict, Decision::Block);
    assert!(summary.contains("timed out"), "summary was {summary:?}");

    // The 300ms timeout bounded the run far below the 1500ms sleep.
    assert!(
        elapsed < Duration::from_secs(15),
        "review took {elapsed:?}; the timeout did not bound the hung child"
    );

    // Wait well past the child's sleep; if it had survived the timeout it would
    // have written the marker by now.
    std::thread::sleep(Duration::from_millis(2500));
    assert!(
        !marker_path.exists(),
        "the timed-out agent child was not killed: it ran to completion and wrote the marker"
    );
}

/// Advisors fail open: an advisor that crashes -- and even one that returns a
/// clean block -- never holds up the merge.
#[test]
fn failing_or_blocking_advisors_never_block() {
    let Some(fake) = tooling() else { return };

    let repo = TestRepo::new(&registry(&[
        Reviewer::new("the-gate", "codex", "gate").behavior("pass"),
        Reviewer::new("crashy-advisor", "claude-code", "advisor").behavior("crash"),
        Reviewer::new("blocky-advisor", "codex", "advisor").behavior("block"),
    ]));
    let run = repo.review(fake);

    assert!(run.exited_zero(), "stderr:\n{}", run.stderr);
    let (decision, gates, _cost) = run.completed();
    assert_eq!(decision, Decision::Pass);
    // Only the one gate is tallied; advisors never count toward the gate total.
    assert_eq!(gates.total, 1);
    assert_eq!(gates.passed, 1);
    assert_eq!(run.resolved_count(), 3);
}

/// An advisor that hangs past its timeout fails open (skipped), not closed.
#[test]
fn a_timed_out_advisor_is_skipped_not_blocked() {
    let Some(fake) = tooling() else { return };

    let repo = TestRepo::new(&registry(&[
        Reviewer::new("the-gate", "codex", "gate").behavior("pass"),
        Reviewer::new("slow-advisor", "codex", "advisor")
            .behavior("slow")
            .env("FAKE_SLEEP_MS", "30000")
            .timeout("300ms"),
    ]));
    let run = repo.review(fake);

    assert!(run.exited_zero(), "stderr:\n{}", run.stderr);
    let (decision, gates, _cost) = run.completed();
    assert_eq!(decision, Decision::Pass);
    assert_eq!(gates.total, 1);
}

/// The single-reprompt recovery path works end to end on both backends.
#[test]
fn the_reprompt_recovery_path_works_end_to_end() {
    let Some(fake) = tooling() else { return };

    let repo = TestRepo::new(&registry(&[
        Reviewer::new("codex-recover", "codex", "gate").behavior("reprompt-recover"),
        Reviewer::new("claude-recover", "claude-code", "gate").behavior("reprompt-recover"),
    ]));
    let run = repo.review(fake);

    assert!(run.exited_zero(), "stderr:\n{}", run.stderr);
    let (decision, gates, _cost) = run.completed();
    assert_eq!(decision, Decision::Pass);
    assert_eq!(gates.passed, 2);
    assert_eq!(run.resolved("codex-recover").0, Decision::Pass);
    assert_eq!(run.resolved("claude-recover").0, Decision::Pass);
}

/// A gate that never produces a parseable verdict, even after the reprompt, fails
/// closed rather than being silently dropped.
#[test]
fn a_persistently_malformed_gate_fails_closed() {
    let Some(fake) = tooling() else { return };

    let repo = TestRepo::new(&registry(&[Reviewer::new(
        "garbage-gate",
        "claude-code",
        "gate",
    )
    .behavior("malformed")]));
    let run = repo.review(fake);

    assert_eq!(run.code, Some(1));
    assert_eq!(run.completed().0, Decision::Block);
    let (verdict, summary, _, _) = run.resolved("garbage-gate");
    assert_eq!(verdict, Decision::Block);
    assert!(summary.contains("did not produce a verdict"));
}

/// An internally-inconsistent verdict (a `block` with no blocking finding) is
/// rejected, reprompted, and -- since it stays inconsistent -- fails closed. The
/// gate never trusts a self-contradictory verdict.
#[test]
fn an_inconsistent_verdict_gate_fails_closed() {
    let Some(fake) = tooling() else { return };

    let repo = TestRepo::new(&registry(&[Reviewer::new(
        "inconsistent-gate",
        "codex",
        "gate",
    )
    .behavior("inconsistent")]));
    let run = repo.review(fake);

    assert_eq!(run.code, Some(1));
    assert_eq!(run.completed().0, Decision::Block);
    assert_eq!(run.resolved("inconsistent-gate").0, Decision::Block);
}

/// The unwired Pi backend fails closed for a gate: dispatch bails, the runner
/// turns that into a block, and the failure reason surfaces.
#[test]
fn the_unwired_pi_backend_fails_closed() {
    let Some(fake) = tooling() else { return };

    let repo = TestRepo::new(&registry(&[
        Reviewer::new("pi-gate", "pi", "gate").behavior("pass")
    ]));
    let run = repo.review(fake);

    assert_eq!(run.code, Some(1));
    assert_eq!(run.completed().0, Decision::Block);
    let (_verdict, _summary, findings, _usage) = run.resolved("pi-gate");
    assert!(
        findings.iter().any(|f| f.detail.contains("pi backend")),
        "expected the pi-not-wired reason to surface; findings: {findings:?}"
    );
}

// ---------------------------------------------------------------------------
// Accounting, env propagation, and concurrency.
// ---------------------------------------------------------------------------

/// Reported cost is summed across every reviewer that returned a verdict, across
/// both backends, exactly; per-reviewer token usage also surfaces on the stream.
#[test]
fn cost_and_token_usage_are_reported_across_backends() {
    let Some(fake) = tooling() else { return };

    let repo = TestRepo::new(&registry(&[
        Reviewer::new("c1", "claude-code", "gate")
            .behavior("pass")
            .env("FAKE_COST_CENTS", "5")
            .env("FAKE_TOKENS_IN", "1200")
            .env("FAKE_TOKENS_OUT", "80"),
        Reviewer::new("c2", "codex", "gate")
            .behavior("pass")
            .env("FAKE_COST_CENTS", "10")
            .env("FAKE_TOKENS_IN", "900")
            .env("FAKE_TOKENS_OUT", "40"),
        Reviewer::new("c3", "codex", "advisor")
            .behavior("pass")
            .env("FAKE_COST_CENTS", "7"),
    ]));
    let run = repo.review(fake);

    assert!(run.exited_zero(), "stderr:\n{}", run.stderr);
    let (_decision, _gates, cost) = run.completed();
    assert_eq!(cost, Money::from_cents(22));

    // Per-reviewer token usage is parsed from each backend's native shape.
    let claude_usage = run.resolved("c1").3.expect("claude usage reported");
    assert_eq!(claude_usage.tokens_in, 1200);
    assert_eq!(claude_usage.tokens_out, 80);
    let codex_usage = run.resolved("c2").3.expect("codex usage reported");
    assert_eq!(codex_usage.tokens_in, 900);
    assert_eq!(codex_usage.tokens_out, 40);
}

/// Reviewer `env` is propagated into the agent child and `${...}` inputs are
/// interpolated into the prompt before the agent sees it. The fake asserts the
/// interpolated marker arrived (on both backends) and fails closed if it did not,
/// so a regression in propagation or interpolation turns this test red.
#[test]
fn env_propagation_and_input_interpolation_reach_the_agent() {
    let Some(fake) = tooling() else { return };

    let repo = TestRepo::new(&registry(&[
        Reviewer::new("codex-interp", "codex", "gate")
            .behavior("pass")
            .input("preview_url", "http://preview.example/xyz")
            .prompt("Test against ${preview_url} thoroughly.")
            .env("FAKE_EXPECT_PROMPT_CONTAINS", "http://preview.example/xyz"),
        Reviewer::new("claude-interp", "claude-code", "gate")
            .behavior("pass")
            .input("ticket", "ABC-4242")
            .prompt("Review for ticket ${ticket} carefully.")
            .env("FAKE_EXPECT_PROMPT_CONTAINS", "ABC-4242"),
    ]));
    let run = repo.review(fake);

    assert!(run.exited_zero(), "stderr:\n{}", run.stderr);
    let (decision, gates, _cost) = run.completed();
    assert_eq!(decision, Decision::Pass);
    assert_eq!(gates.passed, 2);
}

/// Reviewers run concurrently, not serially. Eight reviewers each sleep two
/// seconds; awaited concurrently the run finishes in a few seconds, far under the
/// ~16s a serial execution would take.
#[test]
fn reviewers_run_concurrently_not_serially() {
    let Some(fake) = tooling() else { return };

    let names = [
        "slow0", "slow1", "slow2", "slow3", "slow4", "slow5", "slow6", "slow7",
    ];
    let reviewers: Vec<Reviewer> = names
        .iter()
        .map(|name| {
            Reviewer::new(name, "codex", "gate")
                .behavior("slow")
                .env("FAKE_SLEEP_MS", "2000")
        })
        .collect();
    let repo = TestRepo::new(&registry(&reviewers));

    let started = Instant::now();
    let run = repo.review(fake);
    let elapsed = started.elapsed();

    assert!(run.exited_zero(), "stderr:\n{}", run.stderr);
    assert_eq!(run.started_count(), 8);
    assert_eq!(run.resolved_count(), 8);
    assert_eq!(run.completed().1.passed, 8);
    // Serial would be ~16s; concurrent is ~2-3s. 10s catches serialization while
    // leaving generous headroom for a slow/loaded CI box.
    assert!(
        elapsed < Duration::from_secs(10),
        "8x2s reviewers took {elapsed:?}; they did not run concurrently"
    );
}

/// The headline stress scenario: a large, mixed registry across both backends and
/// both modes, staging passes, blocks, crashes, timeouts, reprompts, and advisory
/// noise all at once. Everything must resolve, the aggregate must block, and every
/// reviewer's artifacts must land on disk.
#[test]
fn a_large_mixed_registry_resolves_every_reviewer_and_persists() {
    let Some(fake) = tooling() else { return };

    let reviewers = vec![
        Reviewer::new("g-claude-pass", "claude-code", "gate").behavior("pass"),
        Reviewer::new("g-codex-pass", "codex", "gate").behavior("pass"),
        Reviewer::new("g-any-pass", "any", "gate").behavior("pass"),
        Reviewer::new("g-codex-block", "codex", "gate").behavior("block"),
        Reviewer::new("g-claude-crash", "claude-code", "gate").behavior("crash"),
        Reviewer::new("g-codex-timeout", "codex", "gate")
            .behavior("slow")
            .env("FAKE_SLEEP_MS", "30000")
            .timeout("500ms"),
        Reviewer::new("g-claude-recover", "claude-code", "gate").behavior("reprompt-recover"),
        Reviewer::new("a-codex-pass", "codex", "advisor").behavior("pass"),
        Reviewer::new("a-claude-block", "claude-code", "advisor").behavior("block"),
        Reviewer::new("a-codex-crash", "codex", "advisor").behavior("crash"),
    ];
    let total = reviewers.len();
    let repo = TestRepo::new(&registry(&reviewers));
    let run = repo.review(fake);

    assert_eq!(run.code, Some(1), "stderr:\n{}", run.stderr);
    let (decision, gates, _cost) = run.completed();
    assert_eq!(decision, Decision::Block);
    assert_eq!(gates.total, 7);
    assert_eq!(
        gates.blocked, 3,
        "block + crash + timeout should each block"
    );
    assert_eq!(gates.passed, 4);

    assert_eq!(run.started_count(), total);
    assert_eq!(run.resolved_count(), total);

    assert_eq!(run.resolved("g-claude-pass").0, Decision::Pass);
    assert_eq!(run.resolved("g-claude-recover").0, Decision::Pass);
    assert_eq!(run.resolved("g-codex-block").0, Decision::Block);

    let layout = repo.layout();
    let runs = store::list_runs(&layout).unwrap();
    assert_eq!(runs.len(), 1);
    let run_id = &runs[0].run;
    for reviewer in &reviewers {
        assert!(
            layout.verdict(run_id, reviewer.name).exists(),
            "missing verdict.json for {}",
            reviewer.name
        );
        assert!(
            layout.meta(run_id, reviewer.name).exists(),
            "missing meta.json for {}",
            reviewer.name
        );
    }
}

// ---------------------------------------------------------------------------
// Persistence and the read-back surface.
// ---------------------------------------------------------------------------

/// What a blocking run persists round-trips faithfully: the on-disk `run.jsonl` is
/// the full ordered event stream, `verdict.json`/`meta.json` carry the structured
/// result, and `show` replays the same blocking finding the live run emitted.
#[test]
fn a_blocking_run_persists_and_replays_faithfully() {
    let Some(fake) = tooling() else { return };

    let repo = TestRepo::new(&registry(&[Reviewer::new(
        "persisted",
        "claude-code",
        "gate",
    )
    .behavior("block")
    .env("FAKE_SUMMARY", "blocked for a real reason")]));
    let run = repo.review(fake);
    assert_eq!(run.code, Some(1));

    let layout = repo.layout();
    let runs = store::list_runs(&layout).unwrap();
    assert_eq!(runs.len(), 1);
    let run_id = &runs[0].run;

    // run.jsonl is the full event stream in order. With a single reviewer the
    // exact sequence is pinned, so a reordering (e.g. resolved before started)
    // would be caught, not just a missing-event regression.
    let persisted = store::read_run(&layout, run_id).unwrap();
    let sequence: Vec<&str> = persisted.iter().map(event_kind).collect();
    assert_eq!(
        sequence,
        [
            "run.started",
            "reviewer.started",
            "reviewer.resolved",
            "run.completed"
        ],
        "persisted run.jsonl is not the expected ordered stream"
    );

    // verdict.json carries the structured verdict.
    let verdict_json = std::fs::read_to_string(layout.verdict(run_id, "persisted")).unwrap();
    let verdict: Verdict = serde_json::from_str(&verdict_json).unwrap();
    assert_eq!(verdict.decision, Decision::Block);
    assert!(
        verdict
            .findings
            .iter()
            .any(|f| f.kind == FindingKind::Blocking)
    );

    // meta.json carries the reviewer's backend/mode/trigger (ReviewerMeta is
    // private, so assert via the JSON shape).
    let meta_json = std::fs::read_to_string(layout.meta(run_id, "persisted")).unwrap();
    let meta: serde_json::Value = serde_json::from_str(&meta_json).unwrap();
    assert_eq!(meta["backend"], "claude-code");
    assert_eq!(meta["mode"], "gate");
    assert_eq!(meta["trigger"][0], "src/**/*.rs");

    // `show <run>` replays the persisted finding, proving read-back equals the run.
    let show = repo.run(fake, &["show", run_id.as_str(), "--format", "jsonl"], &[]);
    assert!(show.status.success());
    let replay = parse_events(
        &String::from_utf8_lossy(&show.stdout),
        &String::from_utf8_lossy(&show.stderr),
    );
    let replayed_finding = replay.iter().any(|e| match e {
        RunEvent::ReviewerResolved { findings, .. } => findings
            .iter()
            .any(|f| f.detail.contains("simulated blocking finding")),
        _ => false,
    });
    assert!(replayed_finding, "show did not replay the blocking finding");
}

/// The read-back surface works over a real persisted run, including the explicit
/// run-id forms of `transcript` and `show`, and the deterministic `clean --keep 0`.
#[test]
fn the_read_back_commands_work_over_a_real_run() {
    let Some(fake) = tooling() else { return };

    let repo = TestRepo::new(&registry(&[Reviewer::new("readback", "codex", "gate")
        .behavior("pass")
        .env("FAKE_SUMMARY", "a memorable summary")]));
    let review = repo.review(fake);
    assert!(review.exited_zero(), "stderr:\n{}", review.stderr);

    let run_id = store::list_runs(&repo.layout()).unwrap()[0].run.clone();

    // `runs --format jsonl` lists exactly that run.
    let runs_out = repo.run(fake, &["runs", "--format", "jsonl"], &[]);
    assert!(runs_out.status.success());
    let summaries: Vec<RunSummary> = String::from_utf8_lossy(&runs_out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("runs line is a RunSummary"))
        .collect();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].run, run_id);
    assert_eq!(summaries[0].verdict, Some(Decision::Pass));

    // `show <run> --format jsonl` re-emits the resolved verdict and the completion.
    let show_out = repo.run(fake, &["show", run_id.as_str(), "--format", "jsonl"], &[]);
    assert!(show_out.status.success());
    let show_events = parse_events(
        &String::from_utf8_lossy(&show_out.stdout),
        &String::from_utf8_lossy(&show_out.stderr),
    );
    let resolved_ok = show_events.iter().any(|e| {
        matches!(
            e,
            RunEvent::ReviewerResolved { reviewer, summary, .. }
                if reviewer == "readback" && summary == "a memorable summary"
        )
    });
    assert!(
        resolved_ok,
        "show did not re-emit the readback reviewer with its summary"
    );
    assert!(
        show_events
            .iter()
            .any(|e| matches!(e, RunEvent::RunCompleted { .. }))
    );

    // `transcript <run> <reviewer>` (explicit two-positional form) prints the saved
    // session, which carries this run's specific summary.
    let transcript_out = repo.run(fake, &["transcript", run_id.as_str(), "readback"], &[]);
    assert!(transcript_out.status.success());
    let transcript = String::from_utf8_lossy(&transcript_out.stdout);
    assert!(
        transcript.contains("a memorable summary"),
        "transcript was {transcript:?}"
    );

    // `clean --keep 0` deterministically prunes every run.
    let clean_out = repo.run(fake, &["clean", "--keep", "0"], &[]);
    assert!(clean_out.status.success());
    assert!(store::list_runs(&repo.layout()).unwrap().is_empty());
}

/// Multiple runs in one data directory: the `latest` pointer advances, `runs`
/// lists newest-first, and `clean --keep 1` prunes exactly the older run.
#[test]
fn multiple_runs_track_latest_and_prune_oldest() {
    let Some(fake) = tooling() else { return };

    let repo = TestRepo::new(&registry(&[
        Reviewer::new("r", "codex", "gate").behavior("pass")
    ]));

    // First run, against the dirty working tree.
    let first = repo.review(fake);
    assert!(first.exited_zero(), "stderr:\n{}", first.stderr);
    let first_id = store::list_runs(&repo.layout()).unwrap()[0].run.clone();

    // Advance HEAD (so the run id changes) and introduce a genuinely new change
    // so the second run actually routes its reviewer rather than being a
    // zero-match pass. Sleep first so the second run's directory mtime is strictly
    // later than the first's, making the newest-first ordering unambiguous even on
    // coarse (1s-resolution) filesystems.
    repo.commit_all("advance");
    std::thread::sleep(Duration::from_millis(1100));
    std::fs::write(repo.path().join("src/run2.rs"), "pub fn run2() {}\n").unwrap();
    let second = repo.review(fake);
    assert!(second.exited_zero(), "stderr:\n{}", second.stderr);
    assert_eq!(
        second.completed().1.total,
        1,
        "the second run should have routed its gate, not been a zero-match pass"
    );

    let runs = store::list_runs(&repo.layout()).unwrap();
    assert_eq!(runs.len(), 2, "two distinct runs should be recorded");
    let newest_id = runs[0].run.clone();
    assert_ne!(
        newest_id, first_id,
        "the newest run should not be the first"
    );

    // `show` with no id resolves to the latest (newest) run.
    let show = repo.run(fake, &["show", "--format", "jsonl"], &[]);
    let show_events = parse_events(
        &String::from_utf8_lossy(&show.stdout),
        &String::from_utf8_lossy(&show.stderr),
    );
    assert!(show_events.iter().any(|e| e.run_id() == &newest_id));

    // `clean --keep 1` removes exactly the older run.
    let clean = repo.run(fake, &["clean", "--keep", "1"], &[]);
    assert!(clean.status.success());
    let clean_stdout = String::from_utf8_lossy(&clean.stdout);
    assert!(
        clean_stdout.contains("removed 1 run(s)"),
        "clean said: {clean_stdout}"
    );
    assert!(
        clean_stdout.contains(first_id.as_str()),
        "clean should name the older run"
    );
    let remaining = store::list_runs(&repo.layout()).unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].run, newest_id);
}

/// A changeset that triggers no reviewer is an honest, persisted pass.
#[test]
fn a_changeset_that_triggers_no_reviewer_is_a_clean_pass() {
    let Some(fake) = tooling() else { return };

    // This reviewer only triggers on docs; the dirty tree is all under src/.
    let repo = TestRepo::new(
        "reviewers:\n  - name: docs-only\n    trigger: [docs/**]\n    mode: gate\n    backend: codex\n    prompt: docs review\n",
    );
    let run = repo.review(fake);

    assert!(run.exited_zero(), "stderr:\n{}", run.stderr);
    let (decision, gates, _cost) = run.completed();
    assert_eq!(decision, Decision::Pass);
    assert_eq!(gates.total, 0);
    assert_eq!(run.resolved_count(), 0);

    let runs = store::list_runs(&repo.layout()).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].reviewers, 0);
}

// ---------------------------------------------------------------------------
// Human output, error paths, and the standalone subcommand.
// ---------------------------------------------------------------------------

/// The default (human) output format renders a readable report and still maps a
/// block to a non-zero exit. Human output is the default a person sees, yet every
/// other scenario uses `--format jsonl`, so this pins the render path directly.
#[test]
fn human_output_renders_and_still_gates() {
    let Some(fake) = tooling() else { return };

    let repo = TestRepo::new(&registry(&[
        Reviewer::new("hpass", "codex", "gate").behavior("pass"),
        Reviewer::new("hblock", "claude-code", "gate")
            .behavior("block")
            .env("FAKE_SUMMARY", "a human readable block"),
    ]));
    // No --format flag: defaults to human.
    let output = repo.run(fake, &["review", "--base", "main"], &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(
        output.status.code(),
        Some(1),
        "a block must still exit 1 in human mode"
    );
    assert!(stdout.contains("PASS "), "missing PASS marker:\n{stdout}");
    assert!(
        stdout.contains("BLOCK hblock: a human readable block"),
        "missing block line:\n{stdout}"
    );
    assert!(
        stdout.contains("[blocking] src/extra.rs:1-1: simulated blocking finding"),
        "missing rendered finding:\n{stdout}"
    );
    assert!(
        stdout.contains("run complete"),
        "missing completion line:\n{stdout}"
    );
}

/// A missing reviewer registry is a hard error (a non-zero exit with a message),
/// not a fail-closed block and not a silent pass: nothing is persisted.
#[test]
fn a_missing_registry_is_a_hard_error() {
    let Some(fake) = tooling() else { return };

    let repo = TestRepo::without_registry();
    let output = repo.run(
        fake,
        &["review", "--base", "main", "--format", "jsonl"],
        &[],
    );

    assert_ne!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("run.completed"),
        "no run should be reported"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no reviewer registry found"),
        "stderr:\n{stderr}"
    );
    assert!(store::list_runs(&repo.layout()).unwrap().is_empty());
}

/// An invalid registry (here, duplicate reviewer names) is a hard error surfaced
/// to the user, never swallowed into a pass.
#[test]
fn an_invalid_registry_is_a_hard_error() {
    let Some(fake) = tooling() else { return };

    let repo = TestRepo::new(
        "reviewers:\n  - name: dup\n    trigger: [src/**]\n    mode: gate\n    backend: codex\n    prompt: one\n  - name: dup\n    trigger: [src/**]\n    mode: gate\n    backend: codex\n    prompt: two\n",
    );
    let output = repo.run(
        fake,
        &["review", "--base", "main", "--format", "jsonl"],
        &[],
    );

    assert_ne!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("run.completed"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("duplicate reviewer name"),
        "stderr:\n{stderr}"
    );
    assert!(store::list_runs(&repo.layout()).unwrap().is_empty());
}

/// A base that does not resolve is a hard error (git fails), not a block.
#[test]
fn an_unresolvable_base_is_a_hard_error() {
    let Some(fake) = tooling() else { return };

    let repo = TestRepo::new(&registry(&[
        Reviewer::new("g", "codex", "gate").behavior("pass")
    ]));
    let output = repo.run(
        fake,
        &[
            "review",
            "--base",
            "does-not-exist-branch",
            "--format",
            "jsonl",
        ],
        &[],
    );

    assert_ne!(output.status.code(), Some(0));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("run.completed"));
    assert!(store::list_runs(&repo.layout()).unwrap().is_empty());
}

/// Read-back commands on an empty data directory error cleanly, and unknown run /
/// reviewer ids report a clear not-found error rather than succeeding.
#[test]
fn read_back_errors_are_clear() {
    let Some(fake) = tooling() else { return };

    let repo = TestRepo::new(&registry(&[
        Reviewer::new("r", "codex", "gate").behavior("pass")
    ]));

    // Empty data dir: nothing recorded yet.
    let show_empty = repo.run(fake, &["show"], &[]);
    assert_ne!(show_empty.status.code(), Some(0));
    let empty_stderr = String::from_utf8_lossy(&show_empty.stderr);
    assert!(
        empty_stderr.contains("no runs recorded yet"),
        "stderr:\n{empty_stderr}"
    );

    // After a real run, unknown ids are not-found errors.
    let run = repo.review(fake);
    assert!(run.exited_zero());
    let run_id = store::list_runs(&repo.layout()).unwrap()[0].run.clone();

    let bad_run = repo.run(fake, &["show", "no-such-run"], &[]);
    assert_ne!(bad_run.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&bad_run.stderr).contains("no such run"));

    let bad_reviewer = repo.run(
        fake,
        &["transcript", run_id.as_str(), "no-such-reviewer"],
        &[],
    );
    assert_ne!(bad_reviewer.status.code(), Some(0));
    let bad_reviewer_stderr = String::from_utf8_lossy(&bad_reviewer.stderr);
    assert!(
        bad_reviewer_stderr.contains("no saved transcript"),
        "stderr:\n{bad_reviewer_stderr}"
    );
}

/// `github codeowners` is a standalone subcommand (no repo/git/agent needed): it
/// prints the governance block, and requires at least one `--owner`.
#[test]
fn github_codeowners_emits_the_policy_block() {
    let Some(fake) = tooling() else { return };
    // Reuse a repo only for a valid working directory; the command reads nothing.
    let repo = TestRepo::new(&registry(&[
        Reviewer::new("r", "codex", "gate").behavior("pass")
    ]));

    let ok = repo.run(
        fake,
        &[
            "github",
            "codeowners",
            "--owner",
            "@acme/platform",
            "--owner",
            "@jess",
        ],
        &[],
    );
    assert!(ok.status.success());
    let stdout = String::from_utf8_lossy(&ok.stdout);
    assert!(
        stdout.contains("/bastion/ @acme/platform @jess"),
        "stdout:\n{stdout}"
    );
    assert!(stdout.contains("require human review"), "stdout:\n{stdout}");

    // The owner argument is required.
    let missing = repo.run(fake, &["github", "codeowners"], &[]);
    assert_ne!(missing.status.code(), Some(0));
}

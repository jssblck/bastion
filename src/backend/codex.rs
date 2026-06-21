//! The Codex backend.
//!
//! Translates a reviewer's execution profile into a headless `codex exec`-style
//! invocation, runs it through the injectable [`CommandRunner`] seam, and parses
//! the agent's final structured output into a [`Verdict`] (`docs/DESIGN.md`,
//! "Agent backends").
//!
//! # The Codex invocation contract
//!
//! Bastion drives Codex in its headless `exec` mode and asks for a machine
//! readable event stream (`codex exec --json`). Each line of stdout is a JSON
//! event; Bastion reconstructs the transcript from those events, takes the final
//! agent message as the reviewer's structured output, and reads token/cost
//! accounting from the usage event when Codex reports it. The reviewer's prompt
//! (with [`inputs`](crate::reviewer::Reviewer::inputs) interpolated) is passed as
//! the task, with a trailing instruction pinning the verdict schema. Reviewer
//! [`env`](crate::reviewer::Reviewer::env) is propagated into the child process.
//!
//! # Fail-closed parsing
//!
//! If the final message does not carry a schema-conforming verdict, the backend
//! re-prompts the same session once for *just* the structured output (per
//! `docs/DESIGN.md`), then gives up with an error. The runner turns that error
//! into a fail-closed `block` for gates; this backend never invents a verdict.

use std::ffi::{OsStr, OsString};
use std::path::Path;

use color_eyre::eyre::{Result, bail, eyre};
use serde::Deserialize;

use crate::reviewer::{self};
use crate::verdict::{Money, Usage, Verdict};

use super::command::{CommandRunner, CommandSpec, resolve_program};
use super::{Backend, ReviewOutcome, ReviewRequest};

/// Environment variable that overrides the `codex` program path (tests point this
/// at a fake executable; deployments can pin a specific binary).
pub const PROGRAM_ENV: &str = "BASTION_CODEX_BIN";

/// The default program name, resolved on `PATH` when [`PROGRAM_ENV`] is unset.
pub const DEFAULT_PROGRAM: &str = "codex";

/// The Codex agent backend.
///
/// Generic over the [`CommandRunner`] so production wires a real subprocess while
/// tests drive a fake executable through the identical path.
#[derive(Debug, Clone)]
pub struct CodexBackend<R> {
    runner: R,
    program: OsString,
    /// Leading args before the prompt for a first-pass run (default
    /// `["exec", "--json"]`).
    base_args: Vec<String>,
    /// Leading args that resume a session by id, used for the reprompt so the
    /// schema is requested in the *same* session (default
    /// `["exec", "resume", "--json"]`, followed by the thread id).
    resume_args: Vec<String>,
}

impl<R: CommandRunner> CodexBackend<R> {
    /// Build a backend over `runner`, resolving the `codex` program from
    /// [`PROGRAM_ENV`] (falling back to [`DEFAULT_PROGRAM`] on `PATH`).
    #[must_use]
    pub fn new(runner: R) -> Self {
        Self::with_program(runner, resolve_program(DEFAULT_PROGRAM, PROGRAM_ENV))
    }

    /// Build a backend over `runner` with an explicit program path and the
    /// default `exec` argument layout.
    #[must_use]
    pub fn with_program(runner: R, program: impl Into<OsString>) -> Self {
        Self {
            runner,
            program: program.into(),
            base_args: vec!["exec".to_string(), "--json".to_string()],
            resume_args: vec![
                "exec".to_string(),
                "resume".to_string(),
                "--json".to_string(),
            ],
        }
    }

    /// Build a backend with full control over the program and argument layout.
    /// Used by tests that drive a fake executable through a launcher.
    #[must_use]
    pub fn with_command(
        runner: R,
        program: impl Into<OsString>,
        base_args: Vec<String>,
        resume_args: Vec<String>,
    ) -> Self {
        Self {
            runner,
            program: program.into(),
            base_args,
            resume_args,
        }
    }

    /// The first-pass invocation: the full review prompt with the schema appended.
    fn first_spec(&self, request: &ReviewRequest<'_>, prompt: &str) -> CommandSpec {
        self.build_spec(request, self.base_args.clone(), prompt)
    }

    /// The reprompt invocation: resume `thread_id` in the same session when one is
    /// known, else fall back to a fresh first-pass invocation.
    fn reprompt_spec(
        &self,
        request: &ReviewRequest<'_>,
        thread_id: Option<&str>,
        prompt: &str,
    ) -> CommandSpec {
        match thread_id {
            Some(id) => {
                let mut args = self.resume_args.clone();
                args.push(id.to_string());
                self.build_spec(request, args, prompt)
            }
            None => self.first_spec(request, prompt),
        }
    }

    /// Assemble a [`CommandSpec`] from leading `args`, appending `prompt` as the
    /// final positional argument and forwarding the reviewer's env and checkout.
    fn build_spec(
        &self,
        request: &ReviewRequest<'_>,
        leading: Vec<String>,
        prompt: &str,
    ) -> CommandSpec {
        let mut spec = CommandSpec::new(self.program.clone(), request.repo_root);
        for arg in &leading {
            spec.arg(arg);
        }
        spec.arg(prompt);
        for (key, value) in &request.reviewer.env {
            spec.env.insert(key.clone(), value.clone());
        }
        spec
    }

    /// Run one Codex invocation and parse its event stream into a session.
    async fn run_once(&self, spec: &CommandSpec) -> Result<CodexSession> {
        let output = self.runner.run(spec).await?;
        if !output.success() {
            bail!(
                "codex exited with status {}: {}",
                output
                    .code
                    .map_or_else(|| "signal".to_string(), |c| c.to_string()),
                output.stderr.trim(),
            );
        }
        CodexSession::parse(&output.stdout)
    }
}

impl<R: CommandRunner> Backend for CodexBackend<R> {
    fn id(&self) -> reviewer::Backend {
        reviewer::Backend::Codex
    }

    async fn review(&self, request: &ReviewRequest<'_>) -> Result<ReviewOutcome> {
        let prompt = build_prompt(request);

        // First pass: the full review with the schema instruction appended.
        let first = self.first_spec(request, &prompt);
        let session = self.run_once(&first).await?;

        if let Some(verdict) = session.parse_verdict() {
            return Ok(outcome(verdict, session, None));
        }

        // The agent's final message was not a schema-conforming verdict. Per
        // DESIGN.md, re-run the *same session* asking for just the structured
        // output, then fail closed. Resume by thread id when Codex reported one;
        // when resuming, the new turn is only the reprompt suffix (the session
        // already holds the review). Without a thread id we fall back to a fresh
        // session and must re-send the full prompt.
        let reprompt_text = match session.thread_id.as_deref() {
            Some(_) => REPROMPT_SUFFIX.to_string(),
            None => format!("{prompt}\n\n{REPROMPT_SUFFIX}"),
        };
        let retry = self.reprompt_spec(request, session.thread_id.as_deref(), &reprompt_text);
        let retry_session = self.run_once(&retry).await?;

        match retry_session.parse_verdict() {
            Some(verdict) => Ok(outcome(verdict, retry_session, Some(&session))),
            None => Err(eyre!(
                "codex did not emit a schema-conforming verdict after one reprompt; \
                 failing closed. final message was:\n{}",
                retry_session
                    .final_message()
                    .unwrap_or("(no agent message)")
            )),
        }
    }
}

/// Assemble a [`ReviewOutcome`] from a parsed verdict and the session it came
/// from, optionally prepending an earlier session's transcript (the original
/// review, when the verdict was recovered on a reprompt).
fn outcome(verdict: Verdict, session: CodexSession, prior: Option<&CodexSession>) -> ReviewOutcome {
    let transcript = match prior {
        Some(prior) if !prior.transcript.is_empty() => {
            format!("{}\n{}", prior.transcript.trim_end(), session.transcript)
        }
        _ => session.transcript,
    };
    ReviewOutcome {
        verdict,
        // Prefer the reprompt session's usage, falling back to the original's so
        // accounting is not lost when the resume turn reports none.
        usage: session.usage.or_else(|| prior.and_then(|p| p.usage)),
        transcript: Some(transcript),
    }
}

/// The instruction appended to every review prompt pinning the verdict schema.
const SCHEMA_INSTRUCTION: &str = "\
When you are done reviewing, end your final message with the structured verdict \
and nothing after it, as a fenced YAML code block matching exactly this schema:\n\
```yaml\n\
verdict: pass | block        # the authoritative gate decision\n\
summary: \"...\"               # one-line human-friendly summary\n\
findings:                    # may be empty; a block must carry >=1 blocking finding\n\
  - kind: blocking | optional\n\
    path: relative/file/path\n\
    line_start: 1\n\
    line_end: 1\n\
    detail: \"...\"\n\
```";

/// The instruction used when re-prompting for just the structured output.
const REPROMPT_SUFFIX: &str = "\
Your previous response did not contain a valid structured verdict. Do not perform \
any further work. Reply with ONLY the fenced YAML verdict block described above, \
for the review you already completed, and nothing else.";

/// Build the full prompt handed to Codex for `request`: a one-line context
/// preamble naming the base branch, the interpolated review instruction, and the
/// schema instruction.
fn build_prompt(request: &ReviewRequest<'_>) -> String {
    let reviewer = request.reviewer;
    let interpolated = super::interpolate(&reviewer.prompt, &reviewer.inputs);
    format!(
        "You are reviewing a changeset computed against the base branch \
         `{base}`. Use `git diff {base}...HEAD` to see exactly what changed.\n\n\
         {interpolated}\n\n{SCHEMA_INSTRUCTION}",
        base = request.base,
    )
}

/// A parsed Codex `exec --json` session: the reconstructed transcript, the final
/// agent message, usage when reported, and the thread id used to resume.
#[derive(Debug, Clone)]
struct CodexSession {
    /// The human-readable transcript, reconstructed from the event stream.
    transcript: String,
    /// The text of the final agent message, if any.
    last_message: Option<String>,
    /// Token/cost accounting, when Codex reported it.
    usage: Option<Usage>,
    /// The thread/session id, when Codex reported it, for resuming on a reprompt.
    thread_id: Option<String>,
}

impl CodexSession {
    /// Parse a Codex `exec --json` stdout stream (JSON-lines) into a session.
    ///
    /// Unknown event types are tolerated, so it survives Codex adding events.
    /// Non-JSON lines are kept in the transcript verbatim (some builds interleave
    /// plain log lines).
    ///
    /// # Errors
    ///
    /// Returns an error if the stream carried neither a recognized event nor any
    /// other output to record.
    fn parse(stdout: &str) -> Result<Self> {
        let mut acc = SessionAccumulator::default();

        for line in stdout.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<CodexEvent>(trimmed) {
                Ok(event) => {
                    acc.saw_event = true;
                    event.fold_into(&mut acc);
                }
                Err(_) => {
                    // Not a recognized JSON event line; keep it in the transcript.
                    acc.transcript.push_str(line);
                    acc.transcript.push('\n');
                }
            }
        }

        if !acc.saw_event && acc.transcript.is_empty() {
            bail!("codex produced no output to parse");
        }

        Ok(Self {
            transcript: acc.transcript,
            last_message: acc.last_message,
            usage: acc.usage,
            thread_id: acc.thread_id,
        })
    }

    /// The final agent message text, if the session produced one.
    fn final_message(&self) -> Option<&str> {
        self.last_message.as_deref()
    }

    /// Parse the final agent message into a [`Verdict`], if it carries one.
    fn parse_verdict(&self) -> Option<Verdict> {
        let message = self.last_message.as_deref()?;
        extract_verdict(message)
    }
}

/// Extract a schema-conforming, internally-consistent [`Verdict`] from `message`.
///
/// Tries the last fenced code block, then the entire message, parsing each as
/// YAML (a superset of JSON, so JSON verdicts parse too). A verdict that parses
/// but is inconsistent (e.g. a `block` with no blocking finding) is rejected so
/// the caller fails closed rather than trusting a malformed gate decision.
fn extract_verdict(message: &str) -> Option<Verdict> {
    for candidate in verdict_candidates(message) {
        if let Ok(verdict) = serde_yaml_ng::from_str::<Verdict>(&candidate)
            && verdict.is_consistent()
        {
            return Some(verdict);
        }
    }
    None
}

/// Candidate verdict texts to attempt parsing, most-specific first: each fenced
/// code block (last to first), then the whole message.
fn verdict_candidates(message: &str) -> Vec<String> {
    let mut candidates: Vec<String> = fenced_blocks(message);
    candidates.reverse();
    candidates.push(message.to_string());
    candidates
}

/// Extract the contents of every fenced code block in `message`, in source
/// order, following CommonMark fence matching: a fence opens with three or more
/// backticks and only closes on a line with at least as many backticks. An
/// optional info string (e.g. ```` ```yaml ````) on the opening fence is dropped.
///
/// Tracking the opening fence length means a longer outer fence can wrap a block
/// that itself contains shorter ` ``` ` runs without being closed prematurely.
fn fenced_blocks(message: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut open_len: Option<usize> = None;
    let mut current = String::new();
    for line in message.lines() {
        let ticks = leading_backticks(line);
        match open_len {
            // Inside a block: close only on a fence at least as long, that is
            // *only* backticks (a closing fence carries no info string).
            Some(len) if ticks >= len && is_bare_fence(line) => {
                blocks.push(std::mem::take(&mut current));
                open_len = None;
            }
            Some(_) => {
                current.push_str(line);
                current.push('\n');
            }
            // Outside a block: a run of three or more backticks opens one.
            None if ticks >= 3 => {
                open_len = Some(ticks);
                current.clear();
            }
            None => {}
        }
    }
    blocks
}

/// The number of leading backticks on `line` after stripping indentation.
fn leading_backticks(line: &str) -> usize {
    line.trim_start().chars().take_while(|&c| c == '`').count()
}

/// Whether `line` (after indentation) is only backticks — a valid closing fence.
fn is_bare_fence(line: &str) -> bool {
    let trimmed = line.trim_start();
    !trimmed.is_empty() && trimmed.chars().all(|c| c == '`')
}

/// One event in a Codex `exec --json` stream.
///
/// Modeled as a tagged union over the `type` field. This matches the current
/// Codex threaded event schema (`thread.started`, `item.completed`,
/// `turn.completed`) and also tolerates the older flat schema (`agent_message`,
/// `token_count`) so the backend works across Codex versions. Anything else
/// deserializes to [`CodexEvent::Other`] and is ignored beyond the transcript.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum CodexEvent {
    /// The session opened; carries the thread id used to resume for the reprompt.
    #[serde(rename = "thread.started")]
    ThreadStarted {
        /// The thread/session identifier.
        thread_id: String,
    },
    /// A completed conversation item (current schema). Agent-message items carry
    /// the text; the final one is the structured verdict.
    #[serde(rename = "item.completed")]
    ItemCompleted {
        /// The completed item.
        item: CodexItem,
    },
    /// A turn finished; carries token usage in the current schema.
    #[serde(rename = "turn.completed")]
    TurnCompleted {
        /// Token usage for the turn.
        usage: CodexUsage,
    },
    /// A message from the agent (legacy flat schema).
    #[serde(rename = "agent_message")]
    AgentMessage {
        /// The message text.
        message: String,
    },
    /// A reasoning/thinking trace (legacy flat schema); transcript only.
    #[serde(rename = "agent_reasoning")]
    AgentReasoning {
        /// The reasoning text.
        text: String,
    },
    /// Token and cost accounting (legacy flat schema).
    #[serde(rename = "token_count")]
    TokenCount {
        /// Input tokens consumed.
        #[serde(default)]
        input_tokens: u64,
        /// Output tokens produced.
        #[serde(default)]
        output_tokens: u64,
        /// Session cost in US dollars, when Codex computes it.
        #[serde(default)]
        cost_usd: Option<f64>,
    },
    /// Any other event; ignored beyond being recorded as "seen".
    #[serde(other)]
    Other,
}

/// A completed conversation item in the current Codex schema.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum CodexItem {
    /// An assistant message; the final one is the verdict.
    AgentMessage {
        /// The message text.
        text: String,
    },
    /// A reasoning trace; transcript only.
    Reasoning {
        /// The reasoning text.
        text: String,
    },
    /// Any other item kind; ignored.
    #[serde(other)]
    Other,
}

/// Token usage as carried by a `turn.completed` event in the current schema.
#[derive(Debug, Deserialize)]
struct CodexUsage {
    /// Input tokens consumed.
    #[serde(default)]
    input_tokens: u64,
    /// Output tokens produced.
    #[serde(default)]
    output_tokens: u64,
    /// Session cost in US dollars, when Codex reports it.
    #[serde(default)]
    cost_usd: Option<f64>,
}

/// The mutable accumulator a stream of [`CodexEvent`]s folds into.
#[derive(Debug, Default)]
struct SessionAccumulator {
    transcript: String,
    last_message: Option<String>,
    usage: Option<Usage>,
    thread_id: Option<String>,
    saw_event: bool,
}

impl SessionAccumulator {
    /// Record a message in the transcript and as the latest agent message.
    fn record_message(&mut self, message: String) {
        self.transcript.push_str(&message);
        self.transcript.push('\n');
        self.last_message = Some(message);
    }

    /// Record reasoning text in the transcript only.
    fn record_reasoning(&mut self, text: &str) {
        self.transcript.push_str(text);
        self.transcript.push('\n');
    }

    /// Record token/cost usage.
    fn record_usage(&mut self, input_tokens: u64, output_tokens: u64, cost_usd: Option<f64>) {
        self.usage = Some(Usage {
            tokens_in: input_tokens,
            tokens_out: output_tokens,
            cost_usd: cost_usd.map_or_else(Money::default, super::money_from_dollars),
        });
    }
}

impl CodexEvent {
    /// Fold this event into `acc`.
    fn fold_into(self, acc: &mut SessionAccumulator) {
        match self {
            CodexEvent::ThreadStarted { thread_id } => acc.thread_id = Some(thread_id),
            CodexEvent::ItemCompleted { item } => match item {
                CodexItem::AgentMessage { text } => acc.record_message(text),
                CodexItem::Reasoning { text } => acc.record_reasoning(&text),
                CodexItem::Other => {}
            },
            CodexEvent::TurnCompleted { usage } => {
                acc.record_usage(usage.input_tokens, usage.output_tokens, usage.cost_usd);
            }
            CodexEvent::AgentMessage { message } => acc.record_message(message),
            CodexEvent::AgentReasoning { text } => acc.record_reasoning(&text),
            CodexEvent::TokenCount {
                input_tokens,
                output_tokens,
                cost_usd,
            } => acc.record_usage(input_tokens, output_tokens, cost_usd),
            CodexEvent::Other => {}
        }
    }
}

/// Whether `program` resolves to an executable on `PATH` or as a direct path.
///
/// Used by real-binary tests to detect-and-skip when the Codex CLI is absent.
#[must_use]
pub fn program_available(program: impl AsRef<OsStr>) -> bool {
    let program = program.as_ref();
    let path = Path::new(program);
    if path.is_absolute() || path.components().count() > 1 {
        return path.is_file();
    }
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| {
        let candidate = dir.join(program);
        candidate.is_file()
            || candidate.with_extension("exe").is_file()
            || candidate.with_extension("cmd").is_file()
            || candidate.with_extension("bat").is_file()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;
    use std::sync::Mutex;

    use crate::backend::command::{CommandOutput, SystemCommandRunner};
    use crate::event::RunId;
    use crate::reviewer::{Capabilities, Mode, Reviewer};
    use crate::verdict::{Decision, FindingKind};

    /// A [`CommandRunner`] that returns canned outputs in sequence and records the
    /// command specs it was handed, so tests can assert on the translated call.
    #[derive(Debug, Default)]
    struct FakeRunner {
        responses: Mutex<std::collections::VecDeque<CommandOutput>>,
        seen: Mutex<Vec<CommandSpec>>,
    }

    impl FakeRunner {
        fn new(responses: impl IntoIterator<Item = CommandOutput>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().collect()),
                seen: Mutex::new(Vec::new()),
            }
        }

        fn specs(&self) -> Vec<CommandSpec> {
            self.seen.lock().unwrap().clone()
        }
    }

    impl CommandRunner for FakeRunner {
        async fn run(&self, spec: &CommandSpec) -> Result<CommandOutput> {
            self.seen.lock().unwrap().push(spec.clone());
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| eyre!("FakeRunner ran out of canned responses"))
        }
    }

    /// The arguments of a recorded spec as plain strings, for assertions.
    fn args_of(spec: &CommandSpec) -> Vec<String> {
        spec.args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn ok_output(stdout: impl Into<String>) -> CommandOutput {
        CommandOutput {
            code: Some(0),
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }

    /// A JSON-lines stream in the legacy flat schema.
    fn stream(messages: &[&str], usage: Option<(u64, u64, f64)>) -> String {
        let mut out = String::new();
        for message in messages {
            let event = serde_json::json!({ "type": "agent_message", "message": message });
            out.push_str(&serde_json::to_string(&event).unwrap());
            out.push('\n');
        }
        if let Some((tin, tout, cost)) = usage {
            let event = serde_json::json!({
                "type": "token_count",
                "input_tokens": tin,
                "output_tokens": tout,
                "cost_usd": cost,
            });
            out.push_str(&serde_json::to_string(&event).unwrap());
            out.push('\n');
        }
        out
    }

    /// A JSON-lines stream in the current threaded schema.
    fn threaded_stream(
        thread_id: &str,
        messages: &[&str],
        usage: Option<(u64, u64, f64)>,
    ) -> String {
        let mut out = String::new();
        let started = serde_json::json!({ "type": "thread.started", "thread_id": thread_id });
        out.push_str(&serde_json::to_string(&started).unwrap());
        out.push('\n');
        for message in messages {
            let event = serde_json::json!({
                "type": "item.completed",
                "item": { "type": "agent_message", "text": message },
            });
            out.push_str(&serde_json::to_string(&event).unwrap());
            out.push('\n');
        }
        if let Some((tin, tout, cost)) = usage {
            let event = serde_json::json!({
                "type": "turn.completed",
                "usage": { "input_tokens": tin, "output_tokens": tout, "cost_usd": cost },
            });
            out.push_str(&serde_json::to_string(&event).unwrap());
            out.push('\n');
        }
        out
    }

    fn reviewer() -> Reviewer {
        Reviewer {
            name: "demo".into(),
            trigger: vec!["**".into()],
            mode: Mode::Gate,
            backend: reviewer::Backend::Codex,
            timeout: None,
            runner: None,
            env: Default::default(),
            capabilities: Capabilities::default(),
            inputs: Default::default(),
            prompt: "Check the thing.".into(),
        }
    }

    fn request<'a>(reviewer: &'a Reviewer, run: &'a RunId, root: &'a Path) -> ReviewRequest<'a> {
        ReviewRequest {
            reviewer,
            run,
            repo_root: root,
            base: "main",
        }
    }

    async fn review_with(
        reviewer: &Reviewer,
        responses: impl IntoIterator<Item = CommandOutput>,
    ) -> (Result<ReviewOutcome>, Vec<CommandSpec>) {
        let backend = CodexBackend::with_program(FakeRunner::new(responses), DEFAULT_PROGRAM);
        let run = RunId("r-test".into());
        let root = PathBuf::from(".");
        let req = request(reviewer, &run, &root);
        let outcome = backend.review(&req).await;
        let specs = backend.runner.specs();
        (outcome, specs)
    }

    #[tokio::test]
    async fn id_is_codex() {
        let backend = CodexBackend::with_program(FakeRunner::default(), DEFAULT_PROGRAM);
        assert_eq!(backend.id(), reviewer::Backend::Codex);
    }

    #[tokio::test]
    async fn happy_path_pass_verdict_parses() {
        let message = "Looks fine.\n\n```yaml\nverdict: pass\nsummary: all good\nfindings: []\n```";
        let (outcome, specs) =
            review_with(&reviewer(), [ok_output(stream(&[message], None))]).await;
        let outcome = outcome.expect("verdict parses");
        assert_eq!(outcome.verdict.decision, Decision::Pass);
        assert_eq!(outcome.verdict.summary, "all good");
        assert!(outcome.verdict.findings.is_empty());
        assert!(outcome.usage.is_none());
        assert!(outcome.transcript.unwrap().contains("Looks fine."));
        assert_eq!(specs.len(), 1);
    }

    #[tokio::test]
    async fn happy_path_block_verdict_with_findings_parses() {
        let message = "\
Found an issue.

```yaml
verdict: block
summary: unscoped query
findings:
  - kind: blocking
    path: src/db.ts
    line_start: 10
    line_end: 12
    detail: scope by tenant_id
```";
        let (outcome, _) = review_with(&reviewer(), [ok_output(stream(&[message], None))]).await;
        let verdict = outcome.expect("verdict parses").verdict;
        assert_eq!(verdict.decision, Decision::Block);
        assert_eq!(verdict.findings.len(), 1);
        assert_eq!(verdict.findings[0].kind, FindingKind::Blocking);
        assert_eq!(verdict.findings[0].path, "src/db.ts");
        assert!(verdict.is_consistent());
    }

    #[tokio::test]
    async fn usage_is_captured_when_reported() {
        let message = "```yaml\nverdict: pass\nsummary: ok\n```";
        let (outcome, _) = review_with(
            &reviewer(),
            [ok_output(stream(&[message], Some((18204, 1560, 0.21))))],
        )
        .await;
        let usage = outcome.expect("parses").usage.expect("usage present");
        assert_eq!(usage.tokens_in, 18204);
        assert_eq!(usage.tokens_out, 1560);
        assert_eq!(usage.cost_usd, Money::from_cents(21));
    }

    #[tokio::test]
    async fn usage_absent_leaves_none() {
        let message = "```yaml\nverdict: pass\nsummary: ok\n```";
        let (outcome, _) = review_with(&reviewer(), [ok_output(stream(&[message], None))]).await;
        assert!(outcome.expect("parses").usage.is_none());
    }

    #[tokio::test]
    async fn malformed_output_triggers_one_reprompt_then_succeeds() {
        let bad = ok_output(stream(&["I reviewed it but forgot the schema."], None));
        let good = ok_output(stream(
            &["```yaml\nverdict: pass\nsummary: recovered\n```"],
            None,
        ));
        let (outcome, specs) = review_with(&reviewer(), [bad, good]).await;
        let verdict = outcome.expect("recovers on reprompt").verdict;
        assert_eq!(verdict.decision, Decision::Pass);
        assert_eq!(verdict.summary, "recovered");
        assert_eq!(specs.len(), 2);
        assert!(
            !args_of(&specs[0])
                .last()
                .unwrap()
                .contains("did not contain")
        );
        assert!(
            args_of(&specs[1])
                .last()
                .unwrap()
                .contains("ONLY the fenced YAML")
        );
    }

    #[tokio::test]
    async fn malformed_twice_fails_closed() {
        let bad1 = ok_output(stream(&["no verdict here"], None));
        let bad2 = ok_output(stream(&["still no verdict"], None));
        let (outcome, specs) = review_with(&reviewer(), [bad1, bad2]).await;
        let err = outcome.expect_err("fails closed after one reprompt");
        assert!(
            err.to_string()
                .contains("did not emit a schema-conforming verdict")
        );
        assert_eq!(specs.len(), 2);
    }

    #[tokio::test]
    async fn inconsistent_block_is_rejected_and_reprompted() {
        let inconsistent = ok_output(stream(
            &["```yaml\nverdict: block\nsummary: no reason\nfindings: []\n```"],
            None,
        ));
        let recovered = ok_output(stream(&["```yaml\nverdict: pass\nsummary: ok\n```"], None));
        let (outcome, specs) = review_with(&reviewer(), [inconsistent, recovered]).await;
        assert_eq!(outcome.expect("recovers").verdict.decision, Decision::Pass);
        assert_eq!(specs.len(), 2);
    }

    #[tokio::test]
    async fn current_threaded_schema_parses_message_and_usage() {
        let message = "```yaml\nverdict: pass\nsummary: threaded\n```";
        let stdout = threaded_stream("th-123", &[message], Some((100, 20, 0.05)));
        let (outcome, specs) = review_with(&reviewer(), [ok_output(stdout)]).await;
        let outcome = outcome.expect("threaded schema parses");
        assert_eq!(outcome.verdict.summary, "threaded");
        let usage = outcome.usage.expect("usage from turn.completed");
        assert_eq!(usage.tokens_in, 100);
        assert_eq!(usage.tokens_out, 20);
        assert_eq!(usage.cost_usd, Money::from_cents(5));
        assert_eq!(specs.len(), 1);
    }

    #[tokio::test]
    async fn reprompt_resumes_the_same_session_by_thread_id() {
        let bad = ok_output(threaded_stream("th-abc", &["no verdict yet"], None));
        let good = ok_output(threaded_stream(
            "th-abc",
            &["```yaml\nverdict: pass\nsummary: resumed\n```"],
            None,
        ));
        let (outcome, specs) = review_with(&reviewer(), [bad, good]).await;
        assert_eq!(outcome.expect("recovers").verdict.summary, "resumed");
        assert_eq!(specs.len(), 2);
        let retry = args_of(&specs[1]);
        assert_eq!(retry[0], "exec");
        assert_eq!(retry[1], "resume");
        assert!(retry.contains(&"th-abc".to_string()));
        assert!(retry.last().unwrap().contains("ONLY the fenced YAML"));
        assert!(!retry.last().unwrap().contains("Check the thing."));
    }

    #[tokio::test]
    async fn reprompt_without_thread_id_falls_back_to_a_fresh_session() {
        let bad = ok_output(stream(&["no verdict"], None));
        let good = ok_output(stream(&["```yaml\nverdict: pass\nsummary: ok\n```"], None));
        let (outcome, specs) = review_with(&reviewer(), [bad, good]).await;
        assert_eq!(outcome.expect("recovers").verdict.decision, Decision::Pass);
        let retry = args_of(&specs[1]);
        assert_eq!(retry[0], "exec");
        assert_eq!(retry[1], "--json");
        assert!(retry.last().unwrap().contains("Check the thing."));
        assert!(retry.last().unwrap().contains("ONLY the fenced YAML"));
    }

    #[tokio::test]
    async fn recovered_transcript_includes_the_original_session() {
        let bad = ok_output(threaded_stream(
            "th-x",
            &["I did a thorough review of the database layer."],
            Some((500, 40, 0.10)),
        ));
        let good = ok_output(threaded_stream(
            "th-x",
            &["```yaml\nverdict: pass\nsummary: ok\n```"],
            None,
        ));
        let (outcome, _) = review_with(&reviewer(), [bad, good]).await;
        let outcome = outcome.expect("recovers");
        let transcript = outcome.transcript.unwrap();
        assert!(transcript.contains("thorough review of the database layer"));
        assert!(transcript.contains("verdict: pass"));
        assert_eq!(outcome.usage.expect("usage carried over").tokens_in, 500);
    }

    #[tokio::test]
    async fn nonzero_exit_is_an_error() {
        let failed = CommandOutput {
            code: Some(1),
            stdout: String::new(),
            stderr: "boom".into(),
        };
        let (outcome, _) = review_with(&reviewer(), [failed]).await;
        let err = outcome.expect_err("non-zero exit errors");
        assert!(err.to_string().contains("codex exited with status 1"));
        assert!(err.to_string().contains("boom"));
    }

    #[tokio::test]
    async fn prompt_inputs_are_interpolated_and_schema_appended() {
        let mut reviewer = reviewer();
        reviewer.prompt = "Test against ${preview_url} now.".into();
        reviewer
            .inputs
            .insert("preview_url".into(), "http://localhost:3000".into());
        let (_, specs) = review_with(
            &reviewer,
            [ok_output(stream(
                &["```yaml\nverdict: pass\nsummary: ok\n```"],
                None,
            ))],
        )
        .await;
        let prompt = args_of(&specs[0]).last().unwrap().clone();
        assert!(prompt.contains("Test against http://localhost:3000 now."));
        assert!(prompt.contains("structured verdict"));
        assert!(!prompt.contains("${preview_url}"));
        assert!(prompt.contains("base branch `main`"));
    }

    #[tokio::test]
    async fn env_is_propagated_to_the_spec() {
        let mut reviewer = reviewer();
        reviewer.env.insert("PREVIEW_URL".into(), "x".into());
        let (_, specs) = review_with(
            &reviewer,
            [ok_output(stream(
                &["```yaml\nverdict: pass\nsummary: ok\n```"],
                None,
            ))],
        )
        .await;
        assert_eq!(
            specs[0].env.get("PREVIEW_URL").map(String::as_str),
            Some("x")
        );
    }

    #[tokio::test]
    async fn base_args_and_cwd_match_config() {
        let backend = CodexBackend::with_program(
            FakeRunner::new([ok_output(stream(
                &["```yaml\nverdict: pass\nsummary: ok\n```"],
                None,
            ))]),
            DEFAULT_PROGRAM,
        );
        let run = RunId("r".into());
        let root = PathBuf::from("/some/repo");
        let reviewer = reviewer();
        let req = request(&reviewer, &run, &root);
        backend.review(&req).await.expect("ok");
        let spec = &backend.runner.specs()[0];
        assert_eq!(spec.program, OsString::from(DEFAULT_PROGRAM));
        let args = args_of(spec);
        assert_eq!(args[0], "exec");
        assert_eq!(args[1], "--json");
        assert_eq!(spec.cwd, root);
    }

    // -- Pure parsing-helper unit tests ---------------------------------------

    #[test]
    fn fenced_blocks_extracts_last_block() {
        let message = "intro\n```\nfirst\n```\nmiddle\n```yaml\nsecond\n```\n";
        let blocks = fenced_blocks(message);
        assert_eq!(blocks, vec!["first\n".to_string(), "second\n".to_string()]);
    }

    #[test]
    fn fenced_blocks_respects_longer_outer_fences() {
        let message = "````markdown\nhere is an example:\n```\ninner\n```\ndone\n````\n";
        let blocks = fenced_blocks(message);
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].contains("inner"));
        assert!(blocks[0].contains("```"));
        assert!(blocks[0].contains("done"));
    }

    #[test]
    fn fenced_blocks_does_not_treat_info_string_lines_as_closers() {
        let message = "```\nverdict: pass\n```yaml not a closer\nsummary: ok\n```\n";
        let blocks = fenced_blocks(message);
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].contains("```yaml not a closer"));
        assert!(blocks[0].contains("summary: ok"));
    }

    #[test]
    fn extract_verdict_prefers_last_fenced_block() {
        let message = "\
```text
not a verdict
```
```yaml
verdict: pass
summary: chosen
```";
        let verdict = extract_verdict(message).expect("finds the verdict block");
        assert_eq!(verdict.summary, "chosen");
    }

    #[test]
    fn extract_verdict_falls_back_to_whole_message() {
        let message = "verdict: pass\nsummary: bare yaml\n";
        let verdict = extract_verdict(message).expect("parses bare yaml");
        assert_eq!(verdict.summary, "bare yaml");
    }

    #[test]
    fn parse_rejects_empty_stream() {
        let err = CodexSession::parse("   \n\n").unwrap_err();
        assert!(err.to_string().contains("no output"));
    }

    #[test]
    fn parse_keeps_non_json_lines_in_transcript() {
        let stdout = "plain log line\n{\"type\":\"agent_message\",\"message\":\"hi\"}\n";
        let session = CodexSession::parse(stdout).expect("parses");
        assert!(session.transcript.contains("plain log line"));
        assert_eq!(session.final_message(), Some("hi"));
    }

    #[test]
    fn reasoning_events_join_the_transcript_only() {
        let stdout = "\
{\"type\":\"agent_reasoning\",\"text\":\"thinking\"}
{\"type\":\"agent_message\",\"message\":\"done\"}
";
        let session = CodexSession::parse(stdout).expect("parses");
        assert!(session.transcript.contains("thinking"));
        assert_eq!(session.final_message(), Some("done"));
    }

    // -- Real-subprocess test against a fake executable on disk ----------------

    /// Write a fake `codex` program into `dir` that echoes a fixed JSON event
    /// stream and exits zero. Returns the `(program, base_args)` to invoke it by:
    /// on Windows a `.cmd` driven through `cmd /c`; elsewhere a `chmod +x` script.
    fn write_fake_codex(dir: &Path) -> (PathBuf, Vec<String>) {
        let line = r#"{"type":"agent_message","message":"```yaml\nverdict: pass\nsummary: from a real process\n```"}"#;
        if cfg!(windows) {
            let path = dir.join("fake_codex.cmd");
            let script = format!("@echo off\r\necho {line}\r\n");
            std::fs::write(&path, script).unwrap();
            (
                PathBuf::from("cmd"),
                vec!["/c".to_string(), path.to_string_lossy().into_owned()],
            )
        } else {
            let path = dir.join("fake_codex.sh");
            let script = format!("#!/bin/sh\ncat <<'EOF'\n{line}\nEOF\n");
            std::fs::write(&path, script).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = std::fs::metadata(&path).unwrap().permissions();
                perms.set_mode(0o755);
                std::fs::set_permissions(&path, perms).unwrap();
            }
            (path, Vec::new())
        }
    }

    #[tokio::test]
    async fn real_subprocess_against_a_fake_codex_executable() {
        let tmp = tempfile::tempdir().unwrap();
        let (program, base_args) = write_fake_codex(tmp.path());

        if !program_available(&program) {
            eprintln!("skipping: launcher not available at {}", program.display());
            return;
        }

        let backend =
            CodexBackend::with_command(SystemCommandRunner, program, base_args, Vec::new());
        let reviewer = reviewer();
        let run = RunId("r-real".into());
        let root = tmp.path().to_path_buf();
        let req = request(&reviewer, &run, &root);

        let outcome = backend.review(&req).await.expect("real subprocess parses");
        assert_eq!(outcome.verdict.decision, Decision::Pass);
        assert_eq!(outcome.verdict.summary, "from a real process");
    }

    #[test]
    fn program_available_detects_missing_binary() {
        assert!(!program_available("definitely-not-a-real-program-xyz123"));
    }
}

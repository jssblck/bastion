//! The reviewer schema: the declarative definition of a single-concern reviewer.
//!
//! A reviewer is a bundle of *prompt + trigger + mode + backend + capabilities +
//! (optional) environment*: its execution profile. Reviewers are declarative and
//! static so they stay reviewable and produce a stable trigger set; see
//! `docs/developer-guide/design.md`.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Whether a reviewer gates the merge or only advises.
///
/// A `Gate` decides the merge: all gates must pass. An `Advisor` always
/// functionally passes and contributes findings without blocking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum Mode {
    /// Blocks the merge unless it passes.
    Gate,
    /// Comments but never blocks.
    Advisor,
}

impl Mode {
    /// The lowercase wire form (`"gate"` / `"advisor"`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Gate => "gate",
            Mode::Advisor => "advisor",
        }
    }
}

/// The agent harness a reviewer runs on.
///
/// `Any` lets Bastion choose; the named variants pin a specific harness, e.g.
/// because a subscription's terms require it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Backend {
    /// Bastion picks an available backend.
    #[default]
    Any,
    /// Anthropic's Claude Code CLI.
    ClaudeCode,
    /// OpenAI's Codex CLI.
    Codex,
    /// The Pi harness.
    Pi,
}

impl Backend {
    /// The wire form of the backend name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::Any => "any",
            Backend::ClaudeCode => "claude-code",
            Backend::Codex => "codex",
            Backend::Pi => "pi",
        }
    }
}

/// Capabilities a reviewer opts into. Least privilege is the default: an empty
/// block grants nothing beyond the checkout and the model provider.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Capabilities {
    /// General outbound network beyond the always-allowed model provider.
    #[serde(default)]
    pub network: bool,
    /// MCP servers to load into the agent's context and permit it to call.
    #[serde(default)]
    pub mcp: Vec<String>,
    /// Skills to load into the agent's context.
    #[serde(default)]
    pub skills: Vec<String>,
}

impl Capabilities {
    /// Whether this is the default least-privilege profile (no opt-ins).
    #[must_use]
    pub fn is_least_privilege(&self) -> bool {
        !self.network && self.mcp.is_empty() && self.skills.is_empty()
    }
}

/// How a reviewer's execution environment is provisioned. Absent means the
/// reviewer runs native/in-process on the runner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RunnerSpec {
    /// A Dockerfile to build the environment from. Takes precedence over `image`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dockerfile: Option<String>,
    /// A pre-built image to run, as an alternative to `dockerfile`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
}

/// A single reviewer definition, as parsed from the registry file.
///
/// Trigger globs are kept as raw strings here; they are compiled into a matcher
/// by [`crate::routing`] (parse-don't-validate: the compiled form is a distinct
/// type produced once, at the boundary).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reviewer {
    /// Unique reviewer name; also the check-run name in CI.
    pub name: String,
    /// Path globs over the changed files that trigger this reviewer.
    pub trigger: Vec<String>,
    /// Whether this reviewer gates or advises.
    pub mode: Mode,
    /// The harness to run on.
    #[serde(default)]
    pub backend: Backend,
    /// Per-reviewer wall-clock timeout.
    #[serde(
        default,
        with = "humantime_opt",
        skip_serializing_if = "Option::is_none"
    )]
    pub timeout: Option<Duration>,
    /// Container/runner provisioning; native when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner: Option<RunnerSpec>,
    /// Environment variables injected into the reviewer's environment.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Capability opt-ins.
    #[serde(default, skip_serializing_if = "Capabilities::is_least_privilege")]
    pub capabilities: Capabilities,
    /// Variables interpolated into the prompt before handing off to the agent.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, String>,
    /// The review instruction handed to the agent.
    pub prompt: String,
}

impl Reviewer {
    /// Whether the reviewer runs in a container rather than native.
    #[must_use]
    pub fn is_containerized(&self) -> bool {
        self.runner.is_some()
    }
}

/// Serde helper for an optional [`Duration`] written in human form (`15m`, `90s`).
mod humantime_opt {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub(super) fn serialize<S: Serializer>(
        value: &Option<Duration>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(duration) => {
                serializer.serialize_str(&humantime::format_duration(*duration).to_string())
            }
            None => serializer.serialize_none(),
        }
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Duration>, D::Error> {
        let raw = Option::<String>::deserialize(deserializer)?;
        match raw {
            Some(text) => humantime::parse_duration(&text)
                .map(Some)
                .map_err(D::Error::custom),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_minimal_reviewer() {
        let yaml = r"
name: file-responsibility
trigger: [src/**/*.ts]
mode: gate
prompt: Check single responsibility.
";
        let reviewer: Reviewer = serde_yaml_ng::from_str(yaml).expect("valid reviewer");
        assert_eq!(reviewer.name, "file-responsibility");
        assert_eq!(reviewer.trigger, ["src/**/*.ts"]);
        assert_eq!(reviewer.mode, Mode::Gate);
        assert_eq!(reviewer.backend, Backend::Any);
        assert!(reviewer.timeout.is_none());
        assert!(!reviewer.is_containerized());
        assert!(reviewer.capabilities.is_least_privilege());
    }

    #[test]
    fn parses_a_containerized_reviewer_with_capabilities() {
        let yaml = r"
name: e2e-checkout-flow
trigger: [src/**]
mode: gate
backend: claude-code
timeout: 15m
runner:
  dockerfile: ./.bastion/e2e.Dockerfile
  image: ghcr.io/acme/e2e:latest
env:
  PREVIEW_URL: http://localhost:3000
capabilities:
  network: true
  mcp: [playwright]
  skills: [checkout-flow, browser]
inputs:
  preview_url: http://localhost:3000
prompt: Run the e2e checkout flow.
";
        let reviewer: Reviewer = serde_yaml_ng::from_str(yaml).expect("valid reviewer");
        assert_eq!(reviewer.backend, Backend::ClaudeCode);
        assert_eq!(reviewer.timeout, Some(Duration::from_secs(15 * 60)));
        assert!(reviewer.is_containerized());
        assert!(reviewer.capabilities.network);
        assert_eq!(reviewer.capabilities.mcp, ["playwright"]);
        assert_eq!(
            reviewer.env.get("PREVIEW_URL").map(String::as_str),
            Some("http://localhost:3000")
        );
        assert!(!reviewer.capabilities.is_least_privilege());
    }

    #[test]
    fn mode_and_backend_round_trip_through_their_wire_form() {
        assert_eq!(
            serde_yaml_ng::from_str::<Mode>("advisor").unwrap(),
            Mode::Advisor
        );
        assert_eq!(Mode::Gate.as_str(), "gate");
        assert_eq!(
            serde_yaml_ng::from_str::<Backend>("claude-code").unwrap(),
            Backend::ClaudeCode
        );
        assert_eq!(Backend::Pi.as_str(), "pi");
        assert_eq!(Backend::default(), Backend::Any);
    }
}

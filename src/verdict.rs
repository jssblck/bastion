//! The structured judgment a reviewer returns.
//!
//! Every reviewer emits a [`Verdict`] via its backend's structured-output mode.
//! The top-level [`Decision`] is the authoritative gate outcome; [`Finding`]s
//! explain it. See the verdict schema in `docs/DESIGN.md`.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A monetary amount, stored exactly as integer cents but carried on the wire as
/// decimal dollars to match the documented event schema (`"cost_usd": 0.21`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Money {
    cents: u64,
}

impl Money {
    /// Construct from an exact number of cents.
    #[must_use]
    pub const fn from_cents(cents: u64) -> Self {
        Self { cents }
    }

    /// The amount in whole cents.
    #[must_use]
    pub const fn cents(self) -> u64 {
        self.cents
    }
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "${}.{:02}", self.cents / 100, self.cents % 100)
    }
}

impl Serialize for Money {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[expect(
            clippy::cast_precision_loss,
            reason = "cent counts are far below f64's exact range"
        )]
        serializer.serialize_f64(self.cents as f64 / 100.0)
    }
}

impl<'de> Deserialize<'de> for Money {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let dollars = f64::deserialize(deserializer)?;
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "rounded, non-negative cents"
        )]
        let cents = (dollars * 100.0).round().max(0.0) as u64;
        Ok(Self { cents })
    }
}

/// The authoritative gate outcome of a reviewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum Decision {
    /// The reviewer is satisfied; it does not block the merge.
    Pass,
    /// The reviewer blocks the merge.
    Block,
}

impl Decision {
    /// Whether this decision blocks the merge.
    #[must_use]
    pub fn is_block(self) -> bool {
        matches!(self, Decision::Block)
    }

    /// The lowercase wire form (`"pass"` / `"block"`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Decision::Pass => "pass",
            Decision::Block => "block",
        }
    }
}

/// Whether a finding holds up the merge or is merely a suggestion.
///
/// A finding's kind affects how a comment is surfaced, not the gate outcome;
/// only the top-level [`Decision`] decides that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum FindingKind {
    /// A reason the reviewer blocked.
    Blocking,
    /// A non-blocking suggestion.
    Optional,
}

/// A specific, located comment from a reviewer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// Whether the finding is blocking or optional.
    pub kind: FindingKind,
    /// Repository-relative path the finding refers to.
    pub path: String,
    /// First line of the referenced range (1-based).
    pub line_start: u32,
    /// Last line of the referenced range (1-based, inclusive).
    pub line_end: u32,
    /// The human-readable comment.
    pub detail: String,
}

/// Token and cost accounting for a single reviewer run, when the backend
/// reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Input tokens consumed.
    pub tokens_in: u64,
    /// Output tokens produced.
    pub tokens_out: u64,
    /// Session cost.
    pub cost_usd: Money,
}

/// A reviewer's complete structured judgment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    /// The authoritative gate decision.
    #[serde(rename = "verdict")]
    pub decision: Decision,
    /// A human-friendly summary of the review.
    pub summary: String,
    /// Specific located comments explaining the decision.
    #[serde(default)]
    pub findings: Vec<Finding>,
}

impl Verdict {
    /// Whether the verdict is internally consistent.
    ///
    /// A `block` must carry at least one [`FindingKind::Blocking`] finding (the
    /// reason it blocked). A `pass` must carry *no* blocking findings: a passing
    /// gate that nonetheless lists blocking reasons is self-contradictory, and
    /// since the top-level decision is authoritative, trusting it would fail open
    /// (pass a merge while flagging blockers). A `pass` may carry optional
    /// findings as non-blocking suggestions.
    #[must_use]
    pub fn is_consistent(&self) -> bool {
        let has_blocking = self
            .findings
            .iter()
            .any(|f| f.kind == FindingKind::Blocking);
        match self.decision {
            Decision::Pass => !has_blocking,
            Decision::Block => has_blocking,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_blocking_verdict_from_the_design_schema() {
        let yaml = r#"
verdict: block
summary: A new query path reads rows without scoping by tenant id.
findings:
  - kind: blocking
    path: src/server/db.ts
    line_start: 88
    line_end: 91
    detail: scope this query by tenant_id
"#;
        let verdict: Verdict = serde_yaml_ng::from_str(yaml).expect("valid verdict");
        assert!(verdict.decision.is_block());
        assert_eq!(verdict.findings.len(), 1);
        assert_eq!(verdict.findings[0].kind, FindingKind::Blocking);
        assert!(verdict.is_consistent());
    }

    #[test]
    fn a_pass_needs_no_findings_but_a_block_does() {
        let pass = Verdict {
            decision: Decision::Pass,
            summary: "ok".into(),
            findings: vec![],
        };
        assert!(pass.is_consistent());

        let bad_block = Verdict {
            decision: Decision::Block,
            summary: "no reason".into(),
            findings: vec![],
        };
        assert!(!bad_block.is_consistent());
    }

    #[test]
    fn a_pass_carrying_blocking_findings_is_inconsistent() {
        // A passing gate that also lists a blocking reason is self-contradictory;
        // trusting its top-level `pass` would fail open. Reject it so the caller
        // fails closed.
        let contradictory = Verdict {
            decision: Decision::Pass,
            summary: "passes but blocks?".into(),
            findings: vec![Finding {
                kind: FindingKind::Blocking,
                path: "src/x.rs".into(),
                line_start: 1,
                line_end: 1,
                detail: "this blocks".into(),
            }],
        };
        assert!(!contradictory.is_consistent());

        // A pass with only optional findings is fine.
        let pass_with_optional = Verdict {
            decision: Decision::Pass,
            summary: "ok with a nit".into(),
            findings: vec![Finding {
                kind: FindingKind::Optional,
                path: "src/x.rs".into(),
                line_start: 1,
                line_end: 1,
                detail: "nit".into(),
            }],
        };
        assert!(pass_with_optional.is_consistent());
    }

    #[test]
    fn money_serializes_as_dollars_and_round_trips() {
        let money = Money::from_cents(21);
        assert_eq!(serde_json::to_string(&money).unwrap(), "0.21");
        assert_eq!(money.to_string(), "$0.21");
        let back: Money = serde_json::from_str("0.37").unwrap();
        assert_eq!(back.cents(), 37);
    }
}

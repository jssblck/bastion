# Security Policy

Bastion runs agent backends over repository code and, in CI, brokers per-author
credentials for those backends. Please report suspected security issues
privately instead of opening a public issue.

Report vulnerabilities through GitHub private vulnerability reporting if it is
available for this repository, or email `security@jessica.black`.

Useful reports include: credential exposure or mishandling in the CI
author-to-secret mapping; a way to make a gate report `pass` when it should fail
closed; bypassing the reviewer-policy governance (CODEOWNERS / required check) so
reviewer definitions, prompts, or triggers change without human review;
unsafe handling of a reviewer's container or capability opt-ins; and dependency
vulnerabilities.

There is no bug bounty program. Reports are still appreciated, and responsible
disclosure helps keep real deployments safer.

Only the current main development line is supported.

## Threat model

Bastion is **not** an adversarial security boundary; it is the agent-era
equivalent of team code review for aligned contributors. It assumes PRs are
authored by contributors (human or agent) earnestly working toward the project's
goals, and that reviewed code is therefore trusted input — Bastion does not try
to protect reviewer agents against prompt injection or exfiltration from the code
they review.

The security properties Bastion *does* aim for are governance properties:
gates fail closed, and any change to the reviewer policy is visible to a human
through CODEOWNERS and a required status check. The bar is reasonable reduction
proportionate to effort — a speed bump and good defaults that keep aligned actors
on the rails — not a proof that gaming or exfiltration is impossible. See
[`docs/developer-guide/design.md`](docs/developer-guide/design.md) (_Threat model & trust boundary_) and
[`docs/developer-guide/github-adapter.md`](docs/developer-guide/github-adapter.md) (_Governance_) for the full model.

This is experimental software and has not had a professional security audit. All
usage is at your own risk.

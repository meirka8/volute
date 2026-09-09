# Contributing to CVC

Thank you for helping improve CVC. By participating, you agree to follow the [Code of Conduct](CODE_OF_CONDUCT.md).

## Before opening a change

- Use an issue to discuss substantial behavior or format changes before implementation.
- Do not file public issues for suspected vulnerabilities; follow [SECURITY.md](SECURITY.md).
- Keep changes focused and include tests for behavior changes.
- Do not add captured prompts, credentials, personal data, private repository details, or generated CVC state to a contribution.

## Public issue tracking

GitHub Issues are the canonical record for public Volute OSS work. Use the bug, feature, or improvement form so the right type label (`type: bug`, `type: feature`, or `type: improvement`) is applied. For questions, use a blank issue; maintainers will apply the `question` label during triage. Do not use a public issue for a suspected vulnerability: follow the [public Security Policy contact](https://github.com/meirka8/volute/security/policy) instead.

Maintainers assign the public project fields—Status, Priority, Component, and Initiative—on the [Volute project board](https://github.com/users/meirka8/projects/2). Status describes lifecycle, not a promise of delivery: **Backlog** is recorded for consideration, **Todo** is selected for work, **In Progress** is actively being worked, **In Review** is awaiting review, **Done** is completed, **Duplicate** is tracked elsewhere as the same public work, and **Canceled** will not proceed.

Private hosted-platform implementation is tracked separately and is not mirrored publicly. When work crosses public and private boundaries, public issues are split into self-contained public deliverables; they must not include private links, context, repository details, credentials, captured prompts, personal data, or generated CVC state.

## Optional CVC self-tracking

Self-tracking is optional and applies only when the CVC MCP server is available and capture has been explicitly acknowledged. During substantial work, agentic contributors should use MCP `commit_thought` to record important decisions, intent, and concise, reviewable rationale. Record only explicit summaries; never request or attempt to capture hidden chain-of-thought or another participant's content without their consent.

The presence of `.thoughtignore` does not enable capture or authorize sharing or auto-push. Keep records private by default, review each selected record before sharing, and use CVC's interactive consent flow before sharing or enabling auto-push. See the [privacy reference](Privacy.md) and [CVC MCP documentation](cvc-mcp/README.md).

## Build and test

The Rust workspace requires a current stable Rust toolchain:

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets --all-features
```

For the VS Code extension:

```bash
cd cvc-plugin
npm ci
npm run check-types
npm run lint
npm test
```

For the reviewer:

```bash
cd cvc-reviewer
npm ci
npm run build
npm run lint
npm test -- --run
```

Run the checks relevant to your change and report any checks you could not run in the pull request.

## Developer Certificate of Origin

Contributions require sign-off under the [Developer Certificate of Origin 1.1](https://developercertificate.org/). Sign-off certifies that you have the right to submit the contribution under this project's license.

Add this trailer to every commit, using your real name and an email address you control:

```text
Signed-off-by: Your Name <you@example.com>
```

Git can add it automatically:

```bash
git commit -s
```

The sign-off is a legal certification, not merely a commit-message convention. Pull requests with unsigned commits may be asked to correct them.

## Pull requests

- Explain the problem, approach, privacy/security impact, and test results.
- Update user-facing documentation when behavior changes.
- Preserve local-first and fail-closed privacy boundaries. Treat scrubbers as defense in depth, not as proof that content is safe to publish.
- Avoid unrelated formatting or dependency churn.

Unless you explicitly state otherwise, contributions intentionally submitted for inclusion are provided under the [Apache License 2.0](LICENSE), as described in section 5 of that license.

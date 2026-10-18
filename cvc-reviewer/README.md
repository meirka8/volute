# CVC Reviewer

The web-based review interface for Cognitive Version Control.

## Operating modes

The default, self-contained mode uses a GitHub Personal Access Token (PAT) in
the browser and calls GitHub directly. It has no required CVC-hosted API or
service dependency, so it can be built and run locally with the commands
below. Hosted reviewer integration is optional and is enabled only when a
deployment explicitly sets `VITE_HOSTED_MODE=true` and provides
`VITE_PLATFORM_API_URL`.

## Setup

### Prerequisites
- Node.js 20+
- A GitHub Personal Access Token (PAT)

### Authentication
This application uses the **Alcatraz Protocol**: it runs in the browser and is designed to communicate directly with GitHub. During a session, the GitHub PAT is kept in the browser for that session, including session storage used by the app. In the current implementation, the token is used for requests to `api.github.com`.

#### Required PAT Permissions
If using a **Fine-grained Personal Access Token**, you must grant the following permissions for the target repository:

1.  **Contents**: `Read-only` (Required to fetch the CVC Shadow Graph from `refs/cvc/main`)
2.  **Pull requests**: `Read-only` (Required to fetch PR metadata)
3.  **Metadata**: `Read-only` (Default)

> [!IMPORTANT]
> If you see a 403 error regarding "Resource not accessible by personal access token" when fetching `refs/cvc/main`, you are missing the **Contents** permission.

### Development

```bash
# Install dependencies
npm install

# Run dev server
npm run dev
```

For the local PAT mode, do not set the hosted-mode environment variables.

### Build

```bash
npm run build
```

## CVC sync compatibility and retention

The reviewer reads CVC sync formats v1–v5, including legacy node-embedded links and
append-only `links/<interaction-id>/<commit-sha>.json` records. Format v5 adds immutable
`events/` derivation records and `ranges/` `RangeEvidence` (including
`cvc.changeset/v1` evidence); it validates their canonical IDs, closure, bounds, and
required `by-commit` pointers before joining them to nodes. The reviewer labels all
FORMAT5 derivations—including `rewrite_exact` and `squash_exact`—as **publisher-asserted,
unverified**: it validates wire integrity but does not independently inspect the publisher's
local Git history or establish local trust. Format v4 adds destination-scoped tombstones.
Tombstones are loaded before nodes and win over every legacy or sharded node representation,
as well as the target's FORMAT5 derivation/range closure, so tombstoned interactions are not rendered.
The reviewer evicts the corresponding `cvc-node` query-cache entry when it observes a
tombstone; it refuses malformed/truncated tombstone trees rather than showing an
incomplete timeline. Immutable cognitive entries may be retained in this browser's
IndexedDB query cache for up to 30 days. Tombstones drive eviction from both memory and
that persisted projection; logout and account/PAT transitions clear all reviewer
React Query and IndexedDB caches.

A tombstone is CVC projection suppression, **not** proof that the underlying Git
object was physically erased. Browser query cache eviction only removes this app's
in-memory/query-cache view; it cannot remove data retained by GitHub, a browser
session, developer tools, downloaded responses, clones, forks, reflogs, caches, or
backups. The PAT is retained in browser session storage in the current implementation;
use a private browser context and clear its session data when appropriate.

Temporal links are shown as **lower confidence** because they are inferred from timing
rather than file context. Interaction IDs are UUIDs, not Git content addresses.
Linking-identity attribution is retained for compatibility but is not a public badge.

The PR commit range is fetched in bounded 100-commit pages (up to 100 pages). For a merged
PR, the reported merge SHA is included alongside listed commit SHAs; malformed data or an
over-limit page sequence fails the cognitive overlay closed. Any malformed, missing, or
truncated FORMAT5 namespace, unsafe artifact, or validation failure purges/refuses the
cognitive cache rather than rendering a partial timeline.

## How timeline steps are shown

Nodes shaped like Claude Code transcript steps are laid out as stimulus (what it reacted
to) → reasoning (when the model exposed it) → response → tool calls. The stimulus is usually
a prompt or the previous step's tool results, so a tool's result appears on the **next**
step. Tool results are folded to a one-line summary only on steps that tool output started;
on steps a person started they stay open, as the two can be hard to tell apart. Edit and Write
cards show what the call asked for, not necessarily the change that reached the PR.

Tool results are marked in the text itself, so a fetched web page can imitate the markers.
Each result must match a call of the previous step by tool name and by whether it failed, or
the stimulus is shown undivided, with a note; if that step is not in the timeline, it is
divided by its markers, unchecked. A result claiming a server-side call never matches, as
their results are not recorded. A `[meta]` or `[compaction summary]` part after tool results
is drawn inside them as plain output, marker and all, not as **Context** or
**Compaction summary**. No check catches every imitation, because the markers are plain text.

In a step a person started, only the last `[Request interrupted by user…]` line starts a
prompt, and only if no tool result follows it; otherwise the text stays together, and the
step says when it cannot tell where the prompt starts. Elsewhere that line and plain text
(**Unmarked text**) are monospace, never a prompt.

The Reviewer infers this layout from the shape of the node, because the synced record does
not say which tool captured it. It is a display choice, not a provenance or trust signal.
MCP, VS Code and `cvc run` nodes keep the Thought / Action / Observation layout.

Server-side tools (for example web search run by the model provider) show
**server-side · result not recorded**: the v0.7.0 harness does not see their results, so how
they ended is unknown and they are not counted as failed. Content the capture does not
record appears as a bracketed placeholder such as `[image]` or `[web_search_tool_result]`.
Where the capture hit a limit, the step says so:
**Arguments not recorded: N bytes exceeded the capture limit** replaces tool arguments too
large to keep, and `…[cvc: truncated N bytes]` marks where text was cut. Privacy redactions
appear as `[CVC_REDACTED:v1:<kind>:<id>]`.

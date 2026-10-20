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
on steps a person took part in they stay open (in an older capture the two can be hard to tell
apart). Edit and Write cards show what the call asked for, not necessarily the change that
reached the PR.

**Newer captures mark every part of the stimulus verifiably.** A person's words, a subagent's
task, context the harness injected, a compaction summary and each tool result all start with a
boundary token that tool output cannot have contained when it was written, and the Reviewer
trusts a marker only when its token checks out against the step's own id. So a person's words
are drawn as prose and are never something a tool printed; a subagent's task is labelled
**Task** and described as an instruction from the agent that delegated the work, since nobody
types into a subagent; tool results, context and compaction summaries are drawn as before; and
a note says where the capture cut a part (**… N bytes truncated**), could not scrub it
(**Withheld: this part could not be scrubbed**) or left parts out (**N more parts not
recorded**). The token itself is never shown. Tool output that imitates markers stays output.
The check against the previous step's calls (below) does not undo this division: it puts a note
on a result that none of that step's calls accounts for, and on that result only. The token is
a consistency check, not a signature. It is computed from public values, so it exposes tool
output that imitates the structure, and text changed after it was captured, and it does not say
who wrote the text. It does not cover the name or the error flag in a marker.

A stimulus that carries such tokens but does not verify (changed after it was captured,
damaged, or written for another step) is shown undivided, under **Unverified stimulus**: one
block of monospace text with the note **Shown undivided: this step's markers could not be
verified.**, folded the way tool results are. It is never drawn as a prompt, and never split by
the older rules below.

**Older captures** (before the boundary token) mark tool results in the text itself, so a
fetched web page can imitate the markers. They are read by the rules in the rest of this
section, which are unchanged. Each result must match a call of the previous step by tool name
and by whether it failed, or the stimulus is shown undivided, with a note; if that step is not
in the timeline, it is divided by its markers, unchecked. A result claiming a server-side call
never matches: the model provider runs those calls and delivers their results inside the
assistant's own message, so the capture never writes a tool result for them. A `[meta]` or
`[compaction summary]` part after tool results is drawn inside them as plain output, marker
and all, not as **Context** or **Compaction summary**. No check catches every imitation,
because the markers are plain text.

In an older capture of a step a person started, only the last `[Request interrupted by user…]`
line starts a prompt, and only if no tool result follows it; otherwise the text stays
together, and the step says when it cannot tell where the prompt starts. Elsewhere that line
and plain text (**Unmarked text**) are monospace, never a prompt. (In a newer capture the line
is part of the person's text.)

The Reviewer infers this layout from the shape of the node, because the synced record does
not say which tool captured it. It is a display choice, not a provenance or trust signal.
MCP, VS Code and `cvc run` nodes keep the Thought / Action / Observation layout.

Server-side tools (for example web search, web fetch or code execution run by the model
provider) are listed as `web_search (server-side)` and so on. Newer captures record how each
one ended, as its result block in the same response reported it, and the Reviewer shows that:
**success** or **failed**, and a failed one is counted in **N tool calls failed** (a result block
that reported an error or a non-zero return code, or none arriving in the response; hover the
badge for the wording). Captures from v0.7.0 recorded every server-side call as failed whatever
happened, so for those the Reviewer shows **server-side · result not recorded** and does not
count them as failed. It tells the two apart by the tool protocol the capture recorded: `server`
for a recorded outcome, `native` for v0.7.0. Only the outcome is recorded, never the result's
content: that appears as a bracketed placeholder such as `[web_search_tool_result]`, as other
content the capture does not record does (`[image]`).
Where the capture hit a limit, the step says so:
**Arguments not recorded: N bytes exceeded the capture limit** replaces tool arguments too
large to keep, and `…[cvc: truncated N bytes]` marks where text was cut. Privacy redactions
appear as `[CVC_REDACTED:v1:<kind>:<id>]`, and can replace part of a longer text, such as a
key block inside numbered output, so line numbers can skip.

A field the capture could not scrub is not recorded, and the step says so in words:
**Arguments withheld: N bytes could not be scrubbed.** on the call, and **Withheld: N bytes
could not be scrubbed.** for a response or its reasoning. A step records at most 255 tool
calls and 256 context items. When it left some out, the capture ends the calls with a record of
its own, and the Tool calls end with a note such as **45 more tool calls and 44 context items
were not recorded**. That record is not a call the model made, so it is no card and is not
counted as a call or a failure; only the exact record counts (the `cvc` protocol, the name
`cvc-omitted`, its fixed arguments, as the last entry), so anything that merely resembles it is
drawn as the ordinary call it is. The results of the calls it stands for are not held against
the previous step. A response the capture could not store at all is recorded as a stand-in: its
stimulus, no model name, no reasoning, no tool calls, and a note in place of the response,
which the step words as **This response was withheld: it could not be scrubbed within the
capture limits.**

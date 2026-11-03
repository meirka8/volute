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
divided by its markers, unchecked. A result claiming a server-side call never matches: the
model provider runs those calls and delivers their results inside the assistant's own
message, so the capture never writes a tool result for them. A `[meta]` or
`[compaction summary]` part after tool results is drawn inside them as plain output, marker
and all, not as **Context** or **Compaction summary**. No check catches every imitation,
because the markers are plain text.

In a step a person started, only the last `[Request interrupted by user…]` line starts a
prompt, and only if no tool result follows it; otherwise the text stays together, and the
step says when it cannot tell where the prompt starts. Elsewhere that line and plain text
(**Unmarked text**) are monospace, never a prompt.

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
appear as `[CVC_REDACTED:v1:<kind>:<id>]`.

## Searching and filtering the timeline

The box at the top of the timeline narrows the steps as you type, and the filters under it narrow them by who
or what a step was. Neither fetches anything: they work on the steps already loaded, through an index built the
first time the box or the filters are used (a timeline nobody searches pays nothing for it).

**What is searched: what a card draws.** The index holds the text a step's card shows, and nothing else, so a step
the search leaves always has something on its card that says why. For a step in the stimulus / reasoning / response
layout that is the prompt, injected context, compaction summary and tool results of the stimulus (with the names the
results are shown under), the reasoning, the response, and for each tool call the *values* it recorded as the card
shows them: a command and its description, the lines of an edit's diff, the content of a write, a pattern, a URL, the
rows of a checklist, the value of every other argument (numbers included: a field that holds `42` is found by "42"), and
the value of an option where the card writes one ("timeout 120s", "glob *.ts"). For a step in the Thought / Action /
Observation layout it is the prompt, the line the Action pane writes for each call (the name of the tool and the
arguments as they were recorded, JSON and all), the response, and the reasoning of an AI agent, which is behind "Why?";
a human's reasoning there is on no card and is not searched.

What is **not** searched: the markers of the wire (`[tool_result Bash]`, `[meta]`, `[no stimulus recorded]`), the
Reviewer's own words (labels, badges, placeholders such as "No recorded observation", "runs in background", "ignore
case", "replace all occurrences"), the model name, and the *names* of a tool call's arguments (`command`, `file_path`:
use `tool:` to find a call by what it is called). The names of the tools called, the files a step touched (the
`file_path`, `path` and `notebook_path` of its calls, and its context items) and the hashes of the commits it is linked
to are searched too. A file a step names only in its context items matches even though no card draws it: the card says
so ("Matched in: context file paths, which this card does not show"). The *tool* and *commit* fields are shown in
part (the name a card gives a tool is shorter than the name that was recorded, a commit chip shows seven characters of the
hash): a term that is in the part that is not shown marks the whole of what is.

**How text is compared.** Both the text and the terms are folded the same way, so what is found is what is marked:
case in full (`ß` is "ss", the final sigma is a sigma, a Turkish capital `İ` is an `i` and a dot above), compatibility forms
per character (full-width letters and ligatures are their letters; a precomposed `é` is the same as an `e` and the accent
after it, and "cafe" finds both), every white-space character as a space (a no-break space in a phrase is a space), and every
hidden or bidirectional character left out, so that an invisible character can not hide a match: `pass<ZWSP>word` is found
by "password". The hidden characters are those of Unicode's Default_Ignorable_Code_Point property (and the C0 and C1
controls and the interlinear annotation characters); a card draws them as visible `⟨U+…⟩` tokens, and a match that runs
across one is marked on either side of its token. **Look-alike letters are not folded**: a Cyrillic `а` is not a Latin
`a`, and a search for one does not find the other.

**The query.** Terms are separated by white space; `"retry limit"` is one term. `path:`, `tool:` and `commit:` before a
term (in any case) look in that field only: `path:src/net`, `tool:bash`, `commit:a1b2c3d`, or with a quoted value,
`path:"my dir/a.ts"`. Any other `word:` is plain text (`http://example.com` is searched as it is). A prefix with nothing
after it yet, an empty pair of quotes, and a phrase of nothing but spaces are ignored while you type them. Nothing is read
as a regular expression: `.*` and `(a+)+$` are text. No more than 16 different terms are used ("Only the first 16 terms are
used" says so when there are more), and a term is cut at 512 characters.

**Marks.** What matches is marked in place in `<mark>` elements that hold only text, on every piece of wire text a card
draws, at most 100 marks to a block of text and at most 500 on a card (the card says "Only the first 500 matches on this
card are marked" when it stops: the step still matches, and the folds still count every match they hold). The Reviewer's
own words are never marked, and neither are the names of arguments. A term with a prefix marks only the text of its
field (`tool:` the names of the calls, `path:` file paths, `commit:` hashes). Folded text is never opened for you: a fold
that holds matches says how many, on its control ("Tool results · Bash · 3 matches", "Show all 40 lines · 2 matches", "Why? · 1
match"). The count on a "Show more", "Show all N lines" or diff toggle is of the whole block, matches already in view
included. A control that draws the next page of a long list says how many matches the items not drawn yet hold ("Show 10
more tool calls (53 not shown) · 1 match"). A diff draws a line at a time, so a phrase that runs from one line of it to the
next is not a match there.

**Next and previous.** The count (a live region) reads "3 of 12 · 12 of 300 steps" while there is a query and "12 of 300
steps" when only filters narrow the list: whenever anything narrows the list, the bar says how many of the steps it
shows. Enter in the box goes to the next match, Shift+Enter to the previous, wrapping round; the arrow buttons do the
same, and the step is scrolled into view if it is not and outlined. The outline is not the selection: selecting a step is
unchanged. Escape clears the box, and in an empty box leaves it. `/` anywhere on the page that does not use the key (not while
typing in a box, text area, select or editable region, and not with a modifier) comes to the box.

**Filters.** The "Filters" disclosure holds one group of check boxes for each kind; within a group a step needs *any* of the
values ticked, and it needs one of each group that has a value ticked. Each value that is on is also a chip that removes it.

- *Conversation*: named from its id alone, never from what its steps look like. "Subagent `<id>`" for
  `<session>:agent-<id>`, with the session it belongs to as a note ("of session `<id>`"); "Session `<id>`" for every other
  conversation (a Claude Code session, the MCP server, the editor, `cvc run`). Each shows the number of steps and the first
  line of what its earliest step was asked. Conversation titles are not part of the synced record, so there are none to show;
  ids are cut to their first characters, and no more than it takes to tell two apart (a note cuts its session's id as the
  session's own name does). Ids that are the same as far as a name looks (it never looks past 64 characters) are told apart by
  a number, "(2)".
- *Stimulus origin*: Prompt (a person's), Tool results, and Other (MCP, VS Code and `cvc run` steps, compaction, context).
- *Tool*: the names the cards show (`github · create_issue`, `web_search (server-side)`).
- *Link type*: generated, temporal, verified, and publisher-asserted (any link that carries a derivation event), the same
  split as the commit chips on a card. Only the kinds that are present are offered.
- *Commit*: the pull request's commits, newest first, with the first line of the message when GitHub's commit list gave it
  (no extra request), and how many steps are linked to each. A step linked to the commit stays linked to it whatever else is
  filtered: a filter chooses steps, it never changes them.
- *Touches the open file*: steps whose files include the file open in the diff pane, by path ending (the steps record where
  the file was on the machine that wrote them, and the pull request names it from the repository root, so a root-level
  `index.ts` also finds steps that touched `src/index.ts`). It follows the open file as you switch files, and another pull
  request opens at its own first file, never at one of the last pull request's.

**The list.** Steps that do not match are removed from the list, which is still windowed if it is long, and the steps that
match keep their order. What you opened in a card (a fold, "Show more", "Show all N lines", a page of a long list, "Why?")
is kept with the step, so it is still open when the card comes back, from a search or from being scrolled out of the
window. When a search or filter begins the list opens at the top; when the last one is cleared, the list opens where you
were in it. Clearing from the empty state ("Clear search and filters") puts the keyboard in the box.

**The address keeps the filters after the `#`, and never the search text.** The filters are kept in the fragment of the
address so that a reload, or a step back to the page, finds them again: `#conv=…&origin=…&tool=…&link=…&commit=…&file=…`, one
`conv`, `origin`, `tool`, `link` or `commit` for each value and a `file` path (for "touches the open file" it is the file
that is open; a path from a link stays that file). The fragment is never sent to a server, and the page sends no referrer.
They are written at once, replacing the history entry (a choice of a filter is not a history entry), and only while the page
is still the one the timeline was drawn on; the path, the parameters of the query string that are not ours (`?tab=files`) and
any other parameter of the fragment are left as they were. A value that is not one its parameter holds (an unknown origin, an
id with a space, a hash that is not hexadecimal) is ignored, and every value is shown as text, with hidden characters made
visible; a value too long for the address, or that reading it would change, is left out of it and never written cut short.
Back restores a page's filters, not what was typed in its box.

What you typed is not in the address: a search is often a word from a log, a name or a secret, and the address goes to the
history, to the screen and to whoever the page is shared with. **"Copy link"** (the button beside the filters) is how a
view is shared: it copies the address with the filters *and* the search text, as `q=` in the fragment. Before it puts a text
that looks like a secret (a private key, a token with a known prefix, a JWT, a password assignment, or a long run of mixed-case
letters and digits that is not a hash: a commit hash, a SHA-256 and a UUID are not taken for one) into a link it asks, in a
line you can reach with the keyboard, whether to copy without the search text (the default) or anyway. That is a nudge and
not a guarantee: a secret that does not look like one passes (a short password does). Where the browser does not let the
page write to the clipboard, the link is shown in a field, selected, to copy from; "Link copied" is announced in the count's
live region.

A link that brings a search or filters (a fragment with `q=` or any of the parameters above, opened in a new tab, in this
one, or pasted into its address bar) is applied, the search text leaves the address at once, and a line under the bar says
"This view was opened from a link with a search and filters applied" with a **Clear** button, until you change the search or
the filters: a crafted link can not narrow the timeline without your seeing it. None of these names (`q`, `conv`, `origin`,
`tool`, `link`, `commit`, `file`) is read from the query string, the old `?q=` included. The page takes them out of it when it
opens, so that a leftover one, from a link made before the filters moved to the fragment, is not kept in the address and the
history, and "Copy link" leaves them out of the query string of the link and keeps every other parameter as it was: a query
string is what a server is sent. Whoever opens a copied link keeps its search text in their own browser history, which a page
can not prevent.

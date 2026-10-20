# CVC for Claude Code

A Claude Code plugin that captures your Claude Code sessions into [CVC](https://github.com/meirka8/volute/blob/main/README.md) (Cognitive Version Control). Claude Code's lifecycle hooks call the `cvc` CLI, which reads the transcript Claude Code already writes and records each response as a thought, private by default, in the repository's local cache. Capture does not depend on the model, and the plugin adds hooks only. It is not the VS Code extension in [`cvc-plugin/`](https://github.com/meirka8/volute/tree/main/cvc-plugin).

Claude Code is Anthropic's product. CVC is an independent open-source project: this plugin and the `volute` marketplace are not made, reviewed, or endorsed by Anthropic and are not listed in Anthropic's plugin directory.

## Requirements

- The `cvc` CLI, 0.7.1 or later, on the `PATH` that Claude Code itself sees. The installers and the npm launcher put `cvc` in `~/.cvc/bin`. The desktop app and IDE-launched sessions inherit their environment from how they were started, which can differ from your terminal's. See [Install a release](https://github.com/meirka8/volute/blob/main/README.md#install-a-release).
- If you installed CVC with npm (`@volute_cvc/cvc`, or `@volute_cvc/cvc-mcp`), the `cvc` in npm's global bin directory is a Node launcher, not the CVC binary. Each time it runs, it checks `~/.cvc/bin/cvc --version`. If that build is older than the package, it downloads the matching release, verifies it, caches it under `~/.cvc/mcp-cache/` and publishes it to `~/.cvc/bin` (never downgrading). Then it runs the binary. For this plugin, put `~/.cvc/bin` on Claude Code's `PATH` ahead of npm's global bin directory, and run `cvc --version` once after each npm install or upgrade. Otherwise every hook event starts the Node launcher first, which adds latency in every project, and the first event after an install or upgrade may download a release (network access and writes under `~/.cvc`) within the hook's 30 second budget.
- In each repository you want captured: `cvc init` and `cvc privacy acknowledge-capture` (below).

## Install

Inside Claude Code:

```text
/plugin marketplace add meirka8/volute
/plugin install cvc@volute
```

Or from a shell: `claude plugin marketplace add meirka8/volute`, then `claude plugin install cvc@volute`.

The default scope is user, so the hooks run in every Claude Code session you start, in every project and worktree: terminal, the desktop app's Code tab and IDE extensions, but, per Claude Code's documentation, not cloud sessions. Whether a session is recorded depends on where it was started (see [What you will see](#what-you-will-see)). Start a new session after installing.

To update, run `/plugin marketplace update volute`. Per Claude Code's documentation, Claude Code does not auto-update third-party (non-Anthropic) marketplaces by default. The plugin's version tracks the CLI release it requires, and you receive plugin changes when that version changes.

## In each repository

```bash
cvc init
cvc privacy acknowledge-capture
```

The second command is a typed challenge at an interactive terminal. The plugin's hooks never run the acknowledgement, share or publish commands, and installing it changes no consent state. Those commands accept their phrase only at an interactive terminal, which is a confirmation step, not a security boundary (see [Privacy.md](https://github.com/meirka8/volute/blob/main/Privacy.md)). Capture stays off in a repository until you grant it there. Then start Claude Code in that repository (or one of its worktrees), work as usual, and run `cvc conversations` to see the captured sessions.

In a repository where capture was already acknowledged (for the VS Code extension or a per-checkout install), the plugin starts recording Claude Code sessions from the next session, with no new prompt.

## What you will see

A session is recorded in the repository it was started in: the repository that contains Claude Code's project directory (`CLAUDE_PROJECT_DIR`, which Claude Code gives every hook). Any of that repository's worktrees counts, and so does any subdirectory. The session is recorded there in full, including work it does in other directories (`/add-dir` folders, other repositories it moves into) and its end, wherever its working directory is by then. The hook's working directory only decides which of that repository's worktrees a response is attributed to. A session started outside an initialized repository (in a parent folder, or in another project) is not recorded, even when it later works inside one. Another repository that a session visits receives nothing. If Claude Code provides no `CLAUDE_PROJECT_DIR` (older Claude Code versions, or a manual run), the hook's working directory decides; if it is set but empty, nothing is recorded. This is the same scope the per-checkout install has, since Claude Code loads a checkout's settings only for sessions started there. Open Claude Code in the repository, or one of its worktrees, rather than in a folder above it.

| Where the session was started | What happens |
| --- | --- |
| A project outside a CVC-initialized repository (not a Git repository, `cvc init` never run, a bare repository, a directory that no longer exists, or a repository owned by another user, which Git itself refuses) | Nothing. The hook exits 0 and prints nothing. |
| A parent folder or another project, even if the session later works inside an initialized repository | Nothing is recorded, and nothing is printed. The repository the session visits receives nothing. |
| An initialized repository without the capture acknowledgement | `consent-required: …` in the session, without blocking it. Nothing is recorded. The message ends by saying how to stop the hooks: `/plugin disable cvc@volute` for this plugin, `cvc harness uninstall claude-code` for a checkout's per-checkout hooks. |
| An initialized repository with the acknowledgement | The whole session is recorded there, including work in other directories. Nothing is printed on success. A commit made there from a Claude Code session prints `CVC: Linked N thought(s) to commit <sha>.` (N can be 0), whether or not that session is being recorded there. |
| Another repository that a recorded session works in | Nothing is recorded there. Its content goes to the repository the session was started in. |
| A `.git` entry that Git cannot open (for example a dangling gitfile) | An error that names the path, without blocking the session. |

Where nothing is recorded, each hook event still starts a short `cvc` process. Claude Code passes it the hook's JSON input on stdin; for `PostToolUse` that input includes the tool's arguments and output, which can contain file contents and command output from that project. `cvc` reads up to 1 MiB of it into memory to find the working directory, then checks whether the session was started in a CVC-initialized repository (the one that contains Claude Code's project directory). When it was not, it exits 0 and prints nothing. It reads no transcript, opens no CVC database, writes no file and uses no network, and the input is discarded when it exits. Nothing from those sessions is recorded. The exceptions are the `.git` error in the table, the `cvc: command not found` errors when `cvc` is missing from the `PATH` (below), and the npm launcher under Requirements. The [privacy reference](https://github.com/meirka8/volute/blob/main/Privacy.md#claude-code-plugin-hooks) has the detail.

The silence includes a repository where you forgot `cvc init`. If nothing is captured and nothing was said, see Troubleshooting below.

## The session-start check

At the start of a session (startup and resume) a small check script, [`scripts/check-cvc.sh`](https://github.com/meirka8/volute/blob/main/plugins/claude-code/scripts/check-cvc.sh), can show you one line. It is a message to you; per Claude Code's documentation, a `systemMessage` is not added to Claude's context. The script is designed not to block the session (it always exits 0). If `cvc` is not on the `PATH` Claude Code runs hooks with:

```text
CVC: the cvc command is not on the PATH Claude Code runs hooks with, so the cvc plugin cannot capture this session. Install CVC (https://github.com/meirka8/volute#install-a-release) or add its directory (by default ~/.cvc/bin) to that PATH, then start a new session.
```

That one message is not the only sign. While `cvc` is missing from that `PATH`, every `PostToolUse`, `Stop` and `SessionEnd` event also shows a non-blocking `cvc: command not found` hook error, in every project. Fix the `PATH`, or disable the plugin with `/plugin disable cvc@volute`.

If the `cvc` on the `PATH` is older than 0.7.1:

```text
CVC: the cvc on PATH is older than the cvc plugin needs (0.7.1 or later), so its hooks report errors in projects without CVC. Upgrade CVC (https://github.com/meirka8/volute#install-a-release), then start a new session.
```

Otherwise the check shows nothing. It decides by trying the behavior the hooks need: it pipes a payload whose working directory and project directory do not exist into `cvc ingest claude-code --hook`, which 0.7.1 and later accept silently. An older `cvc` rejects the input, which names no session or transcript, so it records nothing either. You see the first message even in a checkout that has the per-checkout hooks, and it is accurate there: those hooks pin an absolute path and still capture, but the plugin cannot.

## What is recorded

Each thought holds what the response reacted to (your prompt or tool results, size-bounded), the thinking Claude Code writes to the transcript, the response text, and tool calls with their arguments. It is stored locally under `$(git rev-parse --git-common-dir)/cvc`, private by default. Secret scrubbing and `.thoughtignore` are defense in depth, not guarantees; review before sharing. Content the session reads in other directories or repositories is recorded in the repository it was started in, under that repository's `.thoughtignore` and the built-in detectors only; another repository's own `.thoughtignore` is not consulted (see [Privacy.md](https://github.com/meirka8/volute/blob/main/Privacy.md#claude-code-plugin-hooks)).

## Limits

Capture is best effort. A session can go unrecorded without a message you notice (for example `cvc` missing from an IDE-launched session's PATH, a hook timeout, or an abnormal exit). Check `cvc conversations`; `cvc ingest claude-code --transcript <file>` records a finished session afterwards. CVC is not a complete audit record.

A hook run has the hook's 30 second budget, and a run commits nothing unless it finishes. A very large transcript, roughly tens of megabytes on first ingestion (for example when you resume an old, long session after installing the plugin), may not finish within that budget, so the hooks do not record it. Record it afterwards with `cvc ingest claude-code --transcript <file>`.

## With `cvc harness install claude-code`

Installing both is supported. Per Claude Code's documentation, Claude Code runs a plugin's hooks alongside settings-file hooks without de-duplicating them. In a checkout whose `.claude/settings.local.json` carries CVC's entries, both hooks therefore run for each event, in parallel. CVC serializes ingestion of a session with a per-session lock in the repository's CVC directory. The second run finds nothing new, because thought ids are deterministic and the cursor has advanced. Ingestion is idempotent: thought ids are derived from the session and response, so a second run over the same event adds no duplicate thoughts. The cost is one more short `cvc` process per event.

- Choose the plugin if you use Claude Code across many repositories or worktrees and want one install per machine, with updates through `/plugin`.
- Choose `cvc harness install claude-code` if you want capture confined to specific checkouts, cannot or do not want to put `cvc` on the `PATH` Claude Code sees (its hooks pin an absolute binary path), or your organization restricts Claude Code plugins and marketplaces.

## Uninstall

```text
/plugin disable cvc@volute
/plugin uninstall cvc@volute
```

The first stops the hooks and keeps the plugin installed; the second removes it. Neither withdraws the capture acknowledgement or removes what was already captured, and the `cvc privacy` controls still apply to it.

## Troubleshooting

- **Nothing captured, although the repository is initialized and the capture acknowledged.** Was the session started in a parent folder, or in another project? A session is recorded in the repository it was started in, and one started outside an initialized repository is not recorded, even when it later works inside one. Start Claude Code in the repository, or in one of its worktrees.
- **A long or resumed session is missing.** A very large transcript may not finish within the hook's 30 second budget on first ingestion. See Limits, and record it with `cvc ingest claude-code --transcript <file>`.
- **Nothing captured and nothing said.** Run `cvc init` in the repository: without it the hooks stay silent. A repository owned by another user is silent too, because Git refuses it. For a session started from the desktop app or an IDE, check that the environment it inherited has `cvc` on its `PATH`. `cvc conversations` shows what was recorded.
- **`cvc: command not found` on every event.** `cvc` is not on the `PATH` Claude Code runs hooks with. Fix that `PATH`, or run `/plugin disable cvc@volute`.
- **A `.git` error that names a path.** Git cannot open that `.git` entry, for example a gitfile whose worktree is gone. Fix or remove the entry.
- **Slow hooks in every project, or a download at the first event after an upgrade.** The `cvc` found first on the `PATH` is probably the npm launcher. See Requirements.
- **`consent-required`.** The repository is initialized but has no capture acknowledgement. Run `cvc privacy acknowledge-capture` there, or stop the hooks as the message says.

## Windows

Untested for the Claude Code path ([#97](https://github.com/meirka8/volute/issues/97)). The `SessionStart` check is a POSIX `sh` script, which Claude Code runs through Git Bash on Windows. Without Git Bash (PowerShell fallback) it reports a non-blocking hook error. The ingest hooks are expected to run, but this path is untested.

## More

- [Capture Claude Code sessions](https://github.com/meirka8/volute/blob/main/README.md#capture-claude-code-sessions) in the main README.
- [Claude Code plugin reference](https://github.com/meirka8/volute/blob/main/cvc-cli/README.md#claude-code-plugin) and [ingestion details](https://github.com/meirka8/volute/blob/main/cvc-cli/README.md#ingestion).
- [Privacy reference](https://github.com/meirka8/volute/blob/main/Privacy.md#claude-code-plugin-hooks): what the plugin does in unrelated projects, and consent.

License: [Apache-2.0](https://github.com/meirka8/volute/blob/main/LICENSE).

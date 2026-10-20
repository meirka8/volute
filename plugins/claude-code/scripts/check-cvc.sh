#!/bin/sh
# SessionStart check of the cvc Claude Code plugin.
#
# Shows the user one line, as a JSON systemMessage that Claude Code displays
# without adding it to Claude's context, when the plugin's hooks cannot work:
# cvc is not on the PATH Claude Code runs hooks with, or the cvc found there
# predates the hook-mode rule the plugin relies on (0.7.1: silent outside a
# CVC-initialized repository). Prints nothing otherwise, and always exits 0,
# so a session is never blocked.
#
# The version is checked by behavior rather than by parsing `cvc --version`:
# 0.7.1 and later accept a payload whose working directory and project
# directory do not exist silently, while older builds reject it for its
# missing session_id.

link='https://github.com/meirka8/volute#install-a-release'

if ! command -v cvc >/dev/null 2>&1; then
  message="CVC: the cvc command is not on the PATH Claude Code runs hooks with, so the cvc plugin cannot capture this session. Install CVC ($link) or add its directory (by default ~/.cvc/bin) to that PATH, then start a new session."
elif ! printf '%s' '{"cwd":"/nonexistent/cvc-plugin-probe"}' | CLAUDE_PROJECT_DIR=/nonexistent/cvc-plugin-probe cvc ingest claude-code --hook >/dev/null 2>&1; then
  message="CVC: the cvc on PATH is older than the cvc plugin needs (0.7.1 or later), so its hooks report errors in projects without CVC. Upgrade CVC ($link), then start a new session."
else
  exit 0
fi
printf '{"systemMessage":"%s"}\n' "$message"
exit 0

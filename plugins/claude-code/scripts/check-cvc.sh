#!/bin/sh
# SessionStart check of the cvc Claude Code plugin.
#
# Shows the user one line, as a JSON systemMessage that Claude Code displays
# without adding it to Claude's context, when the plugin's hooks cannot work:
# cvc is not in an absolute directory of the PATH Claude Code runs hooks with
# (the plugin's hooks find cvc through run-cvc.sh, which ignores relative PATH
# entries, so a project cannot supply the cvc they run), that cvc cannot be
# run, or it predates the hook-mode rule the plugin relies on (0.7.1: silent
# outside a CVC-initialized repository). Prints nothing otherwise, and always
# exits 0, so a session is never blocked.
#
# The cvc is located by run-cvc.sh, so the rule that picks it is the hooks'
# own, and then probed directly. The version is checked by behavior rather
# than by parsing `cvc --version`: 0.7.1 and later accept a payload whose
# working directory and project directory do not exist silently, while older
# builds reject it for its missing session_id (exit status 1). Any other
# failure means the cvc could not be run at all.
#
# This script's own directory is taken from how it was run. Without one (it
# was not given a path) it does nothing, rather than look in the working
# directory, which is the project's.

link='https://github.com/meirka8/volute#install-a-release'

case $0 in
  */*) here=${0%/*} ;;
  *) exit 0 ;;
esac

cvc=$(CVC_PLUGIN_LOCATE_ONLY=1 /bin/sh "$here/run-cvc.sh" 2>/dev/null)

if [ -z "$cvc" ]; then
  if command -v cvc >/dev/null 2>&1; then
    message="CVC: the only cvc on the PATH Claude Code runs hooks with is in a relative PATH entry, which the cvc plugin does not use, so it cannot capture this session. Put cvc in an absolute directory on that PATH (by default ~/.cvc/bin), then start a new session."
  else
    message="CVC: the cvc command is not on the PATH Claude Code runs hooks with, so the cvc plugin cannot capture this session. Install CVC ($link) or add its directory (by default ~/.cvc/bin) to that PATH, then start a new session."
  fi
else
  # In a group of its own: a shell reports a program killed by a signal on its
  # own stderr, which the redirections of the program do not reach.
  {
    printf '%s' '{"cwd":"/nonexistent/cvc-plugin-probe"}' | CLAUDE_PROJECT_DIR=/nonexistent/cvc-plugin-probe "$cvc" ingest claude-code --hook >/dev/null 2>&1
  } 2>/dev/null
  status=$?
  case $status in
    0) exit 0 ;;
    1)
      message="CVC: the cvc on PATH is older than the cvc plugin needs (0.7.1 or later), so its hooks report errors in projects without CVC. Upgrade CVC ($link), then start a new session."
      ;;
    *)
      message="CVC: the cvc found on the PATH Claude Code runs hooks with could not be run (for example an npm-installed cvc whose node is not on that PATH), so the cvc plugin cannot capture this session. Check that cvc --version works with that PATH, then start a new session."
      ;;
  esac
fi
printf '{"systemMessage":"%s"}\n' "$message"
exit 0

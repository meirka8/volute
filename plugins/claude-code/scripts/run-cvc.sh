#!/bin/sh
# Runs the cvc found in an absolute directory of PATH, for the cvc Claude Code
# plugin's hooks: `/bin/sh run-cvc.sh <cvc arguments>`.
#
# The plugin's hooks run in every project a user opens. A relative PATH entry
# (".", an empty entry, "bin") would let the project in the working directory
# supply the cvc they run, so the rule is that only entries starting with "/"
# are searched (on Windows Git Bash they are "/c/..."), for `cvc`, then
# `cvc.exe`, as a regular executable file. The first one found is run with the
# arguments and stdin unchanged, and its exit status is the script's.
# With none, the script exits 127 and says why.
#
# With CVC_PLUGIN_LOCATE_ONLY=1 in the environment it does not run anything:
# it prints the path it found, then a newline, and exits 0 (check-cvc.sh uses
# this to find the cvc it checks by the same rule).

set -f
found=
IFS=:
for dir in $PATH; do
  case $dir in
    /*) ;;
    *) continue ;;
  esac
  for name in cvc cvc.exe; do
    if [ -f "$dir/$name" ] && [ -x "$dir/$name" ]; then
      found=$dir/$name
      break 2
    fi
  done
done

if [ -z "$found" ]; then
  echo 'cvc: command not found in an absolute PATH directory (the cvc plugin does not use relative PATH entries)' >&2
  exit 127
fi
if [ "$CVC_PLUGIN_LOCATE_ONLY" = 1 ]; then
  printf '%s\n' "$found"
  exit 0
fi
exec "$found" "$@"

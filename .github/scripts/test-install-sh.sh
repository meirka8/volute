#!/bin/sh
# Offline checks for install.sh, run by .github/workflows/test.yml on Linux and
# macOS. They need no network and never touch the real home directory.
#
# install.sh downloads a release when it runs, so curl is replaced by a stub on
# PATH that serves an archive built here, packed the way release.yml packs it:
# cvc, cvc-mcp and cvc-lsp, then LICENSE and THIRD-PARTY-NOTICES.md, all at the
# archive root. Every case runs the real install.sh in a throwaway HOME with an
# empty environment, detached from any terminal so that its PATH prompt is
# skipped, and checks what was installed where, what it printed and what it left
# behind. The cases cover the license and third-party notices (a fresh install,
# a re-run, an upgrade, archives that lack the notices, a custom install
# directory, destinations that must be refused) and that the checksum and
# safe-extraction checks still stop an install before anything is written.
#
# Needs a platform that install.sh supports (Linux x86-64, macOS), a tar, and
# setsid or perl to detach from the terminal. Keep this file POSIX sh: it runs
# under dash on Ubuntu and under bash 3.2 on macOS.

set -u

HERE=$(cd "$(dirname "$0")" && pwd -P) || exit 1
INSTALL_SH=$(cd "$HERE/../.." && pwd -P)/install.sh
if [ ! -f "$INSTALL_SH" ]; then
  echo "cannot find install.sh at $INSTALL_SH" >&2
  exit 1
fi

case "$(uname -s):$(uname -m)" in
  Linux:x86_64|Linux:amd64|Darwin:x86_64|Darwin:amd64|Darwin:arm64) ;;
  *) echo "skipped: install.sh has no release for this platform ($(uname -s) $(uname -m))"; exit 0 ;;
esac

# Without setsid or perl the installer would find the controlling terminal, if
# there is one, and wait for an answer to its PATH prompt.
if ! command -v setsid >/dev/null 2>&1 && ! command -v perl >/dev/null 2>&1; then
  if (: </dev/tty) 2>/dev/null; then
    echo "skipped: setsid or perl is needed to detach install.sh from this terminal"
    exit 0
  fi
fi

ROOT=$(mktemp -d "${TMPDIR:-/tmp}/cvc-install-sh-test.XXXXXX") || exit 1
trap 'rm -rf "$ROOT"' EXIT
trap 'exit 1' HUP INT TERM
# install.sh rejects other characters in CVC_INSTALL_DIR, and the cases below
# put one under $ROOT.
case "$ROOT" in
  *[!A-Za-z0-9_./\ -]*) echo "the temporary directory $ROOT has characters install.sh rejects; set TMPDIR to a plainer path" >&2; exit 1 ;;
esac

CHECKS=0
FAILURES=0

pass() {
  CHECKS=$((CHECKS + 1))
  printf '  ok    %s\n' "$1"
}

fail() {
  CHECKS=$((CHECKS + 1))
  FAILURES=$((FAILURES + 1))
  printf '  FAIL  %s\n' "$1"
  if [ -n "${2:-}" ]; then printf '        %s\n' "$2"; fi
}

# check <description> <command...>: passes when the command succeeds.
check() {
  _desc=$1
  shift
  if "$@"; then pass "$_desc"; else fail "$_desc"; fi
}

# equals <description> <expected> <actual>: shows both on a mismatch.
equals() {
  if [ "$2" = "$3" ]; then
    pass "$1"
  else
    fail "$1" "expected [$(printf '%s' "$2" | tr '\n' '|')] but found [$(printf '%s' "$3" | tr '\n' '|')]"
  fi
}

# has <file> <text> / lacks <file> <text>: the file contains the text, as a fixed
# string, or does not.
has() {
  grep -F -- "$2" "$1" >/dev/null 2>&1
}

lacks() {
  ! grep -F -- "$2" "$1" >/dev/null 2>&1
}

# tree_lacks <directory> <text>: no file below the directory contains the text.
tree_lacks() {
  ! grep -rF -- "$2" "$1" >/dev/null 2>&1
}

is_regular_file() {
  [ -f "$1" ] && [ ! -L "$1" ]
}

# Names in a directory, dotfiles included, sorted, one per line. Empty when the
# directory is empty or missing.
entries() {
  for _p in "$1"/* "$1"/.[!.]* "$1"/..?*; do
    if [ -e "$_p" ] || [ -L "$_p" ]; then printf '%s\n' "${_p##*/}"; fi
  done | LC_ALL=C sort
}

# The first ten characters of `ls -l`: file type and permission bits.
mode_of() {
  ls -ld "$1" 2>/dev/null | cut -c1-10
}

FIX=$ROOT/fixtures
FAKE_BIN=$ROOT/fakebin
mkdir -p "$FIX/archives" "$FAKE_BIN"

# Two "releases" whose files differ, so that an upgrade is visible.
for v in 1 2; do
  mkdir -p "$FIX/bin$v" "$FIX/notices$v"
  for b in cvc cvc-mcp cvc-lsp; do
    printf '#!/bin/sh\necho "%s release %s"\n' "$b" "$v" > "$FIX/bin$v/$b"
    chmod 0755 "$FIX/bin$v/$b"
  done
  printf 'LICENSE text, release %s\n' "$v" > "$FIX/notices$v/LICENSE"
  printf '# Third-party notices, release %s\n' "$v" > "$FIX/notices$v/THIRD-PARTY-NOTICES.md"
done

# make_archive <out> <bindir> <noticesdir> [notice names...]: the binaries, then
# the named notice files from the other directory, as release.yml packs them.
make_archive() {
  _out=$1
  _bins=$2
  _notices=$3
  shift 3
  if [ $# -gt 0 ]; then
    tar -czf "$_out" -C "$_bins" cvc cvc-mcp cvc-lsp -C "$_notices" "$@" || exit 1
  else
    tar -czf "$_out" -C "$_bins" cvc cvc-mcp cvc-lsp || exit 1
  fi
}

A=$FIX/archives
make_archive "$A/full1.tar.gz" "$FIX/bin1" "$FIX/notices1" LICENSE THIRD-PARTY-NOTICES.md
make_archive "$A/full2.tar.gz" "$FIX/bin2" "$FIX/notices2" LICENSE THIRD-PARTY-NOTICES.md
# An older release: binaries only.
make_archive "$A/bare.tar.gz" "$FIX/bin1" "$FIX/notices1"
make_archive "$A/license-only.tar.gz" "$FIX/bin1" "$FIX/notices1" LICENSE
# A release that lacks one of the binaries, notices or not.
tar -czf "$A/no-lsp.tar.gz" -C "$FIX/bin1" cvc cvc-mcp -C "$FIX/notices1" LICENSE THIRD-PARTY-NOTICES.md || exit 1

# LICENSE as a symlink to a file outside the archive.
mkdir -p "$FIX/evil"
printf 'canary: must never be installed\n' > "$FIX/canary"
ln -s "$FIX/canary" "$FIX/evil/LICENSE"
cp "$FIX/notices1/THIRD-PARTY-NOTICES.md" "$FIX/evil/THIRD-PARTY-NOTICES.md"
tar -czf "$A/link.tar.gz" -C "$FIX/bin1" cvc cvc-mcp cvc-lsp -C "$FIX/evil" LICENSE THIRD-PARTY-NOTICES.md || exit 1

# An empty LICENSE, and a THIRD-PARTY-NOTICES.md far larger than any notice file
# (40 MiB of zeros, a few KiB once compressed).
mkdir -p "$FIX/odd"
: > "$FIX/odd/LICENSE"
cp "$FIX/notices1/THIRD-PARTY-NOTICES.md" "$FIX/odd/THIRD-PARTY-NOTICES.md"
tar -czf "$A/empty-license.tar.gz" -C "$FIX/bin1" cvc cvc-mcp cvc-lsp -C "$FIX/odd" LICENSE THIRD-PARTY-NOTICES.md || exit 1
mkdir -p "$FIX/big"
cp "$FIX/notices1/LICENSE" "$FIX/big/LICENSE"
dd if=/dev/zero of="$FIX/big/THIRD-PARTY-NOTICES.md" bs=1048576 count=40 2>/dev/null
tar -czf "$A/oversized.tar.gz" -C "$FIX/bin1" cvc cvc-mcp cvc-lsp -C "$FIX/big" LICENSE THIRD-PARTY-NOTICES.md || exit 1

# The curl stand-in. It serves FAKE_ARCHIVE under whatever archive name the
# installer asks for, answers the checksum request with a SHA256SUMS.txt for
# FAKE_SUMS_ARCHIVE (default: FAKE_ARCHIVE; a different file simulates a
# tampered download), and logs each URL. An option it does not know fails the
# request, so a change to the installer's curl flags shows up here.
cat > "$FAKE_BIN/curl" <<'STUB'
#!/bin/sh
out=
wout=
url=
while [ $# -gt 0 ]; do
  case "$1" in
    --output) out=$2; shift 2 ;;
    --write-out) wout=$2; shift 2 ;;
    --max-redirs|--max-filesize|--proto|--proto-redir|--retry) shift 2 ;;
    --fail|--location|--tlsv1.2) shift ;;
    -*) echo "stub curl: unexpected option $1" >&2; exit 2 ;;
    *) url=$1; shift ;;
  esac
done
case "$url" in
  https://*) ;;
  *) echo "stub curl: not an https URL: $url" >&2; exit 22 ;;
esac
printf '%s\n' "$url" >> "$FAKE_CURL_LOG"
hash_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}'
  else shasum -a 256 "$1" | awk '{print $1}'; fi
}
name=${url##*/}
case "$name" in
  *.tar.gz)
    cp "$FAKE_ARCHIVE" "$out" || exit 23
    printf '%s\n' "$name" > "$FAKE_CURL_DIR/asset-name" || exit 23 ;;
  SHA256SUMS.txt)
    asset=$(cat "$FAKE_CURL_DIR/asset-name") || exit 23
    printf '%s  %s\n' "$(hash_of "${FAKE_SUMS_ARCHIVE:-$FAKE_ARCHIVE}")" "$asset" > "$out" || exit 23 ;;
  *) echo "stub curl: unexpected URL: $url" >&2; exit 22 ;;
esac
if [ "$wout" = '%{url_effective}' ]; then printf '%s' "$url"; fi
exit 0
STUB
chmod 0755 "$FAKE_BIN/curl"

# Runs "$@" in a new session, so that /dev/tty cannot be opened and install.sh
# skips its PATH prompt instead of waiting for an answer.
detached() {
  if command -v setsid >/dev/null 2>&1; then
    setsid -w "$@"
  elif command -v perl >/dev/null 2>&1; then
    perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV or die "exec: $!\n"' -- "$@"
  else
    "$@"
  fi
}

# new_case <name> <description>: a fresh HOME and TMPDIR for one case.
# Sets $CASE and $HOME_DIR.
new_case() {
  CASE=$ROOT/$1
  HOME_DIR=$CASE/home
  mkdir -p "$HOME_DIR" "$CASE/tmp"
  printf '\n%s\n' "$2"
}

# run_install <archive> [NAME=value ...]: runs install.sh in the current case
# with an empty environment plus the given variables, stdin from /dev/null (or,
# with PIPED=yes, the script itself, as in `curl ... | sh`). Leaves the output
# in $CASE/out and $CASE/err and the exit status in $RC.
PIPED=no
run_install() {
  _archive=$1
  shift
  : > "$CASE/curl.log"
  if [ "$PIPED" = yes ]; then
    cat "$INSTALL_SH" | detached env -i \
      "PATH=$FAKE_BIN:$PATH" "HOME=$HOME_DIR" "TMPDIR=$CASE/tmp" \
      "FAKE_ARCHIVE=$_archive" "FAKE_CURL_LOG=$CASE/curl.log" "FAKE_CURL_DIR=$CASE" \
      "$@" sh > "$CASE/out" 2> "$CASE/err"
  else
    detached env -i \
      "PATH=$FAKE_BIN:$PATH" "HOME=$HOME_DIR" "TMPDIR=$CASE/tmp" \
      "FAKE_ARCHIVE=$_archive" "FAKE_CURL_LOG=$CASE/curl.log" "FAKE_CURL_DIR=$CASE" \
      "$@" sh "$INSTALL_SH" > "$CASE/out" 2> "$CASE/err" < /dev/null
  fi
  RC=$?
}

BINS=$(printf 'cvc\ncvc-lsp\ncvc-mcp')
BOTH=$(printf 'LICENSE\nTHIRD-PARTY-NOTICES.md')
DOWNLOAD=https://github.com/meirka8/volute/releases/latest/download
NOT_INSTALLED_WARNING="Warning: the license and third-party notices were not installed"

# expect_binaries <bin directory> <label>: exactly the three binaries, and
# nothing left over from the install (lock, staging directory), all executable.
expect_binaries() {
  equals "$2: the bin directory holds exactly cvc, cvc-lsp and cvc-mcp" "$BINS" "$(entries "$1")"
  _runnable=yes
  for _b in cvc cvc-mcp cvc-lsp; do
    if [ ! -x "$1/$_b" ]; then _runnable=no; fi
  done
  equals "$2: the binaries are executable" yes "$_runnable"
}

###############################################################################
new_case fresh "a fresh install of an archive that carries the notices"
run_install "$A/full1.tar.gz"
BIN=$HOME_DIR/.cvc/bin
SHARE=$HOME_DIR/.cvc/share/cvc
equals "exit status" 0 "$RC"
expect_binaries "$BIN" "fresh"
equals "the notices directory holds exactly LICENSE and THIRD-PARTY-NOTICES.md" "$BOTH" "$(entries "$SHARE")"
check "LICENSE is the archive's" cmp -s "$SHARE/LICENSE" "$FIX/notices1/LICENSE"
check "THIRD-PARTY-NOTICES.md is the archive's" cmp -s "$SHARE/THIRD-PARTY-NOTICES.md" "$FIX/notices1/THIRD-PARTY-NOTICES.md"
equals "the notices are readable files (0644)" "-rw-r--r--|-rw-r--r--" "$(mode_of "$SHARE/LICENSE")|$(mode_of "$SHARE/THIRD-PARTY-NOTICES.md")"
equals "the binaries are 0755" "-rwxr-xr-x" "$(mode_of "$BIN/cvc")"
check "the summary still reports the install directory" has "$CASE/out" "Success! CVC installed to $BIN"
check "the summary names the notices directory" has "$CASE/out" "License and third-party notices installed to $SHARE"
check "the summary lists both notice files" has "$CASE/out" "  LICENSE, THIRD-PARTY-NOTICES.md"
check "nothing is said about missing notices" lacks "$CASE/out" "Not included"
equals "nothing was printed on stderr" "" "$(cat "$CASE/err")"
equals "nothing else was created in HOME" ".cvc" "$(entries "$HOME_DIR")"
equals "the temporary directory was cleaned up" "" "$(entries "$CASE/tmp")"
check "the archive was requested over https from the latest release" has "$CASE/curl.log" "$DOWNLOAD/cvc-"
check "the checksums were requested from the same release" has "$CASE/curl.log" "$DOWNLOAD/SHA256SUMS.txt"
equals "and nothing else was requested" 2 "$(wc -l < "$CASE/curl.log" | tr -d ' ')"

###############################################################################
printf '\n%s\n' "re-running the installer on the same archive"
mkdir -p "$SHARE"
printf 'edited by hand\n' > "$SHARE/LICENSE"
run_install "$A/full1.tar.gz"
equals "exit status" 0 "$RC"
expect_binaries "$BIN" "re-run"
equals "still exactly one copy of each notice, and nothing else" "$BOTH" "$(entries "$SHARE")"
check "LICENSE was overwritten in place with the archive's" cmp -s "$SHARE/LICENSE" "$FIX/notices1/LICENSE"
check "THIRD-PARTY-NOTICES.md is the archive's" cmp -s "$SHARE/THIRD-PARTY-NOTICES.md" "$FIX/notices1/THIRD-PARTY-NOTICES.md"
check "the summary names the notices directory" has "$CASE/out" "License and third-party notices installed to $SHARE"
equals "nothing was printed on stderr" "" "$(cat "$CASE/err")"
equals "the temporary directory was cleaned up" "" "$(entries "$CASE/tmp")"

###############################################################################
printf '\n%s\n' "upgrading to a newer release"
run_install "$A/full2.tar.gz"
equals "exit status" 0 "$RC"
expect_binaries "$BIN" "upgrade"
check "cvc is the new release" has "$BIN/cvc" "release 2"
equals "still exactly one copy of each notice" "$BOTH" "$(entries "$SHARE")"
check "LICENSE is the new release's" cmp -s "$SHARE/LICENSE" "$FIX/notices2/LICENSE"
check "THIRD-PARTY-NOTICES.md is the new release's" cmp -s "$SHARE/THIRD-PARTY-NOTICES.md" "$FIX/notices2/THIRD-PARTY-NOTICES.md"

###############################################################################
printf '\n%s\n' "installing an older archive without notices over that install"
run_install "$A/bare.tar.gz"
equals "exit status" 0 "$RC"
expect_binaries "$BIN" "older archive"
check "cvc is the older release" has "$BIN/cvc" "release 1"
check "the summary says the notices were not included in the archive" has "$CASE/out" "Not included in this release archive, so not installed: LICENSE, THIRD-PARTY-NOTICES.md"
check "the summary does not claim an install" lacks "$CASE/out" "notices installed to"
equals "the notices of the earlier install are left as they were" "$BOTH" "$(entries "$SHARE")"
check "LICENSE is untouched" cmp -s "$SHARE/LICENSE" "$FIX/notices2/LICENSE"
equals "nothing was printed on stderr" "" "$(cat "$CASE/err")"

###############################################################################
new_case older "a fresh install of an archive without notices"
run_install "$A/bare.tar.gz"
equals "exit status" 0 "$RC"
expect_binaries "$HOME_DIR/.cvc/bin" "older"
check "no notices directory was created" test ! -e "$HOME_DIR/.cvc/share"
check "the summary says the notices were not included in the archive" has "$CASE/out" "Not included in this release archive, so not installed: LICENSE, THIRD-PARTY-NOTICES.md"
check "the summary still reports the install directory" has "$CASE/out" "Success! CVC installed to $HOME_DIR/.cvc/bin"
equals "nothing was printed on stderr" "" "$(cat "$CASE/err")"
printf '%s\n' "  then upgrading it to an archive with notices"
run_install "$A/full1.tar.gz"
equals "exit status" 0 "$RC"
equals "the notices directory now holds exactly LICENSE and THIRD-PARTY-NOTICES.md" "$BOTH" "$(entries "$HOME_DIR/.cvc/share/cvc")"

###############################################################################
new_case partial "an archive with LICENSE but without THIRD-PARTY-NOTICES.md"
run_install "$A/license-only.tar.gz"
equals "exit status" 0 "$RC"
equals "only LICENSE is installed" "LICENSE" "$(entries "$HOME_DIR/.cvc/share/cvc")"
check "the summary names the notices directory" has "$CASE/out" "License and third-party notices installed to $HOME_DIR/.cvc/share/cvc"
check "the summary says THIRD-PARTY-NOTICES.md was not included" has "$CASE/out" "Not included in this release archive, so not installed: THIRD-PARTY-NOTICES.md"

###############################################################################
new_case prefix "a custom CVC_INSTALL_DIR (<prefix>/bin gives <prefix>/share/cvc)"
PREFIX=$CASE/opt
run_install "$A/full1.tar.gz" "CVC_INSTALL_DIR=$PREFIX/bin"
equals "exit status" 0 "$RC"
expect_binaries "$PREFIX/bin" "custom prefix"
equals "the notices are in <prefix>/share/cvc" "$BOTH" "$(entries "$PREFIX/share/cvc")"
check "the summary names that directory" has "$CASE/out" "License and third-party notices installed to $PREFIX/share/cvc"
equals "the default location was not used" "" "$(entries "$HOME_DIR")"
printf '%s\n' "  with a trailing slash and a space in the path"
PREFIX="$CASE/my prefix"
run_install "$A/full1.tar.gz" "CVC_INSTALL_DIR=$PREFIX/bin/"
equals "exit status" 0 "$RC"
equals "the binaries are in <prefix>/bin" "$BINS" "$(entries "$PREFIX/bin")"
equals "the notices are in <prefix>/share/cvc" "$BOTH" "$(entries "$PREFIX/share/cvc")"
equals "<prefix> holds only bin and share" "$(printf 'bin\nshare')" "$(entries "$PREFIX")"

###############################################################################
new_case piped "the script piped into sh, with a pinned release version"
PIPED=yes
run_install "$A/full1.tar.gz" "CVC_RELEASE_VERSION=v0.7.1"
PIPED=no
equals "exit status" 0 "$RC"
equals "the notices directory holds exactly LICENSE and THIRD-PARTY-NOTICES.md" "$BOTH" "$(entries "$HOME_DIR/.cvc/share/cvc")"
check "the pinned release was requested" has "$CASE/curl.log" "https://github.com/meirka8/volute/releases/download/v0.7.1/SHA256SUMS.txt"
equals "nothing was printed on stderr" "" "$(cat "$CASE/err")"

###############################################################################
# A problem with the notices must be reported, but must not stop the verified
# binaries from being installed, and nothing may be written through a link.
new_case unsafe-link "share/cvc is a symlink to another directory"
mkdir -p "$HOME_DIR/.cvc/share" "$CASE/elsewhere"
ln -s "$CASE/elsewhere" "$HOME_DIR/.cvc/share/cvc"
run_install "$A/full1.tar.gz"
equals "exit status" 0 "$RC"
expect_binaries "$HOME_DIR/.cvc/bin" "symlinked share/cvc"
equals "nothing was written through the link" "" "$(entries "$CASE/elsewhere")"
check "the link is still a link" test -L "$HOME_DIR/.cvc/share/cvc"
check "a warning names the notices on stderr" has "$CASE/err" "$NOT_INSTALLED_WARNING"
check "the warning says why" has "$CASE/err" "refusing symlink directory: $HOME_DIR/.cvc/share/cvc"
check "the summary says they were not installed" has "$CASE/out" "Not installed (see the warning above): LICENSE, THIRD-PARTY-NOTICES.md"
check "the summary does not claim an install" lacks "$CASE/out" "notices installed to"
equals "the temporary directory was cleaned up" "" "$(entries "$CASE/tmp")"

new_case unsafe-dir "an existing LICENSE that is a directory"
mkdir -p "$HOME_DIR/.cvc/share/cvc/LICENSE"
printf 'keep\n' > "$HOME_DIR/.cvc/share/cvc/LICENSE/keep"
run_install "$A/full1.tar.gz"
equals "exit status" 0 "$RC"
expect_binaries "$HOME_DIR/.cvc/bin" "LICENSE directory"
equals "the directory and nothing else is there" "LICENSE" "$(entries "$HOME_DIR/.cvc/share/cvc")"
equals "the directory's content is untouched" "keep" "$(entries "$HOME_DIR/.cvc/share/cvc/LICENSE")"
check "a warning names the notices on stderr" has "$CASE/err" "$NOT_INSTALLED_WARNING"

new_case unsafe-file-link "an existing LICENSE that is a symlink to a file"
mkdir -p "$HOME_DIR/.cvc/share/cvc"
printf 'precious\n' > "$CASE/precious"
ln -s "$CASE/precious" "$HOME_DIR/.cvc/share/cvc/LICENSE"
run_install "$A/full1.tar.gz"
equals "exit status" 0 "$RC"
expect_binaries "$HOME_DIR/.cvc/bin" "LICENSE symlink"
equals "the file behind the link is untouched" "precious" "$(cat "$CASE/precious")"
equals "only the link is there" "LICENSE" "$(entries "$HOME_DIR/.cvc/share/cvc")"
check "a warning names the notices on stderr" has "$CASE/err" "$NOT_INSTALLED_WARNING"

new_case dangling "an existing LICENSE that is a dangling symlink"
mkdir -p "$HOME_DIR/.cvc/share/cvc"
ln -s "$CASE/not-there" "$HOME_DIR/.cvc/share/cvc/LICENSE"
run_install "$A/full1.tar.gz"
equals "exit status" 0 "$RC"
check "LICENSE was replaced by a regular file, not written through" is_regular_file "$HOME_DIR/.cvc/share/cvc/LICENSE"
check "nothing was created behind the link" test ! -e "$CASE/not-there"
equals "both notices are there" "$BOTH" "$(entries "$HOME_DIR/.cvc/share/cvc")"

new_case blocked "share is a regular file, so the directory cannot be created"
mkdir -p "$HOME_DIR/.cvc"
printf 'not a directory\n' > "$HOME_DIR/.cvc/share"
run_install "$A/full1.tar.gz"
equals "exit status" 0 "$RC"
expect_binaries "$HOME_DIR/.cvc/bin" "blocked share"
equals "the file is untouched" "not a directory" "$(cat "$HOME_DIR/.cvc/share")"
check "a warning names the notices on stderr" has "$CASE/err" "$NOT_INSTALLED_WARNING"
check "the summary says they were not installed" has "$CASE/out" "Not installed (see the warning above): LICENSE, THIRD-PARTY-NOTICES.md"

new_case failed-copy "a failure after the notices staging directory was created"
mkdir -p "$CASE/failcp"
cat > "$CASE/failcp/cp" <<'STUB'
#!/bin/sh
# cp that refuses to write into the notices staging directory and is the real cp
# for everything else.
case "$2" in
  */share/cvc/.cvc-install.*) echo "stub cp: refusing $2" >&2; exit 1 ;;
esac
exec "$FAKE_REAL_CP" "$@"
STUB
chmod 0755 "$CASE/failcp/cp"
run_install "$A/full1.tar.gz" "PATH=$CASE/failcp:$FAKE_BIN:$PATH" "FAKE_REAL_CP=$(command -v cp)"
equals "exit status" 0 "$RC"
expect_binaries "$HOME_DIR/.cvc/bin" "failed copy"
equals "the notices directory holds no staging directory or partial file" "" "$(entries "$HOME_DIR/.cvc/share/cvc")"
check "a warning names the notices on stderr" has "$CASE/err" "$NOT_INSTALLED_WARNING"
check "the summary says they were not installed" has "$CASE/out" "Not installed (see the warning above): LICENSE, THIRD-PARTY-NOTICES.md"

###############################################################################
# Members that are present but not usable notices are skipped; they never reach
# the file system as anything but an installer-created regular file.
new_case link-member "an archive whose LICENSE is a symlink to a file outside it"
run_install "$A/link.tar.gz"
equals "exit status" 0 "$RC"
equals "only THIRD-PARTY-NOTICES.md is installed" "THIRD-PARTY-NOTICES.md" "$(entries "$HOME_DIR/.cvc/share/cvc")"
check "the symlink's target was not followed" tree_lacks "$HOME_DIR/.cvc" "canary"
check "the summary says LICENSE was not included" has "$CASE/out" "Not included in this release archive, so not installed: LICENSE"

new_case empty-member "an archive whose LICENSE is empty"
run_install "$A/empty-license.tar.gz"
equals "exit status" 0 "$RC"
equals "only THIRD-PARTY-NOTICES.md is installed" "THIRD-PARTY-NOTICES.md" "$(entries "$HOME_DIR/.cvc/share/cvc")"

new_case oversized-member "an archive whose THIRD-PARTY-NOTICES.md is far larger than any notice file"
run_install "$A/oversized.tar.gz"
equals "exit status" 0 "$RC"
expect_binaries "$HOME_DIR/.cvc/bin" "oversized member"
equals "only LICENSE is installed" "LICENSE" "$(entries "$HOME_DIR/.cvc/share/cvc")"
equals "the temporary directory was cleaned up" "" "$(entries "$CASE/tmp")"

###############################################################################
# The checks that already guarded the install must still stop it before
# anything, notices included, is written.
new_case tampered "a download that does not match its checksum"
run_install "$A/full1.tar.gz" "FAKE_SUMS_ARCHIVE=$A/full2.tar.gz"
equals "exit status" 1 "$RC"
check "the checksum failure is reported" has "$CASE/err" "Checksum verification failed"
equals "nothing was created in HOME" "" "$(entries "$HOME_DIR")"
equals "the temporary directory was cleaned up" "" "$(entries "$CASE/tmp")"

new_case missing-binary "an archive without cvc-lsp but with notices"
run_install "$A/no-lsp.tar.gz"
equals "exit status" 1 "$RC"
check "the missing binary is reported" has "$CASE/err" "Failed to extract safe binary: cvc-lsp"
equals "nothing was created in HOME" "" "$(entries "$HOME_DIR")"

new_case unsafe-binary "an existing cvc that is a directory, so the install is refused"
mkdir -p "$HOME_DIR/.cvc/bin/cvc"
run_install "$A/full1.tar.gz"
equals "exit status" 1 "$RC"
check "the refusal is reported" has "$CASE/err" "Refusing unsafe existing target: $HOME_DIR/.cvc/bin/cvc"
check "no notices were installed" test ! -e "$HOME_DIR/.cvc/share"
equals "the bin directory holds only the directory that was there" "cvc" "$(entries "$HOME_DIR/.cvc/bin")"

new_case locked "another installation holding the lock"
mkdir -p "$HOME_DIR/.cvc/bin/.cvc-install.lock"
run_install "$A/full1.tar.gz"
equals "exit status" 1 "$RC"
check "the lock is reported" has "$CASE/err" "Another installation is active"
check "no notices were installed" test ! -e "$HOME_DIR/.cvc/share"
equals "the lock is left to its owner" ".cvc-install.lock" "$(entries "$HOME_DIR/.cvc/bin")"

###############################################################################
printf '\n'
if [ "$FAILURES" -gt 0 ]; then
  echo "$FAILURES of $CHECKS checks FAILED."
  exit 1
fi
echo "All $CHECKS checks passed."

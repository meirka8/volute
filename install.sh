#!/bin/sh
set -eu
umask 077

# Override these for a GitHub Enterprise mirror or a fork. The default is the
# public, self-contained release distribution.
REPO="${CVC_RELEASE_REPOSITORY:-meirka8/volute}"
RELEASE_BASE_URL="${CVC_RELEASE_BASE_URL:-https://github.com}"
INSTALL_DIR="${CVC_INSTALL_DIR:-$HOME/.cvc/bin}"
RELEASE_VERSION="${CVC_RELEASE_VERSION:-latest}"

case "$REPO" in
  *[!A-Za-z0-9_.\/-]*|*..*|/*|*/|*/*/*) echo "CVC_RELEASE_REPOSITORY must be a safe owner/repository name" >&2; exit 1 ;;
  */?*) ;;
  *) echo "CVC_RELEASE_REPOSITORY must be owner/repository" >&2; exit 1 ;;
esac
case "$RELEASE_BASE_URL" in
  *[!A-Za-z0-9._:\/\-]*) echo "CVC_RELEASE_BASE_URL contains unsafe URL characters" >&2; exit 1 ;;
  https://*) ;;
  *) echo "CVC_RELEASE_BASE_URL must use HTTPS" >&2; exit 1 ;;
esac
case "$INSTALL_DIR" in
  *[!A-Za-z0-9_./\ -]*) echo "CVC_INSTALL_DIR contains unsafe characters" >&2; exit 1 ;;
  /*) ;;
  *) echo "CVC_INSTALL_DIR must be an absolute path" >&2; exit 1 ;;
esac
# The release archive also carries LICENSE and THIRD-PARTY-NOTICES.md. They are
# installed into a share directory beside the install directory: ~/.cvc/bin
# gives ~/.cvc/share/cvc, and a custom <prefix>/bin gives <prefix>/share/cvc.
SHARE_PARENT=$(dirname "$INSTALL_DIR")
SHARE_DIR="${SHARE_PARENT%/}/share/cvc"
# Release tags are `v` plus SemVer, with an optional prerelease and no build
# metadata. This is deliberately the same tag shape used by the release gate;
# accepting arbitrary paths here would turn a version override into a URL
# injection surface.
if [ "$RELEASE_VERSION" != "latest" ]; then
  printf '%s\n' "$RELEASE_VERSION" | awk '
    $0 ~ /^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$/ { valid = 1 }
    END { exit(valid ? 0 : 1) }
  ' || { echo "CVC_RELEASE_VERSION must be exactly v<semver> (without build metadata), or unset." >&2; exit 1; }
fi
RELEASE_BASE_URL=${RELEASE_BASE_URL%/}
BASE_AUTHORITY=${RELEASE_BASE_URL#https://}
BASE_AUTHORITY=${BASE_AUTHORITY%%/*}
[ -n "$BASE_AUTHORITY" ] || { echo "CVC_RELEASE_BASE_URL must include a host" >&2; exit 1; }
BASE_ORIGIN="https://$BASE_AUTHORITY"

check_redirect_target() {
  case "$BASE_ORIGIN:$1" in
    https://github.com:https://github.com/*|https://github.com:https://*.githubusercontent.com/*) return 0 ;;
  esac
  case "$1" in "$BASE_ORIGIN"/*) return 0 ;; esac
  echo "Refusing release redirect outside the trusted origin: $1" >&2
  return 1
}

# LICENSE and THIRD-PARTY-NOTICES.md are documentation, so nothing about them may
# keep the verified binaries from being installed: a problem is reported on
# stderr and the install goes on without them.
notices_warn() {
  echo "Warning: the license and third-party notices were not installed: $1" >&2
}

# Streams one optional member of the release archive into $STAGE the way the
# binaries are streamed below: into an installer-created regular file, so links
# and traversal entries in the archive are never materialized. It succeeds only
# for a non-empty regular file under a far tighter `ulimit -f` bound than the
# binaries get (16 MiB, or 32 MiB in shells that count 1024-byte blocks; the
# notices are far smaller) and otherwise leaves nothing behind. Releases that
# predate the notices fail here and are installed without them.
stage_notice() {
  if (ulimit -f 32768; tar -xOzf "$ARCHIVE" -- "$1") > "$STAGE/$1" 2>/dev/null \
    && [ -s "$STAGE/$1" ] && [ -f "$STAGE/$1" ] && [ ! -L "$STAGE/$1" ]; then
    return 0
  fi
  rm -f "$STAGE/$1"
  return 1
}

# Installs the notices staged by stage_notice into $SHARE_DIR. The checks come
# first and are all or nothing: an unsafe directory or an unsafe existing target
# stops it before anything is replaced. The files are copied into a private
# directory inside $SHARE_DIR and renamed into place, as the binaries are, so a
# re-run replaces the one copy of each file in place and never leaves a second
# one. On any problem it warns and returns non-zero; the caller carries on.
publish_notices() {
  if [ -L "$SHARE_DIR" ]; then
    notices_warn "refusing symlink directory: $SHARE_DIR"
    return 1
  fi
  if ! mkdir -p "$SHARE_DIR"; then
    notices_warn "cannot create the directory $SHARE_DIR"
    return 1
  fi
  if [ ! -d "$SHARE_DIR" ] || [ -L "$SHARE_DIR" ]; then
    notices_warn "not a safe directory: $SHARE_DIR"
    return 1
  fi
  for notice in LICENSE THIRD-PARTY-NOTICES.md; do
    [ -f "$STAGE/$notice" ] || continue
    notice_target="$SHARE_DIR/$notice"
    if [ -e "$notice_target" ] && { [ ! -f "$notice_target" ] || [ -L "$notice_target" ]; }; then
      notices_warn "refusing unsafe existing target: $notice_target"
      return 1
    fi
  done
  if ! NOTICE_STAGE=$(mktemp -d "$SHARE_DIR/.cvc-install.XXXXXX"); then
    notices_warn "cannot create a staging directory in $SHARE_DIR"
    return 1
  fi
  for notice in LICENSE THIRD-PARTY-NOTICES.md; do
    [ -f "$STAGE/$notice" ] || continue
    if ! cp "$STAGE/$notice" "$NOTICE_STAGE/$notice" || ! chmod 0644 "$NOTICE_STAGE/$notice"; then
      notices_warn "cannot copy $notice into $SHARE_DIR"
      return 1
    fi
  done
  for notice in LICENSE THIRD-PARTY-NOTICES.md; do
    [ -f "$NOTICE_STAGE/$notice" ] || continue
    if ! mv -f "$NOTICE_STAGE/$notice" "$SHARE_DIR/$notice"; then
      notices_warn "cannot replace $SHARE_DIR/$notice"
      return 1
    fi
  done
  rmdir "$NOTICE_STAGE" 2>/dev/null || rm -rf "$NOTICE_STAGE"
  NOTICE_STAGE=
  return 0
}

cleanup() {
  [ -n "${PUBLISH_STAGE:-}" ] && rm -rf "$PUBLISH_STAGE"
  [ -n "${NOTICE_STAGE:-}" ] && rm -rf "$NOTICE_STAGE"
  [ -n "${LOCK_DIR:-}" ] && rmdir "$LOCK_DIR" 2>/dev/null || true
  [ -n "${TMP_DIR:-}" ] && rm -rf "$TMP_DIR"
}
trap cleanup EXIT HUP INT TERM

echo "Installing CVC..."
OS="$(uname -s)"
ARCH="$(uname -m)"
case "$OS" in
  Linux) OS_TAG="unknown-linux-gnu" ;;
  Darwin) OS_TAG="apple-darwin" ;;
  *) echo "Unsupported OS: $OS" >&2; exit 1 ;;
esac
case "$OS:$ARCH" in
  Linux:x86_64|Linux:amd64) ARCH_TAG="x86_64" ;;
  Darwin:x86_64|Darwin:amd64) ARCH_TAG="x86_64" ;;
  Darwin:arm64) ARCH_TAG="aarch64" ;;
  Linux:aarch64|Linux:arm64) echo "Unsupported architecture: Linux arm64 releases are not published." >&2; exit 1 ;;
  *) echo "Unsupported architecture: $ARCH" >&2; exit 1 ;;
esac

ASSET_NAME="cvc-${ARCH_TAG}-${OS_TAG}.tar.gz"
if [ "$RELEASE_VERSION" = "latest" ]; then
  RELEASE_URL="$RELEASE_BASE_URL/$REPO/releases/latest/download"
else
  RELEASE_URL="$RELEASE_BASE_URL/$REPO/releases/download/$RELEASE_VERSION"
fi
ARCHIVE_URL="$RELEASE_URL/$ASSET_NAME"
CHECKSUM_URL="$RELEASE_URL/SHA256SUMS.txt"
TMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/cvc-install.XXXXXX")
ARCHIVE="$TMP_DIR/$ASSET_NAME"
CHECKSUMS="$TMP_DIR/SHA256SUMS"
STAGE="$TMP_DIR/stage"

command -v curl >/dev/null 2>&1 || { echo "curl is required to install CVC." >&2; exit 1; }
command -v tar >/dev/null 2>&1 || { echo "tar is required to install CVC." >&2; exit 1; }
if command -v sha256sum >/dev/null 2>&1; then HASH_CMD=sha256sum
elif command -v shasum >/dev/null 2>&1; then HASH_CMD=shasum
else echo "sha256sum or shasum is required to verify CVC." >&2; exit 1; fi

echo "Detected platform: $OS ($ARCH_TAG)"
echo "Downloading CVC release from $REPO..."
ARCHIVE_EFFECTIVE=$(curl --fail --location --max-redirs 5 --max-filesize 268435456 --proto '=https' --proto-redir '=https' --tlsv1.2 --retry 3 --output "$ARCHIVE" --write-out '%{url_effective}' "$ARCHIVE_URL")
check_redirect_target "$ARCHIVE_EFFECTIVE"
CHECKSUM_EFFECTIVE=$(curl --fail --location --max-redirs 5 --max-filesize 1048576 --proto '=https' --proto-redir '=https' --tlsv1.2 --retry 3 --output "$CHECKSUMS" --write-out '%{url_effective}' "$CHECKSUM_URL")
check_redirect_target "$CHECKSUM_EFFECTIVE"

EXPECTED=$(awk -v asset="$ASSET_NAME" '$1 ~ /^[[:xdigit:]]{64}$/ && ($2 == asset || $2 == "*" asset) { count++; value=$1 } END { if (count == 1) print value; else exit 1 }' "$CHECKSUMS") || {
  echo "SHA256SUMS does not contain one valid checksum for $ASSET_NAME." >&2; exit 1;
}
if [ "$HASH_CMD" = sha256sum ]; then ACTUAL=$(sha256sum "$ARCHIVE" | awk '{print $1}')
else ACTUAL=$(shasum -a 256 "$ARCHIVE" | awk '{print $1}')
fi
[ "$EXPECTED" = "$ACTUAL" ] || { echo "Checksum verification failed for $ASSET_NAME." >&2; exit 1; }

mkdir "$STAGE"
for binary in cvc cvc-mcp cvc-lsp; do
  # Stream only expected members into installer-created regular files. Archive
  # traversal entries, links, and unrelated members are never materialized.
  (ulimit -f 262144; tar -xOzf "$ARCHIVE" -- "$binary") > "$STAGE/$binary" || { echo "Failed to extract safe binary: $binary" >&2; exit 1; }
  [ -s "$STAGE/$binary" ] && [ -f "$STAGE/$binary" ] && [ ! -L "$STAGE/$binary" ] || { echo "Release archive is missing safe binary: $binary" >&2; exit 1; }
done
# LICENSE and THIRD-PARTY-NOTICES.md come out of the archive the same way, but
# they are optional: an older release that lacks them is installed without.
NOTICES_STAGED=""
NOTICES_ABSENT=""
for notice in LICENSE THIRD-PARTY-NOTICES.md; do
  if stage_notice "$notice"; then NOTICES_STAGED="${NOTICES_STAGED:+$NOTICES_STAGED, }$notice"
  else NOTICES_ABSENT="${NOTICES_ABSENT:+$NOTICES_ABSENT, }$notice"; fi
done

[ ! -L "$INSTALL_DIR" ] || { echo "Refusing symlink installation directory: $INSTALL_DIR" >&2; exit 1; }
mkdir -p "$INSTALL_DIR"
[ -d "$INSTALL_DIR" ] && [ ! -L "$INSTALL_DIR" ] || { echo "CVC_INSTALL_DIR is not a safe directory" >&2; exit 1; }
LOCK_PATH="$INSTALL_DIR/.cvc-install.lock"
mkdir "$LOCK_PATH" 2>/dev/null || { echo "Another installation is active (or a stale lock exists): $LOCK_PATH" >&2; exit 1; }
LOCK_DIR="$LOCK_PATH"
PUBLISH_STAGE=$(mktemp -d "$INSTALL_DIR/.cvc-install.XXXXXX")
for binary in cvc cvc-mcp cvc-lsp; do
  target="$INSTALL_DIR/$binary"
  [ ! -e "$target" ] || { [ -f "$target" ] && [ ! -L "$target" ]; } || { echo "Refusing unsafe existing target: $target" >&2; exit 1; }
  cp "$STAGE/$binary" "$PUBLISH_STAGE/$binary"
  chmod 0755 "$PUBLISH_STAGE/$binary"
done
# The notices go in under the same lock, after every binary target has been
# checked and staged and before the first binary is replaced, so a refused
# install has changed no notice either. They cannot stop the binaries: a problem
# is warned about (see publish_notices) and reported in the summary.
NOTICES_INSTALLED=""
NOTICES_FAILED=""
if [ -n "$NOTICES_STAGED" ]; then
  if publish_notices; then
    NOTICES_INSTALLED=$NOTICES_STAGED
  else
    NOTICES_FAILED=$NOTICES_STAGED
    if [ -n "${NOTICE_STAGE:-}" ]; then rm -rf "$NOTICE_STAGE"; NOTICE_STAGE=; fi
  fi
fi
# cvc moves into place LAST: the npm launcher probes `cvc --version` without
# the lock, so a new cvc must imply its siblings are already new.
for binary in cvc-lsp cvc-mcp cvc; do
  mv -f "$PUBLISH_STAGE/$binary" "$INSTALL_DIR/$binary"
done
# Release the shared lock now so the interactive PATH prompt below does not
# block the npm launcher from publishing. The trap still covers failure paths.
rmdir "$PUBLISH_STAGE" 2>/dev/null || rm -rf "$PUBLISH_STAGE"
PUBLISH_STAGE=
rmdir "$LOCK_DIR" 2>/dev/null || true
LOCK_DIR=

echo ""
echo "Success! CVC installed to $INSTALL_DIR"
echo "  cvc       — CLI interface"
echo "  cvc-mcp   — MCP server for coding agents"
echo "  cvc-lsp   — Language server for the VSCode extension"
echo ""
if [ -n "$NOTICES_INSTALLED" ]; then
  echo "License and third-party notices installed to $SHARE_DIR"
  echo "  $NOTICES_INSTALLED"
  echo ""
fi
if [ -n "$NOTICES_ABSENT" ]; then
  echo "Not included in this release archive, so not installed: $NOTICES_ABSENT"
  echo ""
fi
if [ -n "$NOTICES_FAILED" ]; then
  echo "Not installed (see the warning above): $NOTICES_FAILED"
  echo ""
fi
echo "Please manually add $INSTALL_DIR to your PATH:"
echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
echo ""

# Prompt only when /dev/tty can actually be opened. A process without a
# controlling terminal (CI, cron, `setsid`, a container without a tty) can have
# /dev/tty present as a device node that fails with ENXIO on open, and under
# `set -e` a failing `read` would otherwise exit 1 after a successful install.
if (: </dev/tty) 2>/dev/null; then
  printf 'Would you like to automatically add CVC to your PATH? [Y/n] '
  read -r ADD_PATH </dev/tty || ADD_PATH="n"
else ADD_PATH="n"; fi
case "$ADD_PATH" in
  [Nn]*) echo "Skipping PATH configuration." ;;
  *)
    SHELL_RC=""
    CURRENT_SHELL=$(basename "${SHELL:-}")
    if [ "$CURRENT_SHELL" = zsh ] && [ -f "$HOME/.zshrc" ]; then SHELL_RC="$HOME/.zshrc"
    elif [ "$CURRENT_SHELL" = bash ] && [ -f "$HOME/.bashrc" ]; then SHELL_RC="$HOME/.bashrc"
    elif [ -f "$HOME/.zshrc" ]; then SHELL_RC="$HOME/.zshrc"
    elif [ -f "$HOME/.bashrc" ]; then SHELL_RC="$HOME/.bashrc"; fi
    if [ -n "$SHELL_RC" ]; then
      printf '\n# CVC CLI and MCP Binaries\nexport PATH="%s:$PATH"\n' "$INSTALL_DIR" >> "$SHELL_RC"
      echo "Added $INSTALL_DIR to your PATH in $SHELL_RC"
    else echo "Could not detect a shell config file; add $INSTALL_DIR to PATH manually."; fi ;;
esac
echo ""
echo "MCP config: {\"cvc\": {\"command\": \"$INSTALL_DIR/cvc-mcp\", \"args\": []}}"

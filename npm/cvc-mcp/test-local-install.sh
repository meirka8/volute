#!/bin/bash
set -e

LOCAL_INSTALL=false
if [ "$1" == "--local" ]; then
    LOCAL_INSTALL=true
fi

echo "🧹 Cleaning up global cvc-mcp installation..."
npm uninstall -g @volute_cvc/cvc-mcp || true

if [ "$LOCAL_INSTALL" = true ]; then
    echo "📦 Packing local npm package..."
    cd "$(dirname "$0")"
    TARBALL=$(npm pack)

    echo "🚀 Installing local tarball globally..."
    npm install -g "./$TARBALL"
else
    echo "🚀 Installing from NPM registry..."
    npm install -g @volute_cvc/cvc-mcp
fi

echo "✅ Installed successfully!"

# The launcher publishes into ~/.cvc/bin and skips the download whenever that
# directory already holds the same or a newer release, so run the smoke test
# under a throwaway HOME: it has no ~/.cvc, so the launcher must download the
# GitHub release matching the package version and publish it there, and the
# real ~/.cvc (including hand-installed binaries) stays untouched. Consequently
# the smoke test only passes when that release exists, which is not the case for
# a --local build of a version that has not been released yet. The launcher reads
# CVC_RELEASE_REPOSITORY / CVC_RELEASE_BASE_URL from the environment, so a mirror
# that has the release can be used instead.
# Run the npm-installed shim by absolute path: a hand-installed ~/.cvc/bin/cvc
# earlier on PATH must not be able to satisfy the smoke test.
npm_bin="$(npm prefix -g)/bin"
if [ ! -x "$npm_bin/cvc" ]; then
    echo "❌ Expected the npm-installed cvc at $npm_bin/cvc, but it is missing or not executable."
    exit 1
fi
echo "🔎 Smoke test: $npm_bin/cvc --version under a throwaway HOME (your real ~/.cvc is not touched)..."
smoke_home=$(mktemp -d)
trap 'rm -rf "$smoke_home"' EXIT
smoke_status=0
HOME="$smoke_home" "$npm_bin/cvc" --version || smoke_status=$?
rm -rf "$smoke_home"
trap - EXIT
if [ "$smoke_status" -ne 0 ]; then
    echo "❌ Smoke test failed: cvc --version exited with status $smoke_status."
    echo "   The npm package installed, but a throwaway HOME makes the launcher download the"
    echo "   release matching this package version. If that download failed (for example a 404),"
    echo "   the smoke test needs the GitHub release for this package version to exist, or"
    echo "   CVC_RELEASE_BASE_URL / CVC_RELEASE_REPOSITORY set to a mirror that has it."
    exit "$smoke_status"
fi

echo "✅ Smoke test passed!"
echo "To test the MCP server, start cvc-mcp from your MCP client inside a repository."
echo "It is not run here: it serves MCP on stdio and exits 1 outside a repository."

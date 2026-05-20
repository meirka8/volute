#!/bin/bash
set -e

LOCAL_INSTALL=false
if [ "$1" == "--local" ]; then
    LOCAL_INSTALL=true
fi

echo "🧹 Cleaning up global cvc-mcp installation..."
npm uninstall -g @volute_cvc/cvc-mcp || true

echo "🗑️ Removing downloaded binary cache..."
rm -rf ~/.cvc/mcp-cache

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
echo "You can now test the CLI by running: cvc --version"
echo "And test the MCP server by running: cvc-mcp"

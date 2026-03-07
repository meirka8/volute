#!/bin/sh
set -e

# Configuration
REPO="meirka8/cvc"
INSTALL_DIR="$HOME/.cvc/bin"

echo "Installing CVC..."

# Detect OS and Arch
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
    Linux)
        OS_TAG="unknown-linux-gnu"
        ;;
    Darwin)
        OS_TAG="apple-darwin"
        ;;
    *)
        echo "Unsupported OS: $OS"
        exit 1
        ;;
esac

case "$ARCH" in
    x86_64)
        ARCH_TAG="x86_64"
        ;;
    arm64|aarch64)
        ARCH_TAG="aarch64"
        ;;
    *)
        echo "Unsupported Architecture: $ARCH"
        exit 1
        ;;
esac

ASSET_NAME="cvc-${ARCH_TAG}-${OS_TAG}.tar.gz"

echo "Detected Platform: $OS ($OS_TAG) $ARCH ($ARCH_TAG)"
echo "Target Asset: $ASSET_NAME"

# Create install directory
mkdir -p "$INSTALL_DIR"

# Download latest release
DOWNLOAD_URL="https://github.com/$REPO/releases/latest/download/$ASSET_NAME"

echo "Downloading from $DOWNLOAD_URL..."
# curl -L -o "/tmp/$ASSET_NAME" "$DOWNLOAD_URL"

# Extract (release archive contains both cvc and cvc-mcp binaries)
# tar -xzf "/tmp/$ASSET_NAME" -C "$INSTALL_DIR"
# chmod +x "$INSTALL_DIR/cvc"
# chmod +x "$INSTALL_DIR/cvc-mcp"

# Since we don't have actual releases yet, simulate installation.
echo "NOTE: release download is commented out until releases exist."
echo "Simulating installation..."
touch "$INSTALL_DIR/cvc"
chmod +x "$INSTALL_DIR/cvc"
touch "$INSTALL_DIR/cvc-mcp"
chmod +x "$INSTALL_DIR/cvc-mcp"

echo ""
echo "Success! CVC installed to $INSTALL_DIR"
echo "  cvc       — CLI interface"
echo "  cvc-mcp   — MCP server for coding agents"
echo ""
echo "Please add $INSTALL_DIR to your PATH:"
echo "  export PATH=\"\$HOME/.cvc/bin:\$PATH\""
echo ""

# Optional: MCP client configuration
echo "To use CVC with a coding agent (Claude, Cursor, Windsurf, etc.),"
echo "add the following to your MCP client config:"
echo ""
echo "  {\"cvc\": {\"command\": \"$INSTALL_DIR/cvc-mcp\", \"args\": []}}"
echo ""

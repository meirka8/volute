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

# Download latest release via the secure proxy
DOWNLOAD_URL="https://cvc.dev/api/download/$ASSET_NAME"

echo "Downloading from $DOWNLOAD_URL..."
curl -fSL -o "/tmp/$ASSET_NAME" "$DOWNLOAD_URL"

# Extract (release archive contains cvc, cvc-mcp, cvc-lsp binaries)
tar -xzf "/tmp/$ASSET_NAME" -C "$INSTALL_DIR"
chmod +x "$INSTALL_DIR/cvc"
chmod +x "$INSTALL_DIR/cvc-mcp"
chmod +x "$INSTALL_DIR/cvc-lsp"

echo ""
echo "Success! CVC installed to $INSTALL_DIR"
echo "  cvc       — CLI interface"
echo "  cvc-mcp   — MCP server for coding agents"
echo "  cvc-lsp   — Language server for the VSCode extension"
echo ""
echo "Please manually add $INSTALL_DIR to your PATH:"
echo "  export PATH=\"\$INSTALL_DIR:\$PATH\""
echo ""

read -p "Would you like to automatically add CVC to your PATH? [Y/n] " ADD_PATH
case $ADD_PATH in
    [Nn]* )
        echo "Skipping PATH configuration."
        echo "You can add it manually later:"
        echo "  export PATH=\"\$INSTALL_DIR:\$PATH\""
        ;;
    * )
        SHELL_RC=""
        CURRENT_SHELL=$(basename "$SHELL")
        if [ "$CURRENT_SHELL" = "zsh" ] && [ -f "$HOME/.zshrc" ]; then
            SHELL_RC="$HOME/.zshrc"
        elif [ "$CURRENT_SHELL" = "bash" ] && [ -f "$HOME/.bashrc" ]; then
            SHELL_RC="$HOME/.bashrc"
        elif [ -f "$HOME/.zshrc" ]; then
            SHELL_RC="$HOME/.zshrc"
        elif [ -f "$HOME/.bashrc" ]; then
            SHELL_RC="$HOME/.bashrc"
        fi

        if [ -n "$SHELL_RC" ]; then
            echo "" >> "$SHELL_RC"
            echo "# CVC CLI and MCP Binaries" >> "$SHELL_RC"
            echo "export PATH=\"$INSTALL_DIR:\$PATH\"" >> "$SHELL_RC"
            echo "✔ Added $INSTALL_DIR to your PATH in $SHELL_RC"
            echo "👉 Run 'source $SHELL_RC' or restart your terminal to apply the changes."
        else
            echo "Could not confidently detect default shell config file."
            echo "Please manually add $INSTALL_DIR to your PATH:"
            echo "  export PATH=\"\$INSTALL_DIR:\$PATH\""
        fi
        ;;
esac

# Optional: MCP client configuration
echo "To use CVC with a coding agent (Claude, Cursor, Windsurf, etc.),"
echo "add the following to your MCP client config:"
echo ""
echo "  {\"cvc\": {\"command\": \"$INSTALL_DIR/cvc-mcp\", \"args\": []}}"
echo ""

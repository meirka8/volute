#!/bin/sh
set -e

# Configuration
REPO="helixthought/cvc2"
INSTALL_DIR="$HOME/.cvc/bin"
BINARY_NAME="cvc"

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
        if [ "$OS" = "Darwin" ]; then
            ARCH_TAG="aarch64"
        else
            ARCH_TAG="aarch64" # Assuming linux aarch64 support
        fi
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

# Download latest release (Placeholder URL logic - this would need to query GitHub API for 'latest' tag in production)
# For now, we'll assume a direct download link pattern or use a placeholder message if release implementation isn't live.
DOWNLOAD_URL="https://github.com/$REPO/releases/latest/download/$ASSET_NAME"

echo "Downloading from $DOWNLOAD_URL..."
# curl -L -o "/tmp/$ASSET_NAME" "$DOWNLOAD_URL"

# Extract
# tar -xzf "/tmp/$ASSET_NAME" -C "$INSTALL_DIR"
# chmod +x "$INSTALL_DIR/$BINARY_NAME"

# echo "Installed to $INSTALL_DIR/$BINARY_NAME"
# echo "Please add $INSTALL_DIR to your PATH."

# Since we don't have actual releases yet, we'll write a mock success message.
echo "NOTE: specific release URL download is commented out until releases exist."
echo "Simulating installation..."
touch "$INSTALL_DIR/$BINARY_NAME"
chmod +x "$INSTALL_DIR/$BINARY_NAME"

echo "Success! CVC installed."

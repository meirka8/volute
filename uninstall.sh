#!/bin/sh

echo "Uninstalling CVC..."

# Remove binaries
if [ -d "$HOME/.cvc" ]; then
    echo "Removing CVC binaries at $HOME/.cvc..."
    rm -rf "$HOME/.cvc"
fi

# Determine specific paths
OS="$(uname -s)"
if [ "$OS" = "Darwin" ]; then
    PATHS_TO_REMOVE="
$HOME/Library/Application Support/com.helixthought.cvc
$HOME/Library/Caches/com.helixthought.cvc
$HOME/Library/Preferences/com.helixthought.cvc
"
else
    PATHS_TO_REMOVE="
$HOME/.local/share/cvc
$HOME/.config/cvc
$HOME/.cache/cvc
"
fi

for p in $PATHS_TO_REMOVE; do
    if [ -d "$p" ]; then
        echo "Removing data directory: $p"
        rm -rf "$p"
    fi
done

echo ""
echo "CVC has been successfully uninstalled from your user profile."
echo "Note: If you have initialized CVC in any local Git repositories, the .git/cvc databases and hooks still exist in those specific directories. You can safely delete them manually if desired."

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
$HOME/Library/Application Support/dev.volute.cvc
$HOME/Library/Caches/dev.volute.cvc
$HOME/Library/Preferences/dev.volute.cvc
"
else
    PATHS_TO_REMOVE="
$HOME/.local/share/cvc
$HOME/.config/cvc
$HOME/.cache/cvc
"
fi

echo "$PATHS_TO_REMOVE" | while IFS= read -r p; do
    [ -z "$p" ] && continue
    if [ -d "$p" ]; then
        echo "Removing data directory: $p"
        rm -rf "$p"
    fi
done

echo ""
echo "CVC has been successfully uninstalled from your user profile."
echo "Note: Repository CVC state is intentionally left in place. In a repository, git rev-parse --git-common-dir identifies its common Git directory; CVC data is in its cvc/ directory and is shared by linked worktrees."
echo "CVC refs (refs/cvc), related Git objects/reflogs, and hooks may also remain. Hooks use Git's effective hooks path (the common directory's hooks/ by default, or core.hooksPath). Cleanup, if wanted, is manual."

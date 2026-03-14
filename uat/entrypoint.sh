#!/bin/bash
set -e

# Clean up any stale VNC locks
rm -f /tmp/.X1-lock /tmp/.X11-unix/X1

# Start VNC server
vncserver :1 -geometry 1280x800 -depth 24 -localhost no

# Start noVNC (browser-accessible VNC)
websockify --web /usr/share/novnc 6080 localhost:5901 &

# Install the extension from the mounted source (fresh every time)
if [ -d /workspace/extension ]; then
    cd /workspace/extension
    # Package and install it, or install from VSIX if pre-built
    if [ -f *.vsix ]; then
        code --user-data-dir /tmp/vscode-user --extensions-dir /tmp/vscode-ext \
             --install-extension *.vsix --no-sandbox
    fi
fi

echo "✅ Desktop ready at http://localhost:6080/vnc.html"

# Keep container alive
tail -f /dev/null
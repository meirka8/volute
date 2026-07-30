#!/bin/bash
set -euo pipefail

PASSWORD_FILE=/run/secrets/vnc_password
if [[ ! -f "$PASSWORD_FILE" ]]; then
  echo "Missing Docker secret: vnc_password" >&2
  exit 1
fi
VNC_PASSWORD=$(<"$PASSWORD_FILE")
if [[ ! "$VNC_PASSWORD" =~ ^[A-Za-z0-9]{8}$ ]]; then
  echo "The UAT VNC password must be exactly 8 ASCII letters/digits." >&2
  exit 1
fi
mkdir -p "$HOME/.vnc"
printf '%s\n' "$VNC_PASSWORD" | vncpasswd -f > "$HOME/.vnc/passwd"
chmod 600 "$HOME/.vnc/passwd"
unset VNC_PASSWORD

# Clean up any stale VNC locks
rm -f /tmp/.X1-lock /tmp/.X11-unix/X1

# Keep VNC private to the container; Docker publishes only the noVNC port to
# the host loopback interface.
vncserver :1 -geometry 1280x800 -depth 24 -localhost yes

# Start noVNC (browser-accessible VNC)
websockify --web /usr/share/novnc 6080 localhost:5901 &
WEBSOCKIFY_PID=$!
trap 'kill "$WEBSOCKIFY_PID" 2>/dev/null || true; vncserver -kill :1 >/dev/null 2>&1 || true' EXIT INT TERM

# The image build packages the repository's cvc-plugin into this VSIX. Installing
# it at startup keeps each tmpfs-backed VS Code profile clean and repeatable.
code --user-data-dir /tmp/vscode-user --extensions-dir /tmp/vscode-ext \
     --install-extension /opt/cvc/cvc.vsix --no-sandbox

echo "✅ Desktop ready at http://localhost:6080/vnc.html"

# Keep the container tied to the proxy rather than an unrelated tail process.
wait "$WEBSOCKIFY_PID"

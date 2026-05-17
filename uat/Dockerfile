FROM ubuntu:22.04

ENV DEBIAN_FRONTEND=noninteractive

# Desktop environment + noVNC deps
RUN apt-get update && apt-get install -y \
    xfce4 xfce4-terminal \
    tigervnc-standalone-server tigervnc-tools \
    novnc websockify \
    wget curl git sudo \
    libgtk-3-0 libnotify4 libgconf-2-4 \
    libnss3 libxss1 libxtst6 xdg-utils \
    libatspi2.0-0 libdrm2 libgbm1 libxcb-dri3-0 \
    && rm -rf /var/lib/apt/lists/*

# Install Node.js & npm (v20)
RUN curl -fsSL https://deb.nodesource.com/setup_20.x | bash - && \
    apt-get install -y nodejs && \
    rm -rf /var/lib/apt/lists/*

# Install AI CLI Agents
RUN npm install -g @anthropic-ai/claude-code @google/gemini-cli opencode-ai

# Install VS Code
RUN wget -qO /tmp/vscode.deb \
    "https://code.visualstudio.com/sha/download?build=stable&os=linux-deb-x64" \
    && dpkg -i /tmp/vscode.deb || apt-get install -yf \
    && rm /tmp/vscode.deb

# Create a non-root user (VS Code is unhappy as root)
RUN useradd -m -s /bin/bash dev && echo "dev:dev" | chpasswd && adduser dev sudo

RUN echo '#!/bin/bash\ncode --user-data-dir /tmp/vscode-user --extensions-dir /tmp/vscode-ext --no-sandbox "$@"' \
    > /usr/local/bin/vscode-clean && chmod +x /usr/local/bin/vscode-clean

USER dev
WORKDIR /home/dev

# VNC setup
RUN mkdir -p ~/.vnc && \
    echo "password" | vncpasswd -f > ~/.vnc/passwd && \
    chmod 600 ~/.vnc/passwd

COPY --chown=dev:dev entrypoint.sh /home/dev/entrypoint.sh    
RUN chmod +x /home/dev/entrypoint.sh

EXPOSE 6080

ENTRYPOINT ["/home/dev/entrypoint.sh"]
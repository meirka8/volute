FROM ubuntu:22.04

ENV DEBIAN_FRONTEND=noninteractive

# Desktop environment + noVNC deps
RUN apt-get update && apt-get install -y \
    xfce4 xfce4-terminal \
    tigervnc-standalone-server tigervnc-tools \
    novnc websockify \
    curl git ca-certificates gnupg \
    libgtk-3-0 libnotify4 libgconf-2-4 \
    libnss3 libxss1 libxtst6 xdg-utils \
    libatspi2.0-0 libdrm2 libgbm1 libxcb-dri3-0 \
    && rm -rf /var/lib/apt/lists/*

# Install Node.js 20 through an authenticated apt repository. Do not execute a
# remotely downloaded repository-setup shell script during the image build.
RUN mkdir -p /etc/apt/keyrings && \
    curl -fsSL https://deb.nodesource.com/gpgkey/nodesource-repo.gpg.key | gpg --dearmor -o /etc/apt/keyrings/nodesource.gpg && \
    echo "deb [signed-by=/etc/apt/keyrings/nodesource.gpg] https://deb.nodesource.com/node_20.x nodistro main" > /etc/apt/sources.list.d/nodesource.list && \
    apt-get update && apt-get install -y nodejs && \
    rm -rf /var/lib/apt/lists/*

# Install VS Code through Microsoft's authenticated apt repository rather than
# executing an unverified, mutable .deb download.
RUN curl -fsSL https://packages.microsoft.com/keys/microsoft.asc | gpg --dearmor -o /etc/apt/keyrings/microsoft.gpg && \
    echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/microsoft.gpg] https://packages.microsoft.com/repos/code stable main" > /etc/apt/sources.list.d/vscode.list && \
    apt-get update && apt-get install -y code && \
    rm -rf /var/lib/apt/lists/*

# Create an unprivileged user. The UAT desktop has no sudo/admin path.
RUN useradd -m -s /bin/bash dev

RUN echo '#!/bin/bash\ncode --user-data-dir /tmp/vscode-user --extensions-dir /tmp/vscode-ext --no-sandbox "$@"' \
    > /usr/local/bin/vscode-clean && chmod +x /usr/local/bin/vscode-clean

# Build a deterministic VSIX from the extension checked into this repository.
# VSCE is the lockfile-backed cvc-plugin dev dependency; npm exec must not fetch.
WORKDIR /opt/cvc-plugin
COPY cvc-plugin/package.json cvc-plugin/package-lock.json ./
RUN npm ci
COPY cvc-plugin/ ./
RUN mkdir -p /opt/cvc && npm run package && \
    npm exec --no -- vsce package --out /opt/cvc/cvc.vsix

USER dev
WORKDIR /home/dev

COPY --chown=root:root uat/entrypoint.sh /usr/local/bin/cvc-uat-entrypoint
USER root
RUN chmod 0755 /usr/local/bin/cvc-uat-entrypoint
USER dev

EXPOSE 6080

ENTRYPOINT ["/usr/local/bin/cvc-uat-entrypoint"]

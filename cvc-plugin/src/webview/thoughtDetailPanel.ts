import * as vscode from "vscode";
import { VoluteLanguageClient } from "../lsp/client";
import { InteractionDetail } from "../lsp/protocol";
import { marked } from "marked";

/**
 * Volute CVC Brand Colors
 * These are used as accents alongside VS Code's native theming
 */
const BRAND = {
  gitOrange: "#F05032", // Primary Accent, Git Actions, Highlights
  cognitiveNavy: "#0A192F", // Backgrounds, "Deep" UI elements
  electricTeal: "#64FFDA", // Success states, Links, "Augmentation"
  voidSlate: "#112240", // Secondary Backgrounds, Cards
  textWhite: "#E6F1FF", // Primary Text
  textMuted: "#8892B0", // Secondary Text
};

/**
 * Manages Thought Detail webview panels
 */
export class ThoughtDetailPanel {
  public static currentPanel: ThoughtDetailPanel | undefined;
  public static readonly viewType = "volute.thoughtDetail";

  private readonly panel: vscode.WebviewPanel;
  private readonly extensionUri: vscode.Uri;
  private readonly outputChannel: vscode.OutputChannel;
  private readonly lspClient: VoluteLanguageClient;
  private currentInteractionId: string | undefined;
  private disposables: vscode.Disposable[] = [];

  private constructor(
    panel: vscode.WebviewPanel,
    extensionUri: vscode.Uri,
    outputChannel: vscode.OutputChannel,
    lspClient: VoluteLanguageClient,
  ) {
    this.panel = panel;
    this.extensionUri = extensionUri;
    this.outputChannel = outputChannel;
    this.lspClient = lspClient;

    // Set up panel event handlers
    this.panel.onDidDispose(() => this.dispose(), null, this.disposables);

    // Handle messages from the webview
    this.panel.webview.onDidReceiveMessage(
      (message) => this.handleMessage(message),
      null,
      this.disposables,
    );
  }

  /**
   * Create or show the thought detail panel
   */
  public static createOrShow(
    extensionUri: vscode.Uri,
    outputChannel: vscode.OutputChannel,
    lspClient: VoluteLanguageClient,
    interactionId: string,
  ): ThoughtDetailPanel {
    const column = vscode.ViewColumn.Two;

    // If we already have a panel, show it in the target column
    if (ThoughtDetailPanel.currentPanel) {
      ThoughtDetailPanel.currentPanel.panel.reveal(column);
      ThoughtDetailPanel.currentPanel.loadInteraction(interactionId);
      return ThoughtDetailPanel.currentPanel;
    }

    // Create a new panel
    const panel = vscode.window.createWebviewPanel(
      ThoughtDetailPanel.viewType,
      "Thought Detail",
      column,
      {
        enableScripts: true,
        retainContextWhenHidden: true,
        localResourceRoots: [
          vscode.Uri.joinPath(
            extensionUri,
            "node_modules",
            "@vscode/webview-ui-toolkit",
          ),
          vscode.Uri.joinPath(extensionUri, "dist"),
        ],
      },
    );

    ThoughtDetailPanel.currentPanel = new ThoughtDetailPanel(
      panel,
      extensionUri,
      outputChannel,
      lspClient,
    );

    ThoughtDetailPanel.currentPanel.loadInteraction(interactionId);
    return ThoughtDetailPanel.currentPanel;
  }

  /**
   * Load and display an interaction
   */
  public async loadInteraction(interactionId: string): Promise<void> {
    this.currentInteractionId = interactionId;
    this.panel.title = "Loading...";

    // Show loading state
    this.panel.webview.html = this.getLoadingHtml();

    // Fetch interaction details
    const detail = await this.lspClient.sendInteractionGet({
      id: interactionId,
    });

    if (detail) {
      this.panel.title = this.truncateTitle(detail.userPrompt);
      this.panel.webview.html = this.getDetailHtml(detail);
    } else {
      this.panel.title = "Error";
      this.panel.webview.html = this.getErrorHtml(
        "Failed to load interaction details",
      );
    }
  }

  /**
   * Handle messages from the webview
   */
  private handleMessage(message: {
    command: string;
    [key: string]: unknown;
  }): void {
    switch (message.command) {
      case "refresh":
        if (this.currentInteractionId) {
          this.loadInteraction(this.currentInteractionId);
        }
        break;
      case "copyPrompt":
        if (message.text) {
          vscode.env.clipboard.writeText(message.text as string);
          vscode.window.showInformationMessage("Prompt copied to clipboard");
        }
        break;
      case "openFile":
        if (message.path) {
          const uri = vscode.Uri.file(message.path as string);
          vscode.window.showTextDocument(uri);
        }
        break;
    }
  }

  /**
   * Generate loading HTML
   */
  private getLoadingHtml(): string {
    return `<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline';">
    <title>Loading...</title>
    <style>
        body {
            font-family: var(--vscode-font-family, 'Inter', 'Segoe UI', sans-serif);
            padding: 20px;
            color: var(--vscode-foreground, ${BRAND.textWhite});
            background-color: var(--vscode-editor-background, ${BRAND.cognitiveNavy});
            display: flex;
            justify-content: center;
            align-items: center;
            height: 100vh;
            margin: 0;
        }
        .loading {
            text-align: center;
        }
        .spinner {
            width: 40px;
            height: 40px;
            border: 3px solid ${BRAND.electricTeal};
            border-top-color: transparent;
            border-radius: 50%;
            animation: spin 1s linear infinite;
            margin: 0 auto 16px;
        }
        @keyframes spin {
            to { transform: rotate(360deg); }
        }
    </style>
</head>
<body>
    <div class="loading">
        <div class="spinner"></div>
        <p>Loading thought details...</p>
    </div>
</body>
</html>`;
  }

  /**
   * Generate error HTML
   */
  private getErrorHtml(message: string): string {
    return `<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline';">
    <title>Error</title>
    <style>
        body {
            font-family: var(--vscode-font-family, 'Inter', 'Segoe UI', sans-serif);
            padding: 20px;
            color: var(--vscode-foreground, ${BRAND.textWhite});
            background-color: var(--vscode-editor-background, ${BRAND.cognitiveNavy});
        }
        .error {
            text-align: center;
            padding: 40px;
        }
        .error-icon {
            font-size: 48px;
            margin-bottom: 16px;
            color: ${BRAND.gitOrange};
        }
        .error-message {
            color: ${BRAND.gitOrange};
        }
    </style>
</head>
<body>
    <div class="error">
        <div class="error-icon">&#9888;</div>
        <p class="error-message">${this.escapeHtml(message)}</p>
    </div>
</body>
</html>`;
  }

  /**
   * Check if content is meaningful (not just whitespace, newlines, quotes, or special chars).
   * Returns true if the content has actual text worth displaying.
   */
  private hasContent(text: string | undefined | null): boolean {
    if (!text) {
      return false;
    }
    // Strip whitespace, newlines, quotes, and common "empty" characters
    const cleaned = text.replace(/[\s\n\r\t"'`]/g, "");
    return cleaned.length > 0;
  }

  /**
   * Generate the main detail HTML
   */
  private getDetailHtml(detail: InteractionDetail): string {
    // Only prepare HTML for sections with actual content
    const hasPrompt = this.hasContent(detail.userPrompt);
    const hasResponse = this.hasContent(detail.modelResponse);
    const hasThought = this.hasContent(detail.chainOfThought);

    const promptHtml = hasPrompt ? this.renderMarkdown(detail.userPrompt) : null;
    const responseHtml = hasResponse ? this.renderMarkdown(detail.modelResponse!) : null;
    const thoughtHtml = hasThought ? this.renderMarkdown(detail.chainOfThought!) : null;

    const timestamp = new Date(detail.timestamp * 1000).toLocaleString();
    const authorIcon = detail.author === "human" ? "&#128100;" : "&#129302;";
    const authorLabel =
      detail.author.charAt(0).toUpperCase() + detail.author.slice(1);

    return `<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline';">
    <title>Thought Detail</title>
    <style>
        /* Volute CVC Brand Colors */
        :root {
            --volute-git-orange: ${BRAND.gitOrange};
            --volute-cognitive-navy: ${BRAND.cognitiveNavy};
            --volute-electric-teal: ${BRAND.electricTeal};
            --volute-void-slate: ${BRAND.voidSlate};
            --volute-text-white: ${BRAND.textWhite};
            --volute-text-muted: ${BRAND.textMuted};

            /* Map to semantic variables with VS Code fallbacks */
            --section-bg: var(--vscode-editor-inactiveSelectionBackground, ${BRAND.voidSlate});
            --border-color: var(--vscode-panel-border, ${BRAND.textMuted}40);
            --code-bg: var(--vscode-textCodeBlock-background, ${BRAND.cognitiveNavy});
        }

        body {
            font-family: var(--vscode-font-family, 'Inter', 'Segoe UI', sans-serif);
            font-size: var(--vscode-font-size, 13px);
            padding: 0;
            margin: 0;
            color: var(--vscode-foreground, var(--volute-text-white));
            background-color: var(--vscode-editor-background, var(--volute-cognitive-navy));
            line-height: 1.6;
        }

        .container {
            max-width: 900px;
            margin: 0 auto;
            padding: 20px;
        }

        .header {
            display: flex;
            justify-content: space-between;
            align-items: center;
            margin-bottom: 20px;
            padding-bottom: 12px;
            border-bottom: 1px solid var(--border-color);
        }

        .meta {
            display: flex;
            gap: 16px;
            font-size: 12px;
            color: var(--vscode-descriptionForeground, var(--volute-text-muted));
        }

        .meta-item {
            display: flex;
            align-items: center;
            gap: 4px;
        }

        .actions {
            display: flex;
            gap: 8px;
        }

        button {
            background: var(--volute-electric-teal);
            color: var(--volute-cognitive-navy);
            border: none;
            padding: 6px 14px;
            border-radius: 2px;
            cursor: pointer;
            font-size: 12px;
            font-weight: 500;
            transition: opacity 0.2s;
        }

        button:hover {
            opacity: 0.85;
        }

        button.secondary {
            background: transparent;
            color: var(--volute-electric-teal);
            border: 1px solid var(--volute-electric-teal);
        }

        button.secondary:hover {
            background: var(--volute-electric-teal)15;
        }

        .section {
            margin-bottom: 24px;
        }

        .section-title {
            font-size: 11px;
            font-weight: 600;
            text-transform: uppercase;
            letter-spacing: 0.5px;
            color: var(--vscode-descriptionForeground, var(--volute-text-muted));
            margin-bottom: 8px;
            display: flex;
            align-items: center;
            gap: 8px;
        }

        .section-content {
            background: var(--section-bg);
            border-radius: 4px;
            padding: 16px;
            border: 1px solid var(--border-color);
        }

        /* Volute-branded section accents */
        .section-content.prompt {
            border-left: 3px solid var(--volute-electric-teal);
        }

        .section-content.response {
            border-left: 3px solid var(--volute-git-orange);
        }

        .section-content.cot {
            border-left: 3px solid var(--volute-text-muted);
            font-size: 13px;
            opacity: 0.9;
        }

        /* Markdown styles */
        .section-content p {
            margin: 0 0 12px 0;
        }

        .section-content p:last-child {
            margin-bottom: 0;
        }

        .section-content pre {
            background: var(--code-bg);
            padding: 12px;
            border-radius: 4px;
            overflow-x: auto;
            margin: 12px 0;
            border: 1px solid var(--border-color);
        }

        .section-content code {
            font-family: var(--vscode-editor-font-family, 'Fira Code', 'JetBrains Mono', monospace);
            font-size: 13px;
        }

        .section-content :not(pre) > code {
            background: var(--code-bg);
            padding: 2px 6px;
            border-radius: 3px;
        }

        .section-content ul, .section-content ol {
            margin: 12px 0;
            padding-left: 24px;
        }

        .section-content blockquote {
            border-left: 3px solid var(--volute-electric-teal)60;
            margin: 12px 0;
            padding-left: 12px;
            color: var(--vscode-descriptionForeground, var(--volute-text-muted));
        }

        .section-content a {
            color: var(--volute-electric-teal);
            text-decoration: none;
        }

        .section-content a:hover {
            text-decoration: underline;
        }

        /* Context files */
        .context-files {
            display: flex;
            flex-wrap: wrap;
            gap: 8px;
        }

        .context-file {
            display: inline-flex;
            align-items: center;
            gap: 4px;
            background: var(--volute-void-slate);
            color: var(--volute-electric-teal);
            padding: 4px 10px;
            border-radius: 4px;
            font-size: 12px;
            cursor: pointer;
            border: 1px solid var(--volute-electric-teal)40;
            transition: all 0.2s;
        }

        .context-file:hover {
            background: var(--volute-electric-teal)15;
            border-color: var(--volute-electric-teal);
        }

        /* Tool executions */
        .tool-execution {
            display: flex;
            align-items: center;
            gap: 8px;
            padding: 8px 12px;
            background: var(--section-bg);
            border-radius: 4px;
            margin-bottom: 8px;
            font-size: 13px;
            border: 1px solid var(--border-color);
        }

        .tool-execution:last-child {
            margin-bottom: 0;
        }

        .tool-status {
            width: 8px;
            height: 8px;
            border-radius: 50%;
        }

        .tool-status.success {
            background: var(--volute-electric-teal);
        }

        .tool-status.failure {
            background: var(--volute-git-orange);
        }

        .tool-name {
            font-weight: 500;
            font-family: var(--vscode-editor-font-family, 'Fira Code', monospace);
        }

        .tool-protocol {
            color: var(--vscode-descriptionForeground, var(--volute-text-muted));
            font-size: 11px;
        }

        .linked-commit {
            display: inline-flex;
            align-items: center;
            gap: 6px;
            background: var(--volute-git-orange)20;
            color: var(--volute-git-orange);
            padding: 4px 10px;
            border-radius: 4px;
            font-family: var(--vscode-editor-font-family, 'Fira Code', monospace);
            font-size: 12px;
            border: 1px solid var(--volute-git-orange)40;
        }

        .collapsible {
            cursor: pointer;
            user-select: none;
        }

        .collapsible::before {
            content: '\\25BC';
            font-size: 10px;
            transition: transform 0.2s;
            display: inline-block;
            margin-right: 4px;
        }

        .collapsible.collapsed::before {
            transform: rotate(-90deg);
        }

        .collapsible-content {
            overflow: hidden;
            transition: max-height 0.3s ease-out;
        }

        .collapsible-content.collapsed {
            max-height: 0 !important;
            padding: 0;
            margin: 0;
            border: none;
        }

        /* Volute branding footer */
        .footer {
            margin-top: 32px;
            padding-top: 16px;
            border-top: 1px solid var(--border-color);
            text-align: center;
            font-size: 11px;
            color: var(--volute-text-muted);
            letter-spacing: 0.5px;
        }

        .footer .brand {
            color: var(--volute-electric-teal);
            font-weight: 600;
        }
    </style>
</head>
<body>
    <div class="container">
        <div class="header">
            <div class="meta">
                <div class="meta-item">
                    <span>${authorIcon}</span>
                    <span>${authorLabel}</span>
                </div>
                <div class="meta-item">
                    <span>&#128337;</span>
                    <span>${timestamp}</span>
                </div>
                ${
                  detail.modelName
                    ? `
                <div class="meta-item">
                    <span>&#129302;</span>
                    <span>${this.escapeHtml(detail.modelName)}</span>
                </div>
                `
                    : ""
                }
                ${
                  detail.linkedCommit
                    ? `
                <div class="meta-item">
                    <span class="linked-commit">
                        <span>&#128279;</span>
                        ${detail.linkedCommit.substring(0, 7)}
                    </span>
                </div>
                `
                    : ""
                }
            </div>
            <div class="actions">
                <button class="secondary" onclick="copyPrompt()">Copy Prompt</button>
                <button onclick="refresh()">Refresh</button>
            </div>
        </div>

        ${
          hasPrompt
            ? `
        <div class="section">
            <div class="section-title">
                <span>&#128172;</span> Prompt
            </div>
            <div class="section-content prompt">
                ${promptHtml}
            </div>
        </div>
        `
            : ""
        }

        ${
          detail.contextFiles && detail.contextFiles.length > 0
            ? `
        <div class="section">
            <div class="section-title">
                <span>&#128193;</span> Context Files
            </div>
            <div class="context-files">
                ${detail.contextFiles
                  .map(
                    (f) => `
                    <span class="context-file" onclick="openFile('${this.escapeHtml(f.path)}')">
                        &#128196; ${this.escapeHtml(this.getFileName(f.path))}
                        ${f.startLine !== undefined ? `<small>(${f.startLine}-${f.endLine})</small>` : ""}
                    </span>
                `,
                  )
                  .join("")}
            </div>
        </div>
        `
            : ""
        }

        ${
          hasResponse
            ? `
        <div class="section">
            <div class="section-title">
                <span>&#129302;</span> Response
            </div>
            <div class="section-content response">
                ${responseHtml}
            </div>
        </div>
        `
            : ""
        }

        ${
          hasThought
            ? `
        <div class="section">
            <div class="section-title collapsible" onclick="toggleCollapse(this)">
                <span>&#129504;</span> Chain of Thought
            </div>
            <div class="section-content cot collapsible-content">
                ${thoughtHtml}
            </div>
        </div>
        `
            : ""
        }

        ${
          detail.toolExecutions && detail.toolExecutions.length > 0
            ? `
        <div class="section">
            <div class="section-title collapsible" onclick="toggleCollapse(this)">
                <span>&#128295;</span> Tool Executions (${detail.toolExecutions.length})
            </div>
            <div class="collapsible-content">
                ${detail.toolExecutions
                  .map(
                    (t) => `
                    <div class="tool-execution">
                        <span class="tool-status ${t.status}"></span>
                        <span class="tool-name">${this.escapeHtml(t.name)}</span>
                        <span class="tool-protocol">${this.escapeHtml(t.protocol)}</span>
                    </div>
                `,
                  )
                  .join("")}
            </div>
        </div>
        `
            : ""
        }

        <div class="footer">
            Tracked by <span class="brand">Volute CVC</span>
        </div>
    </div>

    <script>
        const vscode = acquireVsCodeApi();
        const prompt = ${JSON.stringify(detail.userPrompt)};

        function refresh() {
            vscode.postMessage({ command: 'refresh' });
        }

        function copyPrompt() {
            vscode.postMessage({ command: 'copyPrompt', text: prompt });
        }

        function openFile(path) {
            vscode.postMessage({ command: 'openFile', path: path });
        }

        function toggleCollapse(element) {
            element.classList.toggle('collapsed');
            const content = element.nextElementSibling;
            if (content) {
                content.classList.toggle('collapsed');
            }
        }
    </script>
</body>
</html>`;
  }

  /**
   * Render markdown to HTML with sanitization
   */
  private renderMarkdown(text: string): string {
    try {
      // Configure marked for safe rendering
      marked.setOptions({
        gfm: true,
        breaks: true,
      });

      const html = marked.parse(text);
      // Basic sanitization - remove script tags and event handlers
      return this.sanitizeHtml(typeof html === "string" ? html : "");
    } catch (error) {
      this.outputChannel.appendLine("Markdown render error");
      return this.escapeHtml(text);
    }
  }

  /**
   * Basic HTML sanitization
   */
  private sanitizeHtml(html: string): string {
    return (
      html
        // Remove script tags
        .replace(/<script\b[^<]*(?:(?!<\/script>)<[^<]*)*<\/script>/gi, "")
        // Remove event handlers
        .replace(/\son\w+\s*=/gi, " data-removed=")
        // Remove javascript: URLs
        .replace(/javascript:/gi, "removed:")
    );
  }

  /**
   * Escape HTML special characters
   */
  private escapeHtml(text: string): string {
    return text
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;")
      .replace(/'/g, "&#039;");
  }

  /**
   * Get filename from path
   */
  private getFileName(filePath: string): string {
    return filePath.split(/[/\\]/).pop() || filePath;
  }

  /**
   * Truncate title for panel
   */
  private truncateTitle(text: string): string {
    const cleaned = text.replace(/\s+/g, " ").trim();
    if (cleaned.length > 40) {
      return cleaned.substring(0, 37) + "...";
    }
    return cleaned;
  }

  /**
   * Dispose of the panel
   */
  public dispose(): void {
    ThoughtDetailPanel.currentPanel = undefined;

    this.panel.dispose();

    while (this.disposables.length) {
      const disposable = this.disposables.pop();
      if (disposable) {
        disposable.dispose();
      }
    }
  }
}

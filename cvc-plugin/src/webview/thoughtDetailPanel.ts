import * as vscode from 'vscode';
import { CvcLanguageClient } from '../lsp/client';
import { InteractionDetail } from '../lsp/protocol';
import { marked } from 'marked';

/**
 * Manages Thought Detail webview panels
 */
export class ThoughtDetailPanel {
    public static currentPanel: ThoughtDetailPanel | undefined;
    public static readonly viewType = 'cvc.thoughtDetail';

    private readonly panel: vscode.WebviewPanel;
    private readonly extensionUri: vscode.Uri;
    private readonly outputChannel: vscode.OutputChannel;
    private readonly lspClient: CvcLanguageClient;
    private currentInteractionId: string | undefined;
    private disposables: vscode.Disposable[] = [];

    private constructor(
        panel: vscode.WebviewPanel,
        extensionUri: vscode.Uri,
        outputChannel: vscode.OutputChannel,
        lspClient: CvcLanguageClient
    ) {
        this.panel = panel;
        this.extensionUri = extensionUri;
        this.outputChannel = outputChannel;
        this.lspClient = lspClient;

        // Set up panel event handlers
        this.panel.onDidDispose(() => this.dispose(), null, this.disposables);

        // Handle messages from the webview
        this.panel.webview.onDidReceiveMessage(
            message => this.handleMessage(message),
            null,
            this.disposables
        );
    }

    /**
     * Create or show the thought detail panel
     */
    public static createOrShow(
        extensionUri: vscode.Uri,
        outputChannel: vscode.OutputChannel,
        lspClient: CvcLanguageClient,
        interactionId: string
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
            'Thought Detail',
            column,
            {
                enableScripts: true,
                retainContextWhenHidden: true,
                localResourceRoots: [
                    vscode.Uri.joinPath(extensionUri, 'node_modules', '@vscode/webview-ui-toolkit'),
                    vscode.Uri.joinPath(extensionUri, 'dist'),
                ],
            }
        );

        ThoughtDetailPanel.currentPanel = new ThoughtDetailPanel(
            panel,
            extensionUri,
            outputChannel,
            lspClient
        );

        ThoughtDetailPanel.currentPanel.loadInteraction(interactionId);
        return ThoughtDetailPanel.currentPanel;
    }

    /**
     * Load and display an interaction
     */
    public async loadInteraction(interactionId: string): Promise<void> {
        this.currentInteractionId = interactionId;
        this.panel.title = 'Loading...';

        // Show loading state
        this.panel.webview.html = this.getLoadingHtml();

        // Fetch interaction details
        const detail = await this.lspClient.sendInteractionGet({ id: interactionId });

        if (detail) {
            this.panel.title = this.truncateTitle(detail.userPrompt);
            this.panel.webview.html = this.getDetailHtml(detail);
        } else {
            this.panel.title = 'Error';
            this.panel.webview.html = this.getErrorHtml('Failed to load interaction details');
        }
    }

    /**
     * Handle messages from the webview
     */
    private handleMessage(message: { command: string; [key: string]: unknown }): void {
        switch (message.command) {
            case 'refresh':
                if (this.currentInteractionId) {
                    this.loadInteraction(this.currentInteractionId);
                }
                break;
            case 'copyPrompt':
                if (message.text) {
                    vscode.env.clipboard.writeText(message.text as string);
                    vscode.window.showInformationMessage('Prompt copied to clipboard');
                }
                break;
            case 'openFile':
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
            font-family: var(--vscode-font-family);
            padding: 20px;
            color: var(--vscode-foreground);
            background-color: var(--vscode-editor-background);
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
            border: 3px solid var(--vscode-button-background);
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
            font-family: var(--vscode-font-family);
            padding: 20px;
            color: var(--vscode-foreground);
            background-color: var(--vscode-editor-background);
        }
        .error {
            text-align: center;
            padding: 40px;
        }
        .error-icon {
            font-size: 48px;
            margin-bottom: 16px;
        }
        .error-message {
            color: var(--vscode-errorForeground);
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
     * Generate the main detail HTML
     */
    private getDetailHtml(detail: InteractionDetail): string {
        const promptHtml = this.renderMarkdown(detail.userPrompt);
        const responseHtml = detail.modelResponse
            ? this.renderMarkdown(detail.modelResponse)
            : '<em>No response recorded</em>';
        const cotHtml = detail.modelCot
            ? this.renderMarkdown(detail.modelCot)
            : null;

        const timestamp = new Date(detail.timestamp).toLocaleString();
        const authorIcon = detail.author === 'human' ? '&#128100;' : '&#129302;';
        const authorLabel = detail.author.charAt(0).toUpperCase() + detail.author.slice(1);

        return `<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline';">
    <title>Thought Detail</title>
    <style>
        :root {
            --section-bg: var(--vscode-editor-inactiveSelectionBackground);
            --border-color: var(--vscode-panel-border);
            --code-bg: var(--vscode-textCodeBlock-background);
        }

        body {
            font-family: var(--vscode-font-family);
            font-size: var(--vscode-font-size);
            padding: 0;
            margin: 0;
            color: var(--vscode-foreground);
            background-color: var(--vscode-editor-background);
            line-height: 1.5;
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
            color: var(--vscode-descriptionForeground);
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
            background: var(--vscode-button-background);
            color: var(--vscode-button-foreground);
            border: none;
            padding: 6px 12px;
            border-radius: 2px;
            cursor: pointer;
            font-size: 12px;
        }

        button:hover {
            background: var(--vscode-button-hoverBackground);
        }

        button.secondary {
            background: var(--vscode-button-secondaryBackground);
            color: var(--vscode-button-secondaryForeground);
        }

        button.secondary:hover {
            background: var(--vscode-button-secondaryHoverBackground);
        }

        .section {
            margin-bottom: 24px;
        }

        .section-title {
            font-size: 11px;
            font-weight: 600;
            text-transform: uppercase;
            letter-spacing: 0.5px;
            color: var(--vscode-descriptionForeground);
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

        .section-content.prompt {
            border-left: 3px solid var(--vscode-charts-blue);
        }

        .section-content.response {
            border-left: 3px solid var(--vscode-charts-green);
        }

        .section-content.cot {
            border-left: 3px solid var(--vscode-charts-yellow);
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
        }

        .section-content code {
            font-family: var(--vscode-editor-font-family);
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
            border-left: 3px solid var(--border-color);
            margin: 12px 0;
            padding-left: 12px;
            color: var(--vscode-descriptionForeground);
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
            background: var(--vscode-badge-background);
            color: var(--vscode-badge-foreground);
            padding: 4px 8px;
            border-radius: 4px;
            font-size: 12px;
            cursor: pointer;
        }

        .context-file:hover {
            opacity: 0.8;
        }

        /* Tool executions */
        .tool-execution {
            display: flex;
            align-items: center;
            gap: 8px;
            padding: 8px;
            background: var(--section-bg);
            border-radius: 4px;
            margin-bottom: 8px;
            font-size: 13px;
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
            background: var(--vscode-charts-green);
        }

        .tool-status.failure {
            background: var(--vscode-charts-red);
        }

        .tool-name {
            font-weight: 500;
        }

        .tool-protocol {
            color: var(--vscode-descriptionForeground);
            font-size: 11px;
        }

        .linked-commit {
            display: inline-flex;
            align-items: center;
            gap: 6px;
            background: var(--vscode-badge-background);
            color: var(--vscode-badge-foreground);
            padding: 4px 10px;
            border-radius: 4px;
            font-family: var(--vscode-editor-font-family);
            font-size: 12px;
        }

        .collapsible {
            cursor: pointer;
        }

        .collapsible::before {
            content: '\\25BC';
            font-size: 10px;
            transition: transform 0.2s;
        }

        .collapsible.collapsed::before {
            transform: rotate(-90deg);
        }

        .collapsible-content {
            overflow: hidden;
            transition: max-height 0.2s;
        }

        .collapsible-content.collapsed {
            max-height: 0;
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
                ${detail.modelName ? `
                <div class="meta-item">
                    <span>&#129302;</span>
                    <span>${this.escapeHtml(detail.modelName)}</span>
                </div>
                ` : ''}
                ${detail.linkedCommit ? `
                <div class="meta-item">
                    <span class="linked-commit">
                        <span>&#128279;</span>
                        ${detail.linkedCommit.substring(0, 7)}
                    </span>
                </div>
                ` : ''}
            </div>
            <div class="actions">
                <button class="secondary" onclick="copyPrompt()">Copy Prompt</button>
                <button onclick="refresh()">Refresh</button>
            </div>
        </div>

        <div class="section">
            <div class="section-title">
                <span>&#128172;</span> Prompt
            </div>
            <div class="section-content prompt">
                ${promptHtml}
            </div>
        </div>

        ${detail.contextFiles && detail.contextFiles.length > 0 ? `
        <div class="section">
            <div class="section-title">
                <span>&#128193;</span> Context Files
            </div>
            <div class="context-files">
                ${detail.contextFiles.map(f => `
                    <span class="context-file" onclick="openFile('${this.escapeHtml(f.path)}')">
                        &#128196; ${this.escapeHtml(this.getFileName(f.path))}
                        ${f.startLine !== undefined ? `<small>(${f.startLine}-${f.endLine})</small>` : ''}
                    </span>
                `).join('')}
            </div>
        </div>
        ` : ''}

        <div class="section">
            <div class="section-title">
                <span>&#129302;</span> Response
            </div>
            <div class="section-content response">
                ${responseHtml}
            </div>
        </div>

        ${cotHtml ? `
        <div class="section">
            <div class="section-title collapsible" onclick="toggleCollapse(this)">
                <span>&#129504;</span> Chain of Thought
            </div>
            <div class="section-content cot collapsible-content">
                ${cotHtml}
            </div>
        </div>
        ` : ''}

        ${detail.toolExecutions && detail.toolExecutions.length > 0 ? `
        <div class="section">
            <div class="section-title collapsible" onclick="toggleCollapse(this)">
                <span>&#128295;</span> Tool Executions (${detail.toolExecutions.length})
            </div>
            <div class="collapsible-content">
                ${detail.toolExecutions.map(t => `
                    <div class="tool-execution">
                        <span class="tool-status ${t.status}"></span>
                        <span class="tool-name">${this.escapeHtml(t.name)}</span>
                        <span class="tool-protocol">${this.escapeHtml(t.protocol)}</span>
                    </div>
                `).join('')}
            </div>
        </div>
        ` : ''}
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
            return this.sanitizeHtml(typeof html === 'string' ? html : '');
        } catch (error) {
            this.outputChannel.appendLine(`Markdown render error: ${error}`);
            return this.escapeHtml(text);
        }
    }

    /**
     * Basic HTML sanitization
     */
    private sanitizeHtml(html: string): string {
        return html
            // Remove script tags
            .replace(/<script\b[^<]*(?:(?!<\/script>)<[^<]*)*<\/script>/gi, '')
            // Remove event handlers
            .replace(/\son\w+\s*=/gi, ' data-removed=')
            // Remove javascript: URLs
            .replace(/javascript:/gi, 'removed:');
    }

    /**
     * Escape HTML special characters
     */
    private escapeHtml(text: string): string {
        return text
            .replace(/&/g, '&amp;')
            .replace(/</g, '&lt;')
            .replace(/>/g, '&gt;')
            .replace(/"/g, '&quot;')
            .replace(/'/g, '&#039;');
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
        const cleaned = text.replace(/\s+/g, ' ').trim();
        if (cleaned.length > 40) {
            return cleaned.substring(0, 37) + '...';
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

import * as vscode from "vscode";
import * as path from "path";
import * as fs from "fs";
import { VoluteLanguageClient } from "../lsp/client";

/**
 * Structure of a chat session file (partial - only fields we care about)
 */
interface ChatSessionFile {
  version: number;
  sessionId: string;
  requests: ChatRequest[];
  inputState?: {
    selectedModel?: {
      identifier?: string;
      metadata?: {
        name?: string;
        vendor?: string;
      };
    };
  };
}

interface ChatRequest {
  requestId: string;
  message: {
    text: string;
    parts?: Array<{
      text?: string;
      kind?: string;
    }>;
  };
  variableData?: {
    variables?: Array<{
      kind: string;
      value: vscode.Uri | string;
      name?: string;
    }>;
  };
  response?: ChatResponsePart[];
}

interface ChatResponsePart {
  kind: string;
  value?: string;
  id?: string;
  generatedTitle?: string;
  metadata?: {
    vscodeReasoningDone?: boolean;
    stopReason?: string;
  };
  supportThemeIcons?: boolean;
  baseUri?: vscode.Uri;
  uris?: Record<string, unknown>;
}

/**
 * ChatSessionWatcher - The Invisible Stenographer
 *
 * Passively monitors VS Code's Copilot Chat session files to capture
 * conversations without interfering with Copilot's functionality.
 */
export class ChatSessionWatcher {
  private readonly outputChannel: vscode.OutputChannel;
  private readonly lspClient: VoluteLanguageClient;
  private watcher: vscode.FileSystemWatcher | undefined;
  private chatSessionsDir: string | undefined;

  // Track processed requests to avoid duplicates
  private processedRequests: Set<string> = new Set();

  // Debounce timers for file changes (per-file)
  private debounceTimers: Map<string, NodeJS.Timeout> = new Map();
  private readonly debounceMs = 500;

  // Polling interval for backup detection
  private pollingInterval: NodeJS.Timeout | undefined;
  private readonly pollingMs = 3000;

  constructor(
    outputChannel: vscode.OutputChannel,
    lspClient: VoluteLanguageClient,
  ) {
    this.outputChannel = outputChannel;
    this.lspClient = lspClient;
  }

  /**
   * Start watching for chat session changes
   */
  async start(context: vscode.ExtensionContext): Promise<void> {
    this.outputChannel.appendLine("ChatSessionWatcher: Starting...");

    // Find the chat sessions directory
    this.chatSessionsDir = await this.findChatSessionsDir(context);

    if (!this.chatSessionsDir) {
      this.outputChannel.appendLine(
        "ChatSessionWatcher: Could not find chatSessions directory. Copilot Chat may not be installed.",
      );
      return;
    }

    this.outputChannel.appendLine(
      `ChatSessionWatcher: Watching ${this.chatSessionsDir}`,
    );

    // Create file system watcher for JSON files in the chat sessions directory
    const pattern = new vscode.RelativePattern(this.chatSessionsDir, "*.json");
    this.watcher = vscode.workspace.createFileSystemWatcher(pattern);

    // Watch for changes (which includes new content being written)
    this.watcher.onDidChange((uri) => this.onSessionFileChanged(uri));
    this.watcher.onDidCreate((uri) => this.onSessionFileChanged(uri));

    context.subscriptions.push(this.watcher);

    // Do an initial scan of existing sessions
    await this.scanExistingSessions();

    // Start polling as a backup mechanism (file watchers can be unreliable)
    this.startPolling();

    this.outputChannel.appendLine("ChatSessionWatcher: Started successfully");
  }

  /**
   * Stop watching
   */
  stop(): void {
    // Clear all debounce timers
    for (const timer of this.debounceTimers.values()) {
      clearTimeout(timer);
    }
    this.debounceTimers.clear();

    // Stop polling
    if (this.pollingInterval) {
      clearInterval(this.pollingInterval);
      this.pollingInterval = undefined;
    }

    this.watcher?.dispose();
    this.watcher = undefined;
    this.outputChannel.appendLine("ChatSessionWatcher: Stopped");
  }

  /**
   * Start polling for changes as a backup mechanism
   */
  private startPolling(): void {
    this.pollingInterval = setInterval(() => {
      this.pollForChanges();
    }, this.pollingMs);
  }

  /**
   * Poll all session files for new requests
   */
  private async pollForChanges(): Promise<void> {
    if (!this.chatSessionsDir) {
      return;
    }

    try {
      const files = await fs.promises.readdir(this.chatSessionsDir);
      const jsonFiles = files.filter((f) => f.endsWith(".json"));

      let foundNew = false;
      for (const file of jsonFiles) {
        const filePath = path.join(this.chatSessionsDir, file);
        const beforeCount = this.processedRequests.size;
        await this.processSessionFile(filePath);
        if (this.processedRequests.size > beforeCount) {
          foundNew = true;
        }
      }

      if (foundNew) {
        this.outputChannel.appendLine(
          `ChatSessionWatcher: Polling found new requests (total indexed: ${this.processedRequests.size})`,
        );
      }
    } catch {
      // Ignore polling errors
    }
  }

  /**
   * Find the VS Code workspace storage directory containing chatSessions
   */
  private async findChatSessionsDir(
    context: vscode.ExtensionContext,
  ): Promise<string | undefined> {
    // The workspace storage path gives us a hint about where VS Code stores data
    const globalStoragePath = context.globalStorageUri.fsPath;

    // Navigate up to find the User directory
    // globalStoragePath is typically: ~/.config/Code/User/globalStorage/publisher.extension
    // We need: ~/.config/Code/User/workspaceStorage/<workspace-id>/chatSessions

    const userDir = path.resolve(globalStoragePath, "..", "..");
    const workspaceStorageDir = path.join(userDir, "workspaceStorage");

    if (!fs.existsSync(workspaceStorageDir)) {
      this.outputChannel.appendLine(
        `ChatSessionWatcher: workspaceStorage not found at ${workspaceStorageDir}`,
      );
      return undefined;
    }

    // Find the workspace storage for the current workspace
    const workspaceFolders = vscode.workspace.workspaceFolders;
    if (!workspaceFolders || workspaceFolders.length === 0) {
      this.outputChannel.appendLine(
        "ChatSessionWatcher: No workspace folders open",
      );
      return undefined;
    }

    const workspaceUri = workspaceFolders[0].uri.toString();

    // VS Code uses a hash of the workspace URI for the storage folder name
    // We'll try to find it by checking each folder for a workspace.json that matches
    const storageDirs = await fs.promises.readdir(workspaceStorageDir);

    for (const dir of storageDirs) {
      const workspaceJsonPath = path.join(
        workspaceStorageDir,
        dir,
        "workspace.json",
      );

      try {
        const workspaceJson = await fs.promises.readFile(
          workspaceJsonPath,
          "utf-8",
        );
        const workspaceData = JSON.parse(workspaceJson);

        // Check if this is our workspace
        if (
          workspaceData.folder === workspaceUri ||
          workspaceData.workspace === workspaceUri
        ) {
          const chatSessionsPath = path.join(
            workspaceStorageDir,
            dir,
            "chatSessions",
          );

          if (fs.existsSync(chatSessionsPath)) {
            return chatSessionsPath;
          }
        }
      } catch {
        // Ignore errors reading individual workspace.json files
      }
    }

    // Fallback: try to find any chatSessions directory with recent activity
    this.outputChannel.appendLine(
      "ChatSessionWatcher: Falling back to most recent chatSessions directory",
    );

    let mostRecentDir: string | undefined;
    let mostRecentTime = 0;

    for (const dir of storageDirs) {
      const chatSessionsPath = path.join(
        workspaceStorageDir,
        dir,
        "chatSessions",
      );

      try {
        const stats = await fs.promises.stat(chatSessionsPath);
        if (stats.isDirectory() && stats.mtimeMs > mostRecentTime) {
          mostRecentTime = stats.mtimeMs;
          mostRecentDir = chatSessionsPath;
        }
      } catch {
        // Directory doesn't exist or can't be accessed
      }
    }

    return mostRecentDir;
  }

  /**
   * Handle a chat session file change
   */
  private onSessionFileChanged(uri: vscode.Uri): void {
    const filePath = uri.fsPath;

    this.outputChannel.appendLine(
      `ChatSessionWatcher: File change detected: ${path.basename(filePath)}`,
    );

    // Debounce per-file to avoid processing incomplete writes
    const existingTimer = this.debounceTimers.get(filePath);
    if (existingTimer) {
      clearTimeout(existingTimer);
    }

    const timer = setTimeout(() => {
      this.debounceTimers.delete(filePath);
      this.processSessionFile(filePath);
    }, this.debounceMs);

    this.debounceTimers.set(filePath, timer);
  }

  /**
   * Scan existing session files on startup
   */
  private async scanExistingSessions(): Promise<void> {
    if (!this.chatSessionsDir) {
      return;
    }

    try {
      const files = await fs.promises.readdir(this.chatSessionsDir);
      const jsonFiles = files.filter((f) => f.endsWith(".json"));

      this.outputChannel.appendLine(
        `ChatSessionWatcher: Found ${jsonFiles.length} existing session files`,
      );

      // Process each file to build the set of already-processed requests
      // We don't send these to LSP on startup to avoid duplicates
      for (const file of jsonFiles) {
        const filePath = path.join(this.chatSessionsDir, file);
        await this.indexSessionFile(filePath);
      }

      this.outputChannel.appendLine(
        `ChatSessionWatcher: Indexed ${this.processedRequests.size} existing requests`,
      );
    } catch (error) {
      this.outputChannel.appendLine(
        `ChatSessionWatcher: Error scanning sessions: ${error}`,
      );
    }
  }

  /**
   * Index a session file without sending to LSP (for startup)
   */
  private async indexSessionFile(filePath: string): Promise<void> {
    try {
      const content = await fs.promises.readFile(filePath, "utf-8");
      const session: ChatSessionFile = JSON.parse(content);

      for (const request of session.requests || []) {
        if (request.requestId) {
          this.processedRequests.add(request.requestId);
        }
      }
    } catch {
      // Ignore parse errors
    }
  }

  /**
   * Process a session file and extract new interactions
   */
  private async processSessionFile(filePath: string): Promise<void> {
    try {
      const content = await fs.promises.readFile(filePath, "utf-8");
      const session: ChatSessionFile = JSON.parse(content);

      const modelName = this.extractModelName(session);

      for (const request of session.requests || []) {
        // Skip if already processed
        if (this.processedRequests.has(request.requestId)) {
          continue;
        }

        // Check if the response is complete
        if (!this.isResponseComplete(request)) {
          continue;
        }

        this.outputChannel.appendLine(
          `ChatSessionWatcher: Processing new request ${request.requestId}`,
        );

        // Mark as processed
        this.processedRequests.add(request.requestId);

        // Extract the interaction data
        await this.sendInteractionToLsp(request, session.sessionId, modelName);
      }
    } catch (error) {
      this.outputChannel.appendLine(
        `ChatSessionWatcher: Error processing ${filePath}: ${error}`,
      );
    }
  }

  /**
   * Check if a response appears to be complete
   */
  private isResponseComplete(request: ChatRequest): boolean {
    if (!request.response || request.response.length === 0) {
      return false;
    }

    // Look for completion markers
    const lastParts = request.response.slice(-3);
    for (const part of lastParts) {
      // Check for reasoning done marker
      if (part.metadata?.vscodeReasoningDone) {
        return true;
      }
      // Check for stop reason
      if (part.metadata?.stopReason) {
        return true;
      }
      // If we have actual markdown content, it's likely complete
      if (
        part.kind === "markdownContent" ||
        (part.value && part.value.length > 50)
      ) {
        return true;
      }
    }

    // If there's substantial response content, consider it complete
    const totalLength = request.response.reduce((sum, part) => {
      return sum + (part.value?.length || 0);
    }, 0);

    return totalLength > 100;
  }

  /**
   * Extract model name from session
   */
  private extractModelName(session: ChatSessionFile): string | undefined {
    const model = session.inputState?.selectedModel;
    if (model?.metadata) {
      return `${model.metadata.vendor || "unknown"}/${model.metadata.name || "unknown"}`;
    }
    return model?.identifier;
  }

  /**
   * Send an interaction to the LSP server
   */
  private async sendInteractionToLsp(
    request: ChatRequest,
    sessionId: string,
    modelName: string | undefined,
  ): Promise<void> {
    // Extract context files
    const contextFiles: string[] = [];
    if (request.variableData?.variables) {
      for (const variable of request.variableData.variables) {
        if (variable.kind === "file" && variable.name) {
          contextFiles.push(variable.name);
        }
      }
    }

    // Extract the prompt
    const prompt = request.message.text || "";

    // Extract chain of thought and response
    const { chainOfThought, response } = this.extractResponse(request);

    // Generate a unique turn ID
    const turnId = request.requestId;

    // Send turn start
    await this.lspClient.sendTurnStart({
      id: turnId,
      prompt,
      author: "human",
      contextFiles,
    });

    // Send turn end with response
    await this.lspClient.sendTurnEnd({
      id: turnId,
      response,
      chainOfThought,
      model: modelName,
    });

    this.outputChannel.appendLine(
      `ChatSessionWatcher: Logged interaction ${turnId} (${prompt.substring(0, 50)}...)`,
    );
  }

  /**
   * Extract chain of thought and response from request
   */
  private extractResponse(request: ChatRequest): {
    chainOfThought: string | undefined;
    response: string;
  } {
    const thinkingParts: string[] = [];
    const responseParts: string[] = [];

    for (const part of request.response || []) {
      if (part.kind === "thinking" && part.value) {
        thinkingParts.push(part.value);
      } else if (part.value && part.kind !== "progressTaskSerialized") {
        // Skip progress messages, include actual content
        responseParts.push(part.value);
      }
    }

    return {
      chainOfThought:
        thinkingParts.length > 0 ? thinkingParts.join("\n\n") : undefined,
      response: responseParts.join(""),
    };
  }
}

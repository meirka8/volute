import * as crypto from "crypto";
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

  // Track checksums of processed requests to detect actual changes
  private requestChecksums: Map<string, string> = new Map();

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
    const pattern = new vscode.RelativePattern(this.chatSessionsDir, "*.json*");
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
      const jsonFiles = files.filter((f) => f.endsWith(".json") || f.endsWith(".jsonl"));

      for (const file of jsonFiles) {
        const filePath = path.join(this.chatSessionsDir, file);
        await this.processSessionFile(filePath);
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
      const jsonFiles = files.filter((f) => f.endsWith(".json") || f.endsWith(".jsonl"));

      this.outputChannel.appendLine(
        `ChatSessionWatcher: Found ${jsonFiles.length} existing session files`,
      );

      // Process each file to build the set of known checksums
      // We don't send these to LSP on startup to avoid duplicates
      for (const file of jsonFiles) {
        const filePath = path.join(this.chatSessionsDir, file);
        await this.indexSessionFile(filePath);
      }

      this.outputChannel.appendLine(
        `ChatSessionWatcher: Indexed ${this.requestChecksums.size} existing requests`,
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
      const session = this.parseSessionFile(content, filePath);

      if (!session) {
        return;
      }

      for (const request of session.requests || []) {
        if (request.requestId) {
          const checksum = this.calculateChecksum(request);
          this.requestChecksums.set(request.requestId, checksum);
        }
      }
    } catch {
      // Ignore parse errors
    }
  }

  /**
   * Process a session file and extract new/updated interactions
   */
  private async processSessionFile(filePath: string): Promise<void> {
    try {
      const content = await fs.promises.readFile(filePath, "utf-8");
      const session = this.parseSessionFile(content, filePath);

      if (!session) {
        return;
      }

      const modelName = this.extractModelName(session);

      for (const request of session.requests || []) {
        // Calculate current checksum of content
        const currentChecksum = this.calculateChecksum(request);
        const lastChecksum = this.requestChecksums.get(request.requestId);

        // If content hasn't changed, skip
        if (currentChecksum === lastChecksum) {
          continue;
        }

        this.outputChannel.appendLine(
          `ChatSessionWatcher: Processing request update ${request.requestId} (checksum changed)`,
        );

        // Update tracking
        this.requestChecksums.set(request.requestId, currentChecksum);

        // Extract and send interaction data
        await this.sendInteractionToLsp(request, session.sessionId, modelName);
      }
    } catch (error) {
      this.outputChannel.appendLine(
        `ChatSessionWatcher: Error processing ${filePath}: ${error}`,
      );
    }
  }

  /**
   * Parse a session file, handling both JSON and JSONL formats
   */
  private parseSessionFile(content: string, filePath: string): ChatSessionFile | undefined {
    if (filePath.endsWith(".json")) {
      return JSON.parse(content);
    } else if (filePath.endsWith(".jsonl")) {
      return this.reconstructJsonlSession(content);
    }
    return undefined;
  }

  /**
   * Reconstruct a ChatSessionFile from a .jsonl incremental log
   */
  private reconstructJsonlSession(content: string): ChatSessionFile | undefined {
    const lines = content.split('\n').filter(line => line.trim().length > 0);
    if (lines.length === 0) {
      return undefined;
    }

    // Line 0 is the base state (kind: 0)
    let session: ChatSessionFile | undefined;
    try {
      const firstLine = JSON.parse(lines[0]);
      if (firstLine.kind === 0 && firstLine.v) {
        session = firstLine.v;
      }
    } catch (e) {
      return undefined;
    }

    if (!session) {
      return undefined;
    }

    // Apply updates from subsequent lines
    for (const line of lines.slice(1)) {
      try {
        const update = JSON.parse(line);
        // We only care about updates (kind: 2)
        if (update.kind === 2 && update.k && update.v !== undefined) {
          this.applyUpdate(session, update.k, update.v);
        }
      } catch (e) {
        // Ignore malformed lines
      }
    }

    return session;
  }

  /**
   * Apply a delta update to the session object
   * k is an array of keys/indices path, v is the value to set or merge
   */
  private applyUpdate(obj: any, path: (string | number)[], value: any): void {
    let current = obj;
    for (let i = 0; i < path.length - 1; i++) {
      const key = path[i];
      if (current[key] === undefined) {
        // Creating missing structure if needed - simplistic approach
        current[key] = typeof path[i + 1] === 'number' ? [] : {};
      }
      current = current[key];
    }

    const lastKey = path[path.length - 1];

    // If the target is an array and we're just setting a value, it works.
    // But sometimes we might need to merge. For now, strict replacement based on observataion.
    // Observation from example: "k":["requests",0,"response"],"v":[...]
    // This implies replacement of that field.
    if (current) {
      current[lastKey] = value;
    }
  }

  /**
   * Calculate a stable checksum for the request content
   */
  private calculateChecksum(request: ChatRequest): string {
    // We only hash relevant parts to determine if meaningful content changed
    // We skip timestamps/progress/execution metadata that changes frequently without content change
    const partsToHash = (request.response || [])
      .filter(p => ["text", "markdownContent", "toolInvocationSerialized", "thinking"].includes(p.kind))
      .map(p => {
        // Only hash stable fields that represent content
        const stablePart: any = {
          kind: p.kind,
          value: p.value,
        };

        // Include URIs if present (important for file references)
        if (p.uris) {
          stablePart.uris = p.uris;
        }

        return stablePart;
      });

    const content = JSON.stringify({
      prompt: request.message.text,
      parts: partsToHash
    });

    return crypto.createHash('md5').update(content).digest('hex');
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
      rawResponse: request.response,
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

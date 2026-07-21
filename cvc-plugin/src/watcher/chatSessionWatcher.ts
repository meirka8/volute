import * as crypto from "crypto";
import * as vscode from "vscode";
import * as path from "path";
import * as fs from "fs";
import { VoluteLanguageClient } from "../lsp/client";
import type { InteractionSegment } from "../lsp/protocol";
import { isExactWorkspaceStorage } from "../privacy";

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
 * ChatSessionWatcher - passive, consent-gated local observer.
 *
 * Passively monitors VS Code's Copilot Chat session files to capture
 * conversations without interfering with Copilot's functionality.
 */
export class ChatSessionWatcher {
  private readonly outputChannel: vscode.OutputChannel;
  private readonly lspClient: VoluteLanguageClient;
  private watcher: vscode.FileSystemWatcher | undefined;
  private chatSessionsDir: string | undefined;
  private active = false;

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
    this.active = true;
    this.outputChannel.appendLine("ChatSessionWatcher: Starting...");

    // Find the chat sessions directory
    this.chatSessionsDir = await this.findChatSessionsDir(context);

    if (!this.active) {
      return;
    }

    if (!this.chatSessionsDir) {
      this.outputChannel.appendLine(
        "ChatSessionWatcher: Could not find chatSessions directory. Copilot Chat may not be installed.",
      );
      return;
    }

    this.outputChannel.appendLine("ChatSessionWatcher: Exact workspace storage selected");

    // Create file system watcher for JSON files in the chat sessions directory
    const pattern = new vscode.RelativePattern(this.chatSessionsDir, "*.json*");
    this.watcher = vscode.workspace.createFileSystemWatcher(pattern);

    // Watch for changes (which includes new content being written)
    this.watcher.onDidChange((uri) => this.onSessionFileChanged(uri));
    this.watcher.onDidCreate((uri) => this.onSessionFileChanged(uri));

    context.subscriptions.push(this.watcher);

    // Do an initial scan of existing sessions
    await this.scanExistingSessions();

    if (!this.active) {
      return;
    }

    // Start polling as a backup mechanism (file watchers can be unreliable)
    this.startPolling();

    this.outputChannel.appendLine("ChatSessionWatcher: Started successfully");
  }

  /**
   * Stop watching
   */
  stop(): void {
    // Stop is the revocation boundary: callbacks and any in-flight async work
    // re-check this flag before reading or forwarding session content.
    this.active = false;
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
    this.chatSessionsDir = undefined;
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
    if (!this.active || !this.chatSessionsDir) {
      return;
    }

    try {
      const files = await fs.promises.readdir(this.chatSessionsDir);
      const jsonFiles = files.filter((f) => f.endsWith(".json") || f.endsWith(".jsonl"));

      for (const file of jsonFiles) {
        if (!this.active) {
          return;
        }
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
      this.outputChannel.appendLine("ChatSessionWatcher: workspaceStorage not found");
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
        if (isExactWorkspaceStorage(workspaceData, workspaceUri)) {
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

    // Never select another workspace's storage based on recency. If VS Code
    // cannot map this workspace exactly, passive observation remains disabled.
    this.outputChannel.appendLine("ChatSessionWatcher: No exact workspace storage mapping found");
    return undefined;
  }

  /**
   * Handle a chat session file change
   */
  private onSessionFileChanged(uri: vscode.Uri): void {
    if (!this.active) {
      return;
    }
    const filePath = uri.fsPath;

    this.outputChannel.appendLine("ChatSessionWatcher: Session file change detected");

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
    if (!this.active || !this.chatSessionsDir) {
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
        if (!this.active) {
          return;
        }
        const filePath = path.join(this.chatSessionsDir, file);
        await this.indexSessionFile(filePath);
      }

      this.outputChannel.appendLine(
        `ChatSessionWatcher: Indexed ${this.requestChecksums.size} existing requests`,
      );
    } catch {
      this.outputChannel.appendLine("ChatSessionWatcher: Error scanning sessions");
    }
  }

  /**
   * Index a session file without sending to LSP (for startup)
   */
  private async indexSessionFile(filePath: string): Promise<void> {
    if (!this.active) {
      return;
    }
    try {
      const content = await fs.promises.readFile(filePath, "utf-8");
      if (!this.active) {
        return;
      }
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
    if (!this.active) {
      return;
    }
    try {
      const content = await fs.promises.readFile(filePath, "utf-8");
      if (!this.active) {
        return;
      }
      const session = this.parseSessionFile(content, filePath);

      if (!session) {
        return;
      }

      const modelName = this.extractModelName(session);

      for (const request of session.requests || []) {
        if (!this.active) {
          return;
        }
        // Calculate current checksum of content
        const currentChecksum = this.calculateChecksum(request);
        const lastChecksum = this.requestChecksums.get(request.requestId);

        // If content hasn't changed, skip
        if (currentChecksum === lastChecksum) {
          continue;
        }

        this.outputChannel.appendLine("ChatSessionWatcher: Processing request update");

        // Update tracking
        this.requestChecksums.set(request.requestId, currentChecksum);

        // Extract and send interaction data
        await this.sendInteractionToLsp(request, session.sessionId, modelName);
      }
    } catch {
      this.outputChannel.appendLine("ChatSessionWatcher: Error processing session file");
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
    } catch {
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
  // VS Code's persisted JSONL patch payload is intentionally schema-flexible.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  private applyUpdate(obj: any, path: (string | number)[], value: unknown): void {
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
        const stablePart: Record<string, unknown> = {
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
   * Send segmented interactions to the LSP server via the batch notification.
   *
   * Segments a single VS Code chat request into multiple interaction records:
   * - First segment (author: human): user prompt + initial response (before first thinking block)
   * - Subsequent segments (author: agent): thinking block + response until next thinking block
   *
   * This preserves the turn structure for traceability in the cognitive timeline.
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

    const prompt = request.message.text || "";
    const segments = this.segmentResponse(request, prompt, contextFiles);

    await this.lspClient.sendTurnBatch({
      sourceRequestId: request.requestId,
      sessionId,
      model: modelName,
      interactions: segments,
    });

    this.outputChannel.appendLine(
      `ChatSessionWatcher: Logged ${segments.length} segment(s) for a chat request`,
    );
  }

  /**
   * Segment the response parts of a chat request into interaction segments.
   *
   * Algorithm: walk response parts in order. Each "thinking" block starts a new
   * agent segment. Content before the first thinking block goes into the human
   * segment (along with the prompt). Content after a thinking block goes into
   * that agent segment's response.
   */
  private segmentResponse(
    request: ChatRequest,
    prompt: string,
    contextFiles: string[],
  ): InteractionSegment[] {
    const segments: InteractionSegment[] = [];
    let currentResponseParts: string[] = [];
    let currentCot: string | undefined;

    const ignoredKinds = new Set([
      "progressTaskSerialized",
      "undoStop",
      "prepareToolInvocation",
      "mcpServersStarting",
    ]);

    for (const part of request.response || []) {
      if (part.kind === "thinking" && part.value) {
        // Flush the current segment before starting a new one
        if (segments.length === 0) {
          // First flush: this is the human turn (prompt + initial response)
          segments.push({
            author: "human",
            userPrompt: prompt,
            response: currentResponseParts.join("") || undefined,
            contextFiles: contextFiles.length > 0 ? contextFiles : undefined,
          });
        } else if (currentCot || currentResponseParts.length > 0) {
          // Subsequent flush: agent turn with previous thinking + response
          segments.push({
            author: "agent",
            chainOfThought: currentCot,
            response: currentResponseParts.join("") || undefined,
          });
        }
        currentResponseParts = [];
        currentCot = part.value;
      } else if (part.value && !ignoredKinds.has(part.kind)) {
        currentResponseParts.push(part.value);
      }
    }

    // Flush final segment
    if (segments.length === 0) {
      // No thinking blocks at all - single human turn
      segments.push({
        author: "human",
        userPrompt: prompt,
        response: currentResponseParts.join("") || undefined,
        contextFiles: contextFiles.length > 0 ? contextFiles : undefined,
      });
    } else {
      // Final agent segment
      segments.push({
        author: "agent",
        chainOfThought: currentCot,
        response: currentResponseParts.join("") || undefined,
      });
    }

    return segments;
  }
}

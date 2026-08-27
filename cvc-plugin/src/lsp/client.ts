import type * as vscode from "vscode";
import { spawn, ChildProcess } from "child_process";
import * as path from "path";
import type { LanguageClient, LanguageClientOptions, ServerOptions, Trace } from "vscode-languageclient/node";
import {
  SessionStartParams,
  TurnStartParams,
  TurnEndParams,
  TurnBatchParams,
  LinkCommitParams,
  TimelineGetParams,
  TimelineGetResponse,
  InteractionGetParams,
  InteractionDetail,
  PrivacyStatus,
} from "./protocol";
import {
  expandPath,
  isExecutable,
  findBinary,
} from "../setup/binaryUtils";
import { isPrivacyStatus } from "../privacy";

/**
 * Volute VC Language Client - manages the connection to the volute-lsp server
 *
 * Note: The LSP server binary is still named 'cvc-lsp' for backwards compatibility
 * with the existing Rust codebase. The protocol methods also retain the 'cvc' prefix.
 */
/** Small seams for deterministic lifecycle tests. Production uses the defaults below. */
export interface VoluteLanguageClientDependencies {
  findServerBinary: () => Promise<string | undefined>;
  spawn: (command: string, args: string[], options: Parameters<typeof spawn>[2]) => ChildProcess;
  createLanguageClient: (serverOptions: ServerOptions, clientOptions: LanguageClientOptions) => LanguageClient;
  getTrace: () => string;
  warnVerboseTrace: () => void;
  terminateProcess: (process: ChildProcess) => void;
}

export interface VoluteLanguageClientLifecycleState {
  hasActiveClient: boolean;
  hasStartingClient: boolean;
  hasActiveProcess: boolean;
  hasStartingProcess: boolean;
}

export class VoluteLanguageClient {
  private client: LanguageClient | undefined;
  private startingClient: LanguageClient | undefined;
  private startingProcess: ChildProcess | undefined;
  private activeProcess: ChildProcess | undefined;
  private restartPromise: Promise<void> | undefined;
  private lifecycleGeneration = 0;
  private readonly dependencies: VoluteLanguageClientDependencies;
  private readonly context: vscode.ExtensionContext;
  private readonly outputChannel: vscode.OutputChannel;
  private readonly workspaceRoot: vscode.WorkspaceFolder;

  constructor(
    context: vscode.ExtensionContext,
    outputChannel: vscode.OutputChannel,
    workspaceRoot: vscode.WorkspaceFolder,
    dependencies?: Partial<VoluteLanguageClientDependencies>,
  ) {
    this.context = context;
    this.outputChannel = outputChannel;
    this.workspaceRoot = workspaceRoot;
    this.dependencies = {
      findServerBinary: () => this.findServerBinary(),
      spawn,
      createLanguageClient: (serverOptions, clientOptions) => {
        // Keep vscode-languageclient out of node:test's module graph. This is
        // still the production LanguageClient and its ServerOptions factory.
        // eslint-disable-next-line @typescript-eslint/no-require-imports
        const languageClient = require("vscode-languageclient/node") as typeof import("vscode-languageclient/node");
        return new languageClient.LanguageClient("volute-lsp", "Volute Language Server", serverOptions, clientOptions);
      },
      getTrace: () => {
        // eslint-disable-next-line @typescript-eslint/no-require-imports
        const vscodeApi = require("vscode") as typeof import("vscode");
        return vscodeApi.workspace.getConfiguration("volute").get<string>("trace.server", "off");
      },
      warnVerboseTrace: () => {
        // eslint-disable-next-line @typescript-eslint/no-require-imports
        const vscodeApi = require("vscode") as typeof import("vscode");
        void vscodeApi.window.showWarningMessage("Volute verbose protocol tracing can expose captured chat payloads in its trace output. Disable it unless actively debugging.");
      },
      terminateProcess: (process) => { if (!process.killed) { process.kill(); } },
      ...dependencies,
    };
  }

  /**
   * Start the language client and connect to the server
   */
  async start(isActive: () => boolean = () => true): Promise<void> {
    if (!isActive()) { return; }
    const generation = this.lifecycleGeneration;
    const serverPath = await this.dependencies.findServerBinary();
    if (!isActive() || generation !== this.lifecycleGeneration) { return; }

    if (!serverPath) {
      throw new Error(
        "Could not find volute-lsp binary. Please set volute.lspPath in settings or ensure the binary is built.",
      );
    }

    this.outputChannel.appendLine("Using configured Volute language server binary");

    let spawnedProcess: ChildProcess | undefined;
    const serverOptions: ServerOptions = () => {
      const child = this.dependencies.spawn(serverPath, [], {
        cwd: this.workspaceRoot.uri.fsPath,
        shell: false,
        stdio: ["pipe", "pipe", "pipe"],
        windowsHide: true,
      });
      // Drain stderr; do not expose filesystem paths in the extension output.
      child.stderr?.on("data", () => undefined);
      spawnedProcess = child;
      if (generation !== this.lifecycleGeneration || !isActive()) {
        this.dependencies.terminateProcess(child);
      } else {
        this.startingProcess = child;
      }
      return Promise.resolve(child);
    };

    const clientOptions: LanguageClientOptions = {
      // Volute doesn't target specific file types - it's a general purpose cognitive tracker
      // We use a broad document selector but the server will handle filtering
      documentSelector: [{ scheme: "file" }],
      // vscode-languageclient v10 requires a LogOutputChannel here. Keep the
      // extension's regular OutputChannel for application messages and let the
      // client own its dedicated protocol log channel instead.
      initializationOptions: {
        workspaceFolders:
          [this.workspaceRoot.uri.fsPath],
      },
      // Explicitly bind the LSP initialize request to the same single folder
      // used by dependency discovery and `cvc init`.
      workspaceFolder: this.workspaceRoot,
    };

    // Get trace setting
    const trace = this.dependencies.getTrace();

    if (!isActive()) { return; }
    const localClient = this.dependencies.createLanguageClient(serverOptions, clientOptions);

    // Set trace level
    if (trace !== "off") {
      if (trace === "verbose") {
        this.dependencies.warnVerboseTrace();
      }
      localClient.setTrace(
        (trace === "verbose" ? "verbose" : "messages") as unknown as Trace,
      );
    }

    // Register client for disposal
    this.context.subscriptions.push(localClient);

    this.startingClient = localClient;
    try {
      if (!isActive()) { return; }
      await localClient.start();
      if (!isActive() || generation !== this.lifecycleGeneration || this.startingClient !== localClient) {
        await localClient.stop().catch(() => undefined);
        if (spawnedProcess) { this.dependencies.terminateProcess(spawnedProcess); }
        return;
      }
      this.client = localClient;
      this.activeProcess = spawnedProcess;
      if (this.startingProcess === spawnedProcess) { this.startingProcess = undefined; }
      this.startingClient = undefined;
    } catch (error) {
      await localClient.stop().catch(() => undefined);
      if (spawnedProcess) { this.dependencies.terminateProcess(spawnedProcess); }
      if (this.startingClient === localClient) { this.startingClient = undefined; }
      throw error;
    }
  }

  /**
   * Stop the language client
   */
  async stop(): Promise<void> {
    this.lifecycleGeneration += 1;
    const localClient = this.client;
    const pendingClient = this.startingClient;
    const activeProcess = this.activeProcess;
    const pendingProcess = this.startingProcess;
    this.client = undefined;
    this.startingClient = undefined;
    this.activeProcess = undefined;
    this.startingProcess = undefined;
    const clients = [...new Set([localClient, pendingClient].filter((value): value is LanguageClient => !!value))];
    // A client that is still starting may reject stop (and may never settle),
    // so its exact child must be stopped before awaiting client shutdown.
    if (pendingProcess) { this.dependencies.terminateProcess(pendingProcess); }
    await Promise.all(clients.map((value) => value.stop().catch(() => undefined)));
    if (activeProcess) { this.dependencies.terminateProcess(activeProcess); }
  }

  /**
   * Restart the language client
   */
  async restart(isActive: () => boolean = () => true): Promise<void> {
    if (this.restartPromise) { return this.restartPromise; }
    this.restartPromise = (async () => {
      if (!isActive()) { return; }
      await this.stop();
      if (!isActive()) { return; }
      await this.start(isActive);
    })();
    try { await this.restartPromise; } finally { this.restartPromise = undefined; }
  }

  /**
   * Check if the client is running
   */
  isRunning(): boolean {
    return this.client?.isRunning() ?? false;
  }

  /** Deliberately narrow state inspection used by node:test lifecycle tests. */
  getLifecycleStateForTest(): VoluteLanguageClientLifecycleState {
    return {
      hasActiveClient: this.client !== undefined,
      hasStartingClient: this.startingClient !== undefined,
      hasActiveProcess: this.activeProcess !== undefined,
      hasStartingProcess: this.startingProcess !== undefined,
    };
  }

  /** Read policy state before any extension code reads chat-session storage. */
  async getPrivacyStatus(): Promise<PrivacyStatus | null> {
    if (!this.client?.isRunning()) {
      return null;
    }
    try {
      const response = await this.client.sendRequest<unknown>("cvc/privacy/status");
      return isPrivacyStatus(response) ? response : null;
    } catch (error) {
      this.outputChannel.appendLine("Privacy status request failed");
      return null;
    }
  }

  /**
   * Send session start notification to the server
   */
  async sendSessionStart(params: SessionStartParams): Promise<void> {
    if (!this.client?.isRunning()) {
      this.outputChannel.appendLine(
        "Warning: Cannot send session/start - client not running",
      );
      return;
    }
    await this.client.sendNotification("$/cvc/session/start", params);
  }

  /**
   * Send turn start notification to the server
   */
  async sendTurnStart(params: TurnStartParams): Promise<void> {
    if (!this.client?.isRunning()) {
      this.outputChannel.appendLine(
        "Warning: Cannot send turn/start - client not running",
      );
      return;
    }
    this.outputChannel.appendLine("LSP: Sending turn/start");
    await this.client.sendNotification("$/cvc/turn/start", params);
  }

  /**
   * Send turn end notification to the server
   */
  async sendTurnEnd(params: TurnEndParams): Promise<void> {
    if (!this.client?.isRunning()) {
      this.outputChannel.appendLine(
        "Warning: Cannot send turn/end - client not running",
      );
      return;
    }
    this.outputChannel.appendLine("LSP: Sending turn/end");
    await this.client.sendNotification("$/cvc/turn/end", params);
  }

  /**
   * Send a batch of segmented interactions to the server.
   * Used by the Chat Session Watcher for retroactive parsing of complete requests.
   */
  async sendTurnBatch(params: TurnBatchParams): Promise<void> {
    if (!this.client?.isRunning()) {
      this.outputChannel.appendLine(
        "Warning: Cannot send turn/batch - client not running",
      );
      return;
    }
    this.outputChannel.appendLine(`LSP: Sending turn/batch (${params.interactions.length} segments)`);
    await this.client.sendNotification("$/cvc/turn/batch", params);
  }

  /**
   * Send link commit notification to the server
   */
  async sendLinkCommit(params: LinkCommitParams): Promise<void> {
    if (!this.client?.isRunning()) {
      this.outputChannel.appendLine(
        "Warning: Cannot send link/commit - client not running",
      );
      return;
    }
    await this.client.sendNotification("$/cvc/link/commit", params);
  }

  /**
   * Request timeline data from the server
   */
  async sendTimelineGet(
    params: TimelineGetParams,
  ): Promise<TimelineGetResponse | null> {
    if (!this.client?.isRunning()) {
      this.outputChannel.appendLine(
        "Warning: Cannot send timeline/get - client not running",
      );
      return null;
    }
    try {
      const response = await this.client.sendRequest<TimelineGetResponse>(
        "cvc/timeline/get",
        params,
      );
      return response;
    } catch (error) {
      this.outputChannel.appendLine("Timeline request failed");
      return null;
    }
  }

  /**
   * Register a handler for timeline refresh notifications from server
   */
  onTimelineRefresh(handler: () => void): vscode.Disposable {
    if (!this.client) {
      return { dispose: () => { } };
    }
    return this.client.onNotification("cvc/timeline/refresh", handler);
  }

  /**
   * Request full details of a specific interaction from the server
   */
  async sendInteractionGet(
    params: InteractionGetParams,
  ): Promise<InteractionDetail | null> {
    if (!this.client?.isRunning()) {
      this.outputChannel.appendLine(
        "Warning: Cannot send interaction/get - client not running",
      );
      return null;
    }
    try {
      const response = await this.client.sendRequest<InteractionDetail>(
        "cvc/interaction/get",
        params,
      );
      return response;
    } catch (error) {
      this.outputChannel.appendLine("Interaction request failed");
      return null;
    }
  }

  /**
   * Find the volute-lsp binary (still named cvc-lsp in the Rust codebase)
   * Priority:
   * 1. User-configured path (volute.lspPath setting)
   * 2. Bundled binary in extension
   * 3. Development build in workspace
   * 4. Well-known install dir (~/.cvc/bin/) and system PATH (via shared findBinary)
   */
  private async findServerBinary(): Promise<string | undefined> {
    // 1. Check user configuration
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    const { getMachineBinaryPath } = require("../setup/machineSettings") as typeof import("../setup/machineSettings");
    const configuredPath = getMachineBinaryPath("lspPath", this.outputChannel);

    if (configuredPath && configuredPath.trim() !== "") {
      const expandedPath = expandPath(configuredPath);
      if (await isExecutable(expandedPath)) {
        return expandedPath;
      }
      this.outputChannel.appendLine("Warning: Configured lspPath is not executable");
    }

    // 2. Check bundled binary in extension
    const bundledPath = this.getBundledBinaryPath();
    if (bundledPath && (await isExecutable(bundledPath))) {
      return bundledPath;
    }

    // 3. Check development build in workspace (relative to extension)
    const devPaths = this.getDevBinaryPaths();
    for (const devPath of devPaths) {
      if (await isExecutable(devPath)) {
        return devPath;
      }
    }

    // 4. Check well-known install dir and system PATH
    return findBinary("cvc-lsp");
  }

  /**
   * Get the path to the bundled binary based on platform
   */
  private getBundledBinaryPath(): string | undefined {
    const platform = process.platform;
    const arch = process.arch;

    let binaryName: string;

    if (platform === "win32") {
      binaryName = "cvc-lsp.exe";
    } else {
      binaryName = "cvc-lsp";
    }

    // Platform-specific subdirectory
    let platformDir: string;
    if (platform === "darwin") {
      platformDir = arch === "arm64" ? "darwin-arm64" : "darwin-x64";
    } else if (platform === "linux") {
      platformDir = arch === "arm64" ? "linux-arm64" : "linux-x64";
    } else if (platform === "win32") {
      platformDir = "win32-x64";
    } else {
      return undefined;
    }

    return path.join(
      this.context.extensionPath,
      "bin",
      platformDir,
      binaryName,
    );
  }

  /**
   * Get paths to check for development builds
   */
  private getDevBinaryPaths(): string[] {
    const extensionDir = this.context.extensionPath;
    const binaryName = process.platform === "win32" ? "cvc-lsp.exe" : "cvc-lsp";

    // Look in the parent directory's target folder (typical Cargo workspace layout)
    return [
      // Debug build
      path.join(extensionDir, "..", "target", "debug", binaryName),
      // Release build
      path.join(extensionDir, "..", "target", "release", binaryName),
      // cvc-lsp specific target (if built separately)
      path.join(extensionDir, "..", "cvc-lsp", "target", "debug", binaryName),
      path.join(extensionDir, "..", "cvc-lsp", "target", "release", binaryName),
    ];
  }
}

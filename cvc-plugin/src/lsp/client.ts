import * as vscode from "vscode";
import * as path from "path";
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  TransportKind,
  Trace,
} from "vscode-languageclient/node";
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
export class VoluteLanguageClient {
  private client: LanguageClient | undefined;
  private readonly context: vscode.ExtensionContext;
  private readonly outputChannel: vscode.OutputChannel;

  constructor(
    context: vscode.ExtensionContext,
    outputChannel: vscode.OutputChannel,
  ) {
    this.context = context;
    this.outputChannel = outputChannel;
  }

  /**
   * Start the language client and connect to the server
   */
  async start(): Promise<void> {
    const serverPath = await this.findServerBinary();

    if (!serverPath) {
      throw new Error(
        "Could not find volute-lsp binary. Please set volute.lspPath in settings or ensure the binary is built.",
      );
    }

    this.outputChannel.appendLine("Using configured Volute language server binary");

    const serverOptions: ServerOptions = {
      run: {
        command: serverPath,
        transport: TransportKind.stdio,
      },
      debug: {
        command: serverPath,
        transport: TransportKind.stdio,
        options: {
          env: {
            ...process.env,
            RUST_LOG: "debug",
            RUST_BACKTRACE: "1",
          },
        },
      },
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
          vscode.workspace.workspaceFolders?.map((f) => f.uri.fsPath) ?? [],
      },
    };

    // Get trace setting
    const config = vscode.workspace.getConfiguration("volute");
    const trace = config.get<string>("trace.server", "off");

    this.client = new LanguageClient(
      "volute-lsp",
      "Volute Language Server",
      serverOptions,
      clientOptions,
    );

    // Set trace level
    if (trace !== "off") {
      if (trace === "verbose") {
        void vscode.window.showWarningMessage(
          "Volute verbose protocol tracing can expose captured chat payloads in its trace output. Disable it unless actively debugging.",
        );
      }
      this.client.setTrace(
        trace === "verbose" ? Trace.Verbose : Trace.Messages,
      );
    }

    // Register client for disposal
    this.context.subscriptions.push(this.client);

    // Start the client
    await this.client.start();
  }

  /**
   * Stop the language client
   */
  async stop(): Promise<void> {
    if (this.client) {
      await this.client.stop();
      this.client = undefined;
    }
  }

  /**
   * Restart the language client
   */
  async restart(): Promise<void> {
    await this.stop();
    await this.start();
  }

  /**
   * Check if the client is running
   */
  isRunning(): boolean {
    return this.client?.isRunning() ?? false;
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
    const config = vscode.workspace.getConfiguration("volute");
    const configuredPath = config.get<string>("lspPath");

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

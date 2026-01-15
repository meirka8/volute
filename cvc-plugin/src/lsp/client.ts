import * as vscode from "vscode";
import * as path from "path";
import * as fs from "fs";
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
  LinkCommitParams,
} from "./protocol";

/**
 * CVC Language Client - manages the connection to the cvc-lsp server
 */
export class CvcLanguageClient {
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
        "Could not find cvc-lsp binary. Please set cvc.lspPath in settings or ensure the binary is built.",
      );
    }

    this.outputChannel.appendLine(`Using cvc-lsp binary: ${serverPath}`);

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
      // CVC doesn't target specific file types - it's a general purpose cognitive tracker
      // We use a broad document selector but the server will handle filtering
      documentSelector: [{ scheme: "file" }],
      outputChannel: this.outputChannel,
      traceOutputChannel: this.outputChannel,
      initializationOptions: {
        workspaceFolders:
          vscode.workspace.workspaceFolders?.map((f) => f.uri.fsPath) ?? [],
      },
    };

    // Get trace setting
    const config = vscode.workspace.getConfiguration("cvc");
    const trace = config.get<string>("trace.server", "off");

    this.client = new LanguageClient(
      "cvc-lsp",
      "CVC Language Server",
      serverOptions,
      clientOptions,
    );

    // Set trace level
    if (trace !== "off") {
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
    await this.client.sendNotification("$/cvc/turn/end", params);
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
   * Find the cvc-lsp binary
   * Priority:
   * 1. User-configured path (cvc.lspPath setting)
   * 2. Bundled binary in extension
   * 3. Development build in workspace
   * 4. System PATH
   */
  private async findServerBinary(): Promise<string | undefined> {
    // 1. Check user configuration
    const config = vscode.workspace.getConfiguration("cvc");
    const configuredPath = config.get<string>("lspPath");

    if (configuredPath && configuredPath.trim() !== "") {
      const expandedPath = this.expandPath(configuredPath);
      if (await this.isExecutable(expandedPath)) {
        return expandedPath;
      }
      this.outputChannel.appendLine(
        `Warning: Configured lspPath "${configuredPath}" is not executable`,
      );
    }

    // 2. Check bundled binary in extension
    const bundledPath = this.getBundledBinaryPath();
    if (bundledPath && (await this.isExecutable(bundledPath))) {
      return bundledPath;
    }

    // 3. Check development build in workspace (relative to extension)
    const devPaths = this.getDevBinaryPaths();
    for (const devPath of devPaths) {
      if (await this.isExecutable(devPath)) {
        return devPath;
      }
    }

    // 4. Check system PATH
    const systemPath = await this.findInPath("cvc-lsp");
    if (systemPath) {
      return systemPath;
    }

    return undefined;
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

  /**
   * Expand ~ and environment variables in path
   */
  private expandPath(inputPath: string): string {
    let expanded = inputPath;

    // Expand ~
    if (expanded.startsWith("~")) {
      const home = process.env.HOME || process.env.USERPROFILE || "";
      expanded = path.join(home, expanded.slice(1));
    }

    // Expand environment variables ($VAR or ${VAR})
    expanded = expanded.replace(/\$\{?(\w+)\}?/g, (_, varName) => {
      return process.env[varName] || "";
    });

    return expanded;
  }

  /**
   * Check if a file exists and is executable
   */
  private async isExecutable(filePath: string): Promise<boolean> {
    try {
      await fs.promises.access(filePath, fs.constants.X_OK);
      const stats = await fs.promises.stat(filePath);
      return stats.isFile();
    } catch {
      return false;
    }
  }

  /**
   * Find an executable in the system PATH
   */
  private async findInPath(executable: string): Promise<string | undefined> {
    const pathEnv = process.env.PATH || "";
    const pathSeparator = process.platform === "win32" ? ";" : ":";
    const paths = pathEnv.split(pathSeparator);

    const extensions =
      process.platform === "win32" ? ["", ".exe", ".cmd", ".bat"] : [""];

    for (const dir of paths) {
      for (const ext of extensions) {
        const fullPath = path.join(dir, executable + ext);
        if (await this.isExecutable(fullPath)) {
          return fullPath;
        }
      }
    }

    return undefined;
  }
}

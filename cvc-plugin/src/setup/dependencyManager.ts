import * as vscode from "vscode";
import * as path from "path";
import * as fs from "fs";
import { findBinary } from "./binaryUtils";

// ── Constants ──────────────────────────────────────────────────────────────

/**
 * Installation guidance is intentionally sourced from the public repository.
 * The published installers place the matching release artifact in the
 * standard ~/.cvc/bin location.
 */
const SETUP_PAGE_URL = "https://github.com/meirka8/volute/releases";
const RAW_INSTALLER_BASE_URL = "https://raw.githubusercontent.com/meirka8/volute";
const SEMVER_VERSION = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/;

/** How often (ms) to re-prompt after a "Not Now" dismissal — 7 days */
const REMINDER_INTERVAL_MS = 7 * 24 * 60 * 60 * 1000;

/** Keys used to persist dismissal timestamps in globalState */
const DISMISS_KEY_CLI = "volute.setup.dismissedCli";
const DISMISS_KEY_LSP = "volute.setup.dismissedLsp";
const DISMISS_KEY_MCP = "volute.setup.dismissedMcp";
const DISMISS_KEY_INIT = "volute.setup.dismissedInit";

// ── Types ──────────────────────────────────────────────────────────────────

export interface DependencyStatus {
  cvcLsp: { found: boolean; path?: string };
  cvcCli: { found: boolean; path?: string };
  cvcMcp: { found: boolean; path?: string };
  repoInitialized: boolean;
}

export interface InstallerUrls {
  sh: string;
  ps1: string;
  releaseTag: string;
}

type InstallerPlatform = "win32" | "unix";

/**
 * Return version-pinned public installer URLs for this extension release.
 *
 * The package version is treated as untrusted input even though it originates
 * from extension metadata: validating it before composing the URL prevents a
 * malformed package from selecting an arbitrary Git revision or URL path.
 */
export function getInstallerUrls(version: unknown): InstallerUrls | undefined {
  if (typeof version !== "string" || !SEMVER_VERSION.test(version)) {
    return undefined;
  }

  const releaseTag = `v${version}`;
  return {
    sh: `${RAW_INSTALLER_BASE_URL}/${releaseTag}/install.sh`,
    ps1: `${RAW_INSTALLER_BASE_URL}/${releaseTag}/install.ps1`,
    releaseTag,
  };
}

/**
 * Build the install command shown in the terminal. The terminal only receives
 * typed text (`sendText(..., false)`), so the user must explicitly execute it.
 * The validated extension version pins both the installer source and release.
 */
export function createInstallCommand(
  platform: InstallerPlatform,
  installerUrls: InstallerUrls,
): string {
  if (platform === "win32") {
    // New-TemporaryFile creates a unique file in the OS temp directory. The
    // try/finally removes it after both successful and failed installations.
    return `powershell -NoProfile -ExecutionPolicy Bypass -Command "& { $ErrorActionPreference = 'Stop'; $env:CVC_RELEASE_VERSION = '${installerUrls.releaseTag}'; $tempFile = (New-TemporaryFile).FullName; try { Invoke-WebRequest -Uri '${installerUrls.ps1}' -OutFile $tempFile; & $tempFile } finally { Remove-Item -LiteralPath $tempFile -Force -ErrorAction SilentlyContinue } }"`;
  }

  return `curl -fsSL '${installerUrls.sh}' | CVC_RELEASE_VERSION='${installerUrls.releaseTag}' sh`;
}

/**
 * Build a direct process invocation for `cvc init` without involving a shell.
 * In particular, a configured executable path is data, never terminal text.
 */
export function createCvcInitExecution(
  cvcBinary: string | undefined,
  cwd: string | undefined,
): vscode.ProcessExecution {
  return new vscode.ProcessExecution(cvcBinary ?? "cvc", ["init"], cwd ? { cwd } : undefined);
}

// ── Detection ──────────────────────────────────────────────────────────────

/**
 * Detect which CVC components are available on this machine.
 */
export async function detectDependencies(
  outputChannel: vscode.OutputChannel,
): Promise<DependencyStatus> {
  const config = vscode.workspace.getConfiguration("volute");

  // Detect each binary (user-configured path → ~/.cvc/bin → PATH)
  const [cvcLspPath, cvcCliPath, cvcMcpPath] = await Promise.all([
    findBinary("cvc-lsp", config.get<string>("lspPath")),
    findBinary("cvc", config.get<string>("cvcCliPath")),
    findBinary("cvc-mcp", config.get<string>("cvcMcpPath")),
  ]);

  // Check repo initialization (.git/cvc/index.db)
  let repoInitialized = false;
  const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
  if (workspaceFolder) {
    const dbPath = path.join(
      workspaceFolder.uri.fsPath,
      ".git",
      "cvc",
      "index.db",
    );
    try {
      await fs.promises.access(dbPath, fs.constants.F_OK);
      repoInitialized = true;
    } catch {
      // not initialized
    }
  }

  const status: DependencyStatus = {
    cvcLsp: { found: !!cvcLspPath, path: cvcLspPath },
    cvcCli: { found: !!cvcCliPath, path: cvcCliPath },
    cvcMcp: { found: !!cvcMcpPath, path: cvcMcpPath },
    repoInitialized,
  };

  outputChannel.appendLine(
    `Dependency check: CLI=${status.cvcCli.found}, LSP=${status.cvcLsp.found}, MCP=${status.cvcMcp.found}, Repo initialized=${status.repoInitialized}`,
  );

  return status;
}

// ── Prompting ──────────────────────────────────────────────────────────────

/**
 * Show non-intrusive prompts for any missing components.
 * Respects previous "Not Now" dismissals (stored in globalState).
 *
 * @returns `true` if the critical components (CLI + LSP) are present,
 *          `false` if the user needs to install them first.
 */
export async function promptForMissingDependencies(
  context: vscode.ExtensionContext,
  status: DependencyStatus,
  outputChannel: vscode.OutputChannel,
): Promise<boolean> {
  const config = vscode.workspace.getConfiguration("volute");
  if (config.get<boolean>("suppressSetupPrompts", false)) {
    outputChannel.appendLine("Setup prompts suppressed via user setting");
    return status.cvcCli.found && status.cvcLsp.found;
  }

  // ── Critical: CVC CLI ──────────────────────────────────────────────────
  if (!status.cvcCli.found && shouldPrompt(context, DISMISS_KEY_CLI)) {
    promptInstall(
      context,
      DISMISS_KEY_CLI,
      "CVC CLI is not installed. It is required for CVC to function (init, log, push/pull).",
      "Install CVC CLI",
    );
  }

  // ── Critical: CVC LSP (only if CLI is present — LSP ships with CLI) ───
  if (
    !status.cvcLsp.found &&
    status.cvcCli.found &&
    shouldPrompt(context, DISMISS_KEY_LSP)
  ) {
    promptInstall(
      context,
      DISMISS_KEY_LSP,
      "CVC Language Server (cvc-lsp) was not found. The extension needs it to function.",
      "Install CVC",
    );
  }

  // ── Optional: CVC MCP ─────────────────────────────────────────────────
  if (!status.cvcMcp.found && shouldPrompt(context, DISMISS_KEY_MCP)) {
    promptOptional(
      context,
      DISMISS_KEY_MCP,
      "Enhance your AI agent workflows with the CVC MCP Server. " +
        "It allows agents (Claude, Cursor, Windsurf) to submit exposed reasoning when their integration provides it.",
      "Install CVC MCP",
    );
  }

  // ── Repo init check ────────────────────────────────────────────────────
  if (
    status.cvcCli.found &&
    !status.repoInitialized &&
    vscode.workspace.workspaceFolders?.length &&
    shouldPrompt(context, DISMISS_KEY_INIT)
  ) {
    promptInit(context, status);
  }

  return status.cvcCli.found && status.cvcLsp.found;
}

// ── Helpers (private-ish) ──────────────────────────────────────────────────

/** Has enough time elapsed since the user last dismissed this prompt? */
function shouldPrompt(
  context: vscode.ExtensionContext,
  dismissKey: string,
): boolean {
  const dismissed = context.globalState.get<number>(dismissKey);
  if (!dismissed) {
    return true;
  }
  return Date.now() - dismissed > REMINDER_INTERVAL_MS;
}

/** Record the current timestamp as a dismissal for this prompt key. */
function recordDismissal(
  context: vscode.ExtensionContext,
  key: string,
): void {
  context.globalState.update(key, Date.now());
}

/**
 * Show a warning-level notification with [Install] [Learn More] [Not Now].
 * "Install" opens a terminal with the install script.
 * "Learn More" opens the setup page.
 */
function promptInstall(
  context: vscode.ExtensionContext,
  dismissKey: string,
  message: string,
  installLabel: string,
): void {
  vscode.window
    .showWarningMessage(message, installLabel, "Learn More", "Not Now")
    .then((choice) => {
      if (choice === installLabel) {
        openInstallTerminal(context);
      } else if (choice === "Learn More") {
        vscode.env.openExternal(vscode.Uri.parse(SETUP_PAGE_URL));
      } else if (choice === "Not Now") {
        recordDismissal(context, dismissKey);
      }
    });
}

/**
 * Show an info-level notification for optional components.
 */
function promptOptional(
  context: vscode.ExtensionContext,
  dismissKey: string,
  message: string,
  installLabel: string,
): void {
  vscode.window
    .showInformationMessage(message, installLabel, "Learn More", "Not Now")
    .then((choice) => {
      if (choice === installLabel) {
        openInstallTerminal(context);
      } else if (choice === "Learn More") {
        vscode.env.openExternal(vscode.Uri.parse(SETUP_PAGE_URL));
      } else if (choice === "Not Now") {
        recordDismissal(context, dismissKey);
      }
    });
}

/**
 * Prompt the user to initialize CVC in the current repository.
 */
function promptInit(
  context: vscode.ExtensionContext,
  status: DependencyStatus,
): void {
  vscode.window
    .showInformationMessage(
      "This repository hasn't been initialized for CVC yet. Would you like to initialize it?",
      "Run cvc init",
      "Not Now",
    )
    .then((choice) => {
      if (choice === "Run cvc init") {
        const cvcBinary = status.cvcCli.path ?? "cvc";
        const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
        const task = new vscode.Task(
          { type: "cvc", task: "init" },
          workspaceFolder ?? vscode.TaskScope.Workspace,
          "Initialize repository",
          "CVC",
          createCvcInitExecution(cvcBinary, workspaceFolder?.uri.fsPath),
        );

        // ProcessExecution uses spawn-style argument arrays, so a configurable
        // binary path cannot be parsed as shell syntax or inject extra commands.
        void vscode.tasks.executeTask(task).then(
          undefined,
          (error: unknown) => {
            const detail = error instanceof Error ? `: ${error.message}` : "";
            vscode.window.showErrorMessage(`CVC: Failed to start initialization${detail}`);
          },
        );
      } else if (choice === "Not Now") {
        recordDismissal(context, DISMISS_KEY_INIT);
      }
    });
}

/**
 * Open a terminal with the appropriate install command for the current OS.
 * For security reasons, the command is typed into the terminal but NOT 
 * automatically executed (requires the user to press Enter).
 */
function openInstallTerminal(context: vscode.ExtensionContext): void {
  const installerUrls = getInstallerUrls(context.extension.packageJSON.version);
  if (!installerUrls) {
    vscode.window.showErrorMessage(
      "CVC: This extension has an invalid version, so a version-pinned installer cannot be selected.",
    );
    void vscode.env.openExternal(vscode.Uri.parse(SETUP_PAGE_URL));
    return;
  }

  const terminal = vscode.window.createTerminal("CVC Install");
  terminal.show();

  if (process.platform === "win32") {
    terminal.sendText("# Press Enter to run the CVC installation script");
    terminal.sendText(
      createInstallCommand("win32", installerUrls),
      false // Do not auto-execute
    );
  } else {
    terminal.sendText("# Press Enter to run the CVC installation script");
    terminal.sendText(createInstallCommand("unix", installerUrls), false); // Do not auto-execute
  }
}

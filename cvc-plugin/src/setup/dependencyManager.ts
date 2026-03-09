import * as vscode from "vscode";
import * as path from "path";
import * as fs from "fs";
import { findBinary } from "./binaryUtils";

// ── Constants ──────────────────────────────────────────────────────────────

const SETUP_PAGE_URL = "https://cvc.dev/setup";
const INSTALL_SCRIPT_URL =
  "https://raw.githubusercontent.com/meirka8/cvc/main/install.sh";
const INSTALL_PS1_URL =
  "https://raw.githubusercontent.com/meirka8/cvc/main/install.ps1";

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
    `Dependency check: CLI=${status.cvcCli.found ? status.cvcCli.path : "MISSING"}, ` +
      `LSP=${status.cvcLsp.found ? status.cvcLsp.path : "MISSING"}, ` +
      `MCP=${status.cvcMcp.found ? status.cvcMcp.path : "MISSING"}, ` +
      `Repo initialized=${status.repoInitialized}`,
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
        "It allows agents (Claude, Cursor, Windsurf) to record their reasoning automatically.",
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
        openInstallTerminal();
      } else if (choice === "Learn More") {
        vscode.env.openExternal(vscode.Uri.parse(SETUP_PAGE_URL));
      } else {
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
        openInstallTerminal();
      } else if (choice === "Learn More") {
        vscode.env.openExternal(vscode.Uri.parse(SETUP_PAGE_URL));
      } else {
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
        const terminal = vscode.window.createTerminal("CVC Init");
        terminal.show();
        terminal.sendText(`${cvcBinary} init`);
      } else {
        recordDismissal(context, DISMISS_KEY_INIT);
      }
    });
}

/**
 * Open a terminal with the appropriate install command for the current OS.
 */
function openInstallTerminal(): void {
  const terminal = vscode.window.createTerminal("CVC Install");
  terminal.show();

  if (process.platform === "win32") {
    terminal.sendText(
      `powershell -ExecutionPolicy Bypass -Command "& {Invoke-WebRequest -Uri '${INSTALL_PS1_URL}' -OutFile cvc-install.ps1; .\\cvc-install.ps1; Remove-Item cvc-install.ps1}"`,
    );
  } else {
    terminal.sendText(`curl -fsSL '${INSTALL_SCRIPT_URL}' | sh`);
  }
}

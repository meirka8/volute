import * as vscode from "vscode";
import * as path from "path";
import { VoluteLanguageClient } from "./lsp/client";
import { ChatSessionWatcher } from "./watcher/chatSessionWatcher";
import { TimelineTreeProvider } from "./timeline/provider";
import { ThoughtDetailPanel } from "./webview/thoughtDetailPanel";
import { PrivacyStatus } from "./lsp/protocol";
import { isSafeInteractionId, PassiveWatcherGate } from "./privacy";
import {
  detectDependencies,
  promptForMissingDependencies,
} from "./setup/dependencyManager";
import { getActiveWorkspaceRoot } from "./setup/workspaceRoot";
import { getTrustedGitPath } from "./setup/gitPath";
import { BindingToken } from "./setup/bindingToken";

let client: VoluteLanguageClient | undefined;
let chatSessionWatcher: ChatSessionWatcher | undefined;
const watcherGate = new PassiveWatcherGate<ChatSessionWatcher>();
let timelineProvider: TimelineTreeProvider | undefined;
let privacyStatus: PrivacyStatus | undefined;
let privacyStatusBar: vscode.StatusBarItem | undefined;
let repositoryActionsEnabled = false;
let boundWorkspaceRoot: vscode.WorkspaceFolder | undefined;
let bindingToken: BindingToken | undefined;
let cvcBinary: string | undefined;

async function disableRepositoryActions(outputChannel: vscode.OutputChannel): Promise<void> {
  bindingToken?.cancel();
  if (!repositoryActionsEnabled) {
    return;
  }
  repositoryActionsEnabled = false;
  watcherGate.dispose();
  chatSessionWatcher = undefined;
  privacyStatus = undefined;
  const activeClient = client;
  client = undefined;
  if (activeClient) {
    await activeClient.stop();
  }
  updatePrivacyIndicator(undefined);
  outputChannel.appendLine("CVC repository binding was removed; reload after changing workspace folders.");
}

function repositoryActionsAvailable(): boolean {
  if (repositoryActionsEnabled && bindingToken?.isActive() && vscode.workspace.isTrusted) {
    return true;
  }
  void vscode.window.showWarningMessage("CVC is inactive for this workspace. Reload with one trusted repository folder.");
  return false;
}

function updatePrivacyIndicator(status: PrivacyStatus | undefined): void {
  if (!privacyStatusBar) {
    return;
  }
  if (status?.passiveCaptureAllowed) {
    privacyStatusBar.text = "$(shield) CVC: local capture enabled";
    privacyStatusBar.tooltip = `${status.privateDefaultStatement} ${status.sharingSummary}`;
  } else {
    privacyStatusBar.text = "$(shield) CVC: capture not acknowledged";
    privacyStatusBar.tooltip = "Passive chat capture is off until you acknowledge it in the CVC CLI.";
  }
  privacyStatusBar.show();
}

async function showPrivacyNotice(): Promise<void> {
  const choice = await vscode.window.showInformationMessage(
    "CVC can read this workspace’s local VS Code chat sessions only after you acknowledge capture.",
    { modal: true, detail: "Captured locally: prompts, responses, exposed reasoning, and tool/context patches. Data is stored in this repository’s Git common directory under cvc/index.db and is private by default. Sharing uses the hidden refs/cvc/main Git ref only after separate remote consent/share. Scrubbing helps detect secrets but cannot guarantee removal; Git sharing can be permanent." },
    "Not now",
    "Learn more",
    "Open terminal to acknowledge",
  );
  if (choice === "Learn more") {
    await vscode.commands.executeCommand("workbench.action.openWalkthrough", "volute.welcome#volute.welcome.privacy");
  } else if (choice === "Open terminal to acknowledge") {
    await vscode.commands.executeCommand("volute.acknowledgePrivacyCapture");
  }
  // "Not now" intentionally leaves passive capture disabled without recording a choice.
}

export async function activate(
  context: vscode.ExtensionContext,
): Promise<void> {
  const outputChannel = vscode.window.createOutputChannel("Volute CVC");
  outputChannel.appendLine("Activating Volute CVC extension...");
  privacyStatusBar = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 100);
  privacyStatusBar.command = "volute.refreshPrivacyStatus";
  context.subscriptions.push(privacyStatusBar);
  updatePrivacyIndicator(undefined);

  if (!vscode.workspace.isTrusted) {
    outputChannel.appendLine("CVC is inactive because this workspace is not trusted.");
    context.subscriptions.push(outputChannel);
    return;
  }

  // ── Dependency Check (greenfield entrypoint) ───────────────────────────
  const workspaceRoot = getActiveWorkspaceRoot(outputChannel);
  if (!workspaceRoot) {
    context.subscriptions.push(outputChannel);
    return;
  }
  boundWorkspaceRoot = workspaceRoot;
  bindingToken = new BindingToken();
  repositoryActionsEnabled = true;
  context.subscriptions.push(vscode.workspace.onDidChangeWorkspaceFolders(() => {
    void disableRepositoryActions(outputChannel);
  }));
  const gitPath = await getTrustedGitPath();
  if (!bindingToken.isActive()) { return; }
  const status = await detectDependencies(outputChannel, workspaceRoot.uri.fsPath, gitPath);
  if (!bindingToken.isActive()) { return; }
  cvcBinary = status.cvcCli.path;
  await promptForMissingDependencies(
    context,
    status,
    outputChannel,
    workspaceRoot,
    () => bindingToken?.isActive() === true && vscode.workspace.isTrusted,
  );
  if (!bindingToken.isActive() || !vscode.workspace.isTrusted) { return; }

  if (!status.cvcCli.found || !status.cvcLsp.found) {
    const missing = [];
    if (!status.cvcCli.found) {
      missing.push("CVC CLI");
    }
    if (!status.cvcLsp.found) {
      missing.push("CVC LSP");
    }

    outputChannel.appendLine(
      `Critical dependencies missing (${missing.join(", ")}) — running in degraded mode`,
    );
  }

  // ── Initialize and start the LSP client (only if LSP binary exists) ────
  if (status.cvcLsp.found && workspaceRoot && bindingToken.isActive()) {
    const localClient = new VoluteLanguageClient(context, outputChannel, workspaceRoot);
    client = localClient;

    try {
      await localClient.start(() => bindingToken?.isActive() === true && vscode.workspace.isTrusted);
      if (!bindingToken.isActive() || !vscode.workspace.isTrusted) {
        await localClient.stop();
        if (client === localClient) {client = undefined;}
        return;
      }
      outputChannel.appendLine("Volute Language Server started successfully");
    } catch (error) {
      await localClient.stop();
      if (client === localClient) {client = undefined;}
      outputChannel.appendLine("Failed to start Volute Language Server");
      vscode.window.showErrorMessage(
        "Volute CVC: Failed to start language server. See the output channel for status.",
      );
    }
  }

  // Register the Cognitive Timeline tree view (only fully functional with LSP)
  if (client) {
    timelineProvider = new TimelineTreeProvider(outputChannel, client);
    const treeView = vscode.window.createTreeView("volute.timeline", {
      treeDataProvider: timelineProvider,
      showCollapseAll: true,
  });
  context.subscriptions.push(treeView);

    // Register for timeline refresh notifications from server
    context.subscriptions.push(
      client.onTimelineRefresh(() => {
        timelineProvider?.refresh();
      }),
    );

    // Initial timeline load
    timelineProvider.refresh();
  }

  // Register commands
  context.subscriptions.push(
    vscode.commands.registerCommand("volute.refreshPrivacyStatus", async () => {
      if (!repositoryActionsAvailable()) {
        return;
      }
      privacyStatus = await client?.getPrivacyStatus() ?? undefined;
      if (!repositoryActionsEnabled) {
        return;
      }
      if (!privacyStatus?.passiveCaptureAllowed) {
        // A refresh is a revocation boundary. Stop and discard the existing
        // observer before showing any UI that could prompt for re-acknowledgement.
        watcherGate.reconcile(privacyStatus, () => {
          throw new Error("Watcher creation is not permitted without acknowledgement");
        });
        chatSessionWatcher = undefined;
        updatePrivacyIndicator(privacyStatus);
        await showPrivacyNotice();
        return;
      }
      updatePrivacyIndicator(privacyStatus);
      // Construct and read chat-session storage only after the read-only LSP
      // status has confirmed the current acknowledgement.
      if (client) {
        const previousWatcher = chatSessionWatcher;
        chatSessionWatcher = watcherGate.reconcile(
          privacyStatus,
          () => new ChatSessionWatcher(outputChannel, client!, boundWorkspaceRoot!),
        );
        if (chatSessionWatcher && chatSessionWatcher !== previousWatcher) {
        try {
          await chatSessionWatcher.start(context);
          outputChannel.appendLine("Chat Session Watcher started after privacy acknowledgement");
        } catch {
          outputChannel.appendLine("Failed to start Chat Session Watcher");
          watcherGate.dispose();
          chatSessionWatcher = undefined;
        }
        }
      }
    }),

    vscode.commands.registerCommand("volute.acknowledgePrivacyCapture", async () => {
      if (!repositoryActionsAvailable() || !boundWorkspaceRoot) {
        return;
      }
      runCvcTask("Acknowledge Privacy", ["privacy", "acknowledge-capture"]);
      vscode.window.showInformationMessage("Complete the terminal challenge, then run ‘CVC: Refresh Privacy Status’." );
    }),

    vscode.commands.registerCommand("volute.shareRemote", async () => {
      if (!repositoryActionsAvailable() || !boundWorkspaceRoot) {
        return;
      }
      const conversationId = await vscode.window.showInputBox({ prompt: "Conversation ID to share" });
      if (!conversationId) {
        return;
      }
      const remote = await vscode.window.showInputBox({ prompt: "Remote name to share with", value: "origin" });
      if (!remote) {
        return;
      }
      runCvcTask("Share", ["share", conversationId, "--remote", remote]);
    }),

    vscode.commands.registerCommand("volute.restartServer", async () => {
      if (!repositoryActionsAvailable()) {
        return;
      }
      const localClient = client;
      if (localClient) {
        outputChannel.appendLine("Restarting Volute Language Server...");
        await localClient.restart(() => bindingToken?.isActive() === true && vscode.workspace.isTrusted);
        if (!bindingToken?.isActive() || !vscode.workspace.isTrusted) {
          await localClient.stop();
          if (client === localClient) {client = undefined;}
          return;
        }
        outputChannel.appendLine("Volute Language Server restarted");
        // Refresh timeline after server restart
        timelineProvider?.refresh();
      }
    }),

    vscode.commands.registerCommand("volute.refreshTimeline", () => {
      if (!repositoryActionsAvailable()) {
        return;
      }
      outputChannel.appendLine("Timeline refresh requested");
      timelineProvider?.refresh();
    }),

    vscode.commands.registerCommand(
      "volute.openThoughtDetail",
      (interactionId: string) => {
        if (!repositoryActionsAvailable()) {
          return;
        }
        if (!isSafeInteractionId(interactionId)) {
          outputChannel.appendLine("Thought detail request rejected: invalid interaction ID");
          return;
        }

        outputChannel.appendLine(`Opening thought detail: ${interactionId}`);

        if (client) {
          ThoughtDetailPanel.createOrShow(
            context.extensionUri,
            outputChannel,
            client,
            interactionId,
          );
        }
      },
    ),

    vscode.commands.registerCommand("volute.checkSetup", async () => {
      if (!repositoryActionsAvailable() || !boundWorkspaceRoot) {
        return;
      }
      const gitPath = await getTrustedGitPath();
      if (!bindingToken?.isActive()) { return; }
      const freshStatus = await detectDependencies(outputChannel, boundWorkspaceRoot.uri.fsPath, gitPath);
      if (!bindingToken?.isActive() || !vscode.workspace.isTrusted) { return; }
      await promptForMissingDependencies(context, freshStatus, outputChannel, boundWorkspaceRoot, () => bindingToken?.isActive() === true && vscode.workspace.isTrusted);

      if (freshStatus.cvcCli.found && freshStatus.cvcLsp.found) {
        if (!bindingToken?.isActive() || !vscode.workspace.isTrusted) { return; }
        vscode.window.showInformationMessage(
          "Volute CVC: All required components are installed! ✓",
        );
      }
    }),
  );

  // This is the sole activation path for passive observation. It queries the
  // LSP first; an unacknowledged installation never constructs the watcher.
  await vscode.commands.executeCommand("volute.refreshPrivacyStatus");

  // Add output channel to subscriptions for cleanup
  context.subscriptions.push(outputChannel);
}

/** Quote user input for the integrated shell without changing its value. */
function runCvcTask(name: string, args: string[]): void {
  if (!repositoryActionsAvailable() || !boundWorkspaceRoot || !cvcBinary || !path.isAbsolute(cvcBinary)) {
    return;
  }
  const token = bindingToken;
  if (!token?.isActive()) {return;}
  const task = new vscode.Task({ type: "cvc", task: name }, boundWorkspaceRoot, name, "CVC", new vscode.ProcessExecution(cvcBinary, args, { cwd: boundWorkspaceRoot.uri.fsPath }));
  void vscode.tasks.executeTask(task);
}

export async function deactivate(): Promise<void> {
  bindingToken?.cancel();
  repositoryActionsEnabled = false;
  watcherGate.dispose();
  chatSessionWatcher = undefined;
  boundWorkspaceRoot = undefined;
  if (client) {
    await client.stop();
    client = undefined;
  }
}

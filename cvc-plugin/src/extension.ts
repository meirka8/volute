import * as vscode from "vscode";
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

let client: VoluteLanguageClient | undefined;
let chatSessionWatcher: ChatSessionWatcher | undefined;
const watcherGate = new PassiveWatcherGate<ChatSessionWatcher>();
let timelineProvider: TimelineTreeProvider | undefined;
let privacyStatus: PrivacyStatus | undefined;
let privacyStatusBar: vscode.StatusBarItem | undefined;

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
    { modal: true, detail: "Captured locally: prompts, responses, exposed reasoning, and tool/context patches. Data is stored in this repository’s .git/cvc/index.db and is private by default. Sharing uses the hidden refs/cvc/main Git ref only after separate remote consent/share. Scrubbing helps detect secrets but cannot guarantee removal; Git sharing can be permanent." },
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

  // ── Dependency Check (greenfield entrypoint) ───────────────────────────
  const status = await detectDependencies(outputChannel);
  await promptForMissingDependencies(
    context,
    status,
    outputChannel,
  );

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
  if (status.cvcLsp.found) {
    client = new VoluteLanguageClient(context, outputChannel);

    try {
      await client.start();
      outputChannel.appendLine("Volute Language Server started successfully");
    } catch (error) {
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
      privacyStatus = await client?.getPrivacyStatus() ?? undefined;
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
          () => new ChatSessionWatcher(outputChannel, client!),
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
      const terminal = vscode.window.createTerminal({ name: "CVC Privacy" });
      terminal.show();
      // Opening a terminal does not change extension state. The user must pass
      // the CLI's TTY challenge and then explicitly refresh policy status.
      terminal.sendText("cvc privacy acknowledge-capture");
      vscode.window.showInformationMessage("Complete the terminal challenge, then run ‘CVC: Refresh Privacy Status’." );
    }),

    vscode.commands.registerCommand("volute.shareRemote", async () => {
      const conversationId = await vscode.window.showInputBox({ prompt: "Conversation ID to share" });
      if (!conversationId) {
        return;
      }
      const remote = await vscode.window.showInputBox({ prompt: "Remote name to share with", value: "origin" });
      if (!remote) {
        return;
      }
      const terminal = vscode.window.createTerminal({ name: "CVC Sharing" });
      terminal.show();
      // This invokes the CLI's separate share flow; the extension never
      // changes sharing consent or the database itself.
      terminal.sendText(`cvc share ${shellQuote(conversationId)} --remote ${shellQuote(remote)}`);
    }),

    vscode.commands.registerCommand("volute.restartServer", async () => {
      if (client) {
        outputChannel.appendLine("Restarting Volute Language Server...");
        await client.restart();
        outputChannel.appendLine("Volute Language Server restarted");
        // Refresh timeline after server restart
        timelineProvider?.refresh();
      }
    }),

    vscode.commands.registerCommand("volute.refreshTimeline", () => {
      outputChannel.appendLine("Timeline refresh requested");
      timelineProvider?.refresh();
    }),

    vscode.commands.registerCommand(
      "volute.openThoughtDetail",
      (interactionId: string) => {
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
      const freshStatus = await detectDependencies(outputChannel);
      await promptForMissingDependencies(context, freshStatus, outputChannel);

      if (freshStatus.cvcCli.found && freshStatus.cvcLsp.found) {
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
function shellQuote(value: string): string {
  return `'${value.replace(/'/g, "'\\''")}'`;
}

export async function deactivate(): Promise<void> {
  watcherGate.dispose();
  chatSessionWatcher = undefined;
  if (client) {
    await client.stop();
  }
}

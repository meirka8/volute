import * as vscode from "vscode";
import { CvcLanguageClient } from "./lsp/client";
import { CvcChatParticipant } from "./chat/participant";
import { TimelineTreeProvider } from "./timeline/provider";
import { ThoughtDetailPanel } from "./webview/thoughtDetailPanel";

let client: CvcLanguageClient | undefined;
let chatParticipant: CvcChatParticipant | undefined;
let timelineProvider: TimelineTreeProvider | undefined;

export async function activate(
  context: vscode.ExtensionContext,
): Promise<void> {
  const outputChannel = vscode.window.createOutputChannel("CVC");
  outputChannel.appendLine("Activating Cognitive Version Control extension...");

  // Initialize and start the LSP client
  client = new CvcLanguageClient(context, outputChannel);

  try {
    await client.start();
    outputChannel.appendLine("CVC Language Server started successfully");
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    outputChannel.appendLine(`Failed to start CVC Language Server: ${message}`);
    vscode.window.showErrorMessage(
      `CVC: Failed to start language server. ${message}`,
    );
  }

  // Register the @cvc chat participant
  chatParticipant = new CvcChatParticipant(outputChannel, client);
  chatParticipant.register(context);

  // Register the Cognitive Timeline tree view
  timelineProvider = new TimelineTreeProvider(outputChannel, client);
  const treeView = vscode.window.createTreeView("cvc.timeline", {
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

  // Register commands
  context.subscriptions.push(
    vscode.commands.registerCommand("cvc.restartServer", async () => {
      if (client) {
        outputChannel.appendLine("Restarting CVC Language Server...");
        await client.restart();
        outputChannel.appendLine("CVC Language Server restarted");
        // Refresh timeline after server restart
        timelineProvider?.refresh();
      }
    }),

    vscode.commands.registerCommand("cvc.refreshTimeline", () => {
      outputChannel.appendLine("Timeline refresh requested");
      timelineProvider?.refresh();
    }),

    vscode.commands.registerCommand(
      "cvc.openThoughtDetail",
      (interactionId: string) => {
        if (!interactionId) {
          outputChannel.appendLine("No interaction ID provided");
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
  );

  // Add output channel to subscriptions for cleanup
  context.subscriptions.push(outputChannel);
}

export async function deactivate(): Promise<void> {
  if (chatParticipant) {
    chatParticipant.dispose();
  }
  if (client) {
    await client.stop();
  }
}

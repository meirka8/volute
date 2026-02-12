import * as vscode from "vscode";
import { VoluteLanguageClient } from "./lsp/client";
import { VoloteChatParticipant } from "./chat/participant";
import { ChatSessionWatcher } from "./watcher/chatSessionWatcher";
import { TimelineTreeProvider } from "./timeline/provider";
import { ThoughtDetailPanel } from "./webview/thoughtDetailPanel";

let client: VoluteLanguageClient | undefined;
let chatParticipant: VoloteChatParticipant | undefined;
let chatSessionWatcher: ChatSessionWatcher | undefined;
let timelineProvider: TimelineTreeProvider | undefined;

export async function activate(
  context: vscode.ExtensionContext,
): Promise<void> {
  const outputChannel = vscode.window.createOutputChannel("Volute VC");
  outputChannel.appendLine("Activating Volute VC extension...");

  // Initialize and start the LSP client
  client = new VoluteLanguageClient(context, outputChannel);

  try {
    await client.start();
    outputChannel.appendLine("Volute Language Server started successfully");
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    outputChannel.appendLine(
      `Failed to start Volute Language Server: ${message}`,
    );
    vscode.window.showErrorMessage(
      `Volute VC: Failed to start language server. ${message}`,
    );
  }

  // Start the Chat Session Watcher (passive Copilot observation)
  chatSessionWatcher = new ChatSessionWatcher(outputChannel, client);
  try {
    await chatSessionWatcher.start(context);
    outputChannel.appendLine("Chat Session Watcher started successfully");
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    outputChannel.appendLine(
      `Failed to start Chat Session Watcher: ${message}`,
    );
    // Non-fatal - the extension can still work without passive observation
  }

  // Register the @volute chat participant (alternative explicit logging mode)
  chatParticipant = new VoloteChatParticipant(outputChannel, client);
  chatParticipant.register(context);

  // Register the Cognitive Timeline tree view
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

  // Register commands
  context.subscriptions.push(
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
  if (chatSessionWatcher) {
    chatSessionWatcher.stop();
  }
  if (chatParticipant) {
    chatParticipant.dispose();
  }
  if (client) {
    await client.stop();
  }
}

import * as vscode from "vscode";
import { CvcLanguageClient } from "./lsp/client";
import { CvcChatParticipant } from "./chat/participant";

let client: CvcLanguageClient | undefined;
let chatParticipant: CvcChatParticipant | undefined;

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

  // Register commands
  context.subscriptions.push(
    vscode.commands.registerCommand("cvc.restartServer", async () => {
      if (client) {
        outputChannel.appendLine("Restarting CVC Language Server...");
        await client.restart();
        outputChannel.appendLine("CVC Language Server restarted");
      }
    }),

    vscode.commands.registerCommand("cvc.refreshTimeline", () => {
      // TODO: Implement timeline refresh when TreeView is added
      outputChannel.appendLine("Timeline refresh requested");
    }),

    vscode.commands.registerCommand(
      "cvc.openThoughtDetail",
      (interactionId: string) => {
        // TODO: Implement thought detail webview when Feature 4 is added
        outputChannel.appendLine(
          `Open thought detail requested: ${interactionId}`,
        );
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

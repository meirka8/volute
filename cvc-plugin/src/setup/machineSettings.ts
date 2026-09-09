import * as vscode from "vscode";

/** Workspace settings must never choose a binary launched by the extension. */
export function getMachineBinaryPath(
  setting: "lspPath" | "cvcCliPath" | "cvcMcpPath",
  outputChannel?: vscode.OutputChannel,
): string | undefined {
  const configuration = vscode.workspace.getConfiguration("volute");
  const inspected = configuration.inspect<string>(setting);
  if (inspected?.workspaceValue !== undefined || inspected?.workspaceFolderValue !== undefined) {
    outputChannel?.appendLine(`Ignoring workspace-scoped volute.${setting} binary setting.`);
    return undefined;
  }
  return inspected?.globalValue;
}

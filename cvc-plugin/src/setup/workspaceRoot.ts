import * as vscode from "vscode";
import { selectSingleFileWorkspaceRoot } from "./workspaceSelection";

export { selectSingleFileWorkspaceRoot } from "./workspaceSelection";

/**
 * CVC binds an LSP session and its policy to one worktree. A single-folder
 * workspace therefore remains the supported VS Code mode; choosing the first
 * folder in a multi-root workspace could silently bind an unrelated project.
 */
export function getActiveWorkspaceRoot(
  outputChannel: vscode.OutputChannel,
): vscode.WorkspaceFolder | undefined {
  const folders = vscode.workspace.workspaceFolders ?? [];
  const selected = selectSingleFileWorkspaceRoot(folders);
  if (selected) {
    return selected;
  }
  if (folders.length > 1) {
    outputChannel.appendLine(
      "CVC is inactive: multi-root workspaces are not supported; open one repository folder.",
    );
  }
  return undefined;
}

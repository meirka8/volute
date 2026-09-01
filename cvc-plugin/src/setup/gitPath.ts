import * as fs from "fs";
import * as path from "path";
import * as vscode from "vscode";

interface GitApiV1 { git?: { path?: string } }

/** Git comes only from VS Code's built-in trusted Git extension. */
export async function getTrustedGitPath(): Promise<string | undefined> {
  const extension = vscode.extensions.getExtension<unknown>("vscode.git");
  if (!extension) {return undefined;}
  const exports = await extension.activate() as { getAPI?: (version: 1) => GitApiV1 };
  const gitPath = exports.getAPI?.(1).git?.path;
  if (!gitPath || !path.isAbsolute(gitPath)) {return undefined;}
  try { return (await fs.promises.stat(gitPath)).isFile() ? await fs.promises.realpath(gitPath) : undefined; } catch { return undefined; }
}

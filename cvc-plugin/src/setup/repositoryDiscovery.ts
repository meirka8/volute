import { execFile as execFileCallback } from "child_process";
import * as fs from "fs";
import * as path from "path";
import { promisify } from "util";

const execFile = promisify(execFileCallback);

const GIT_TIMEOUT_MS = 3_000;
const GIT_MAX_BUFFER = 4_096;

/** Remove inherited Git repository overrides (including mixed-case Windows keys). */
export function gitDiscoveryEnvironment(source = process.env): NodeJS.ProcessEnv {
  const environment = Object.fromEntries(
    Object.entries(source).filter(([key]) => !key.toUpperCase().startsWith("GIT_")),
  );
  environment.GIT_CONFIG_NOSYSTEM = "1";
  environment.GIT_CONFIG_GLOBAL = process.platform === "win32" ? "NUL" : "/dev/null";
  return environment;
}

/** The result is deliberately not inferred from Git's human-readable errors. */
export type RepositoryInitialization =
  | "initialized"
  | "not-initialized"
  | "not-repository"
  | "invalid-storage"
  | "git-unavailable"
  | "invalid-git-output";

/** Accept exactly one bounded absolute LF-terminated Git path. */
export function parseCommonGitDir(stdout: string): string | undefined {
  if (stdout.length > GIT_MAX_BUFFER || stdout.includes("\0") || stdout.includes("\r")) {
    return undefined;
  }
  const value = stdout.endsWith("\n") ? stdout.slice(0, -1) : stdout;
  return value.length > 0 && !value.includes("\n") && path.isAbsolute(value)
    ? value
    : undefined;
}

/**
 * Locate CVC's shared database through Git rather than inspecting `.git`.
 *
 * Git worktrees represent `.git` as a gitfile, so only Git can authoritatively
 * identify the common directory. `lstat` rejects a symlinked CVC directory or
 * database rather than following it and treating it as initialized.
 */
export async function discoverRepositoryInitialization(
  workspaceRoot: string,
  gitPath: string | undefined,
): Promise<RepositoryInitialization> {
  if (!gitPath || !path.isAbsolute(gitPath)) {return "git-unavailable";}
  let root: string;
  try {
    const rootStat = await fs.promises.stat(workspaceRoot);
    if (!rootStat.isDirectory()) {
      return "not-repository";
    }
    root = await fs.promises.realpath(workspaceRoot);
  } catch {
    return "not-repository";
  }
  let commonGitDir: string;
  try {
    const { stdout } = await execFile(
      gitPath,
      ["rev-parse", "--path-format=absolute", "--git-common-dir"],
      {
        cwd: root,
        env: gitDiscoveryEnvironment(),
        windowsHide: true,
        timeout: GIT_TIMEOUT_MS,
        maxBuffer: GIT_MAX_BUFFER,
      },
    );
    const parsed = parseCommonGitDir(stdout);
    if (!parsed) {
      return "invalid-git-output";
    }
    if (!(await fs.promises.stat(parsed)).isDirectory()) {return "invalid-git-output";}
    commonGitDir = await fs.promises.realpath(parsed);
  } catch (error) {
    const code = (error as { code?: string | number }).code;
    if (code === "ENOENT") {
      return "git-unavailable";
    }
    // git rev-parse uses this non-zero status without requiring prose parsing.
    return code === 128 ? "not-repository" : "invalid-git-output";
  }

  const cvcDir = path.join(commonGitDir, "cvc");
  const dbPath = path.join(cvcDir, "index.db");
  try {
    const cvcStat = await fs.promises.lstat(cvcDir);
    if (!cvcStat.isDirectory() || cvcStat.isSymbolicLink()) {
      return "invalid-storage";
    }

    const dbStat = await fs.promises.lstat(dbPath);
    return dbStat.isFile() && !dbStat.isSymbolicLink()
      ? "initialized"
      : "invalid-storage";
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") {
      return "not-initialized";
    }
    return "invalid-storage";
  }
}

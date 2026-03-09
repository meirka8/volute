import * as path from "path";
import * as fs from "fs";

/**
 * Expand ~ and environment variables in a path string
 */
export function expandPath(inputPath: string): string {
  let expanded = inputPath;

  // Expand ~
  if (expanded.startsWith("~")) {
    const home = process.env.HOME || process.env.USERPROFILE || "";
    expanded = path.join(home, expanded.slice(1));
  }

  // Expand environment variables ($VAR or ${VAR})
  expanded = expanded.replace(/\$\{?(\w+)\}?/g, (_, varName) => {
    return process.env[varName] || "";
  });

  return expanded;
}

/**
 * Check if a file exists and is executable
 */
export async function isExecutable(filePath: string): Promise<boolean> {
  try {
    await fs.promises.access(filePath, fs.constants.X_OK);
    const stats = await fs.promises.stat(filePath);
    return stats.isFile();
  } catch {
    return false;
  }
}

/**
 * Find an executable in the system PATH
 */
export async function findInPath(
  executable: string,
): Promise<string | undefined> {
  const pathEnv = process.env.PATH || "";
  const pathSeparator = process.platform === "win32" ? ";" : ":";
  const paths = pathEnv.split(pathSeparator);

  const extensions =
    process.platform === "win32" ? ["", ".exe", ".cmd", ".bat"] : [""];

  for (const dir of paths) {
    for (const ext of extensions) {
      const fullPath = path.join(dir, executable + ext);
      if (await isExecutable(fullPath)) {
        return fullPath;
      }
    }
  }

  return undefined;
}

/**
 * Get the well-known installation directory for CVC binaries.
 * This is where install.sh / install.ps1 place binaries.
 */
export function getDefaultInstallDir(): string {
  const home = process.env.HOME || process.env.USERPROFILE || "";
  return path.join(home, ".cvc", "bin");
}

/**
 * Locate a named binary by searching:
 *   1. A user-configured path (if provided)
 *   2. The well-known install directory (~/.cvc/bin/)
 *   3. The system PATH
 */
export async function findBinary(
  binaryName: string,
  configuredPath?: string,
): Promise<string | undefined> {
  // 1. User-configured path
  if (configuredPath && configuredPath.trim() !== "") {
    const expanded = expandPath(configuredPath);
    if (await isExecutable(expanded)) {
      return expanded;
    }
  }

  // 2. Well-known install directory
  const installDir = getDefaultInstallDir();
  const suffix = process.platform === "win32" ? ".exe" : "";
  const wellKnownPath = path.join(installDir, binaryName + suffix);
  if (await isExecutable(wellKnownPath)) {
    return wellKnownPath;
  }

  // 3. System PATH
  return findInPath(binaryName);
}

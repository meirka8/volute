import { execFile as execFileCallback } from "child_process";
import * as path from "path";
import { promisify } from "util";

const execFile = promisify(execFileCallback);

/** What bounds a probe of `cvc --version`. */
export interface ProbeLimits {
  /**
   * `cvc --version` answers at once. The bound only keeps a binary that hangs
   * (or an npm launcher that is fetching a release first) from holding up the
   * command that asked.
   */
  timeoutMs: number;
  /** The report is one short line; more output than this is not a version report. */
  maxBuffer: number;
}

const DEFAULT_PROBE_LIMITS: ProbeLimits = { timeoutMs: 3_000, maxBuffer: 4_096 };

/**
 * Semantic Versioning 2.0.0. Groups: 1 major, 2 minor, 3 patch, 4 the
 * prerelease with its leading dash. Build metadata is accepted and ignored.
 */
const SEMVER =
  /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/;

/**
 * The version that `cvc --version` reported, or `undefined` when the output is
 * not a version report. The first line must be `cvc X.Y.Z`, optionally with a
 * `-prerelease`, `+build` metadata or trailing text: the shape clap prints and
 * the one the npm launcher (npm/cvc-mcp/bin/release.js) accepts.
 */
export function parseCvcVersionOutput(output: string): string | undefined {
  const line = /^cvc (\S+)(?:\s.*)?$/.exec(output.split(/\r?\n/, 1)[0]);
  return line && SEMVER.test(line[1]) ? line[1] : undefined;
}

/**
 * Whether `version` is the release `minimum` or a later one, by Semantic
 * Versioning precedence: a prerelease of the minimum itself (`0.7.1-rc.1`
 * against 0.7.1) comes before it, and build metadata does not count. Text that
 * is not a version is below every minimum, so an unreadable report is never
 * taken for a new enough one.
 */
export function isReleaseOrLater(
  version: string,
  minimum: readonly [major: number, minor: number, patch: number],
): boolean {
  const match = SEMVER.exec(version);
  if (!match) {
    return false;
  }
  const core = [Number(match[1]), Number(match[2]), Number(match[3])];
  for (let index = 0; index < core.length; index += 1) {
    if (core[index] !== minimum[index]) {
      return core[index] > minimum[index];
    }
  }
  return match[4] === undefined;
}

/**
 * Runs a program with arguments and resolves with its standard output. It
 * rejects when the program cannot start, fails, times out or says too much.
 */
export type VersionProbe = (file: string, args: readonly string[]) => Promise<string>;

/** A probe that starts the program directly (no shell) within `limits`. */
export function createExecFileProbe(limits: ProbeLimits = DEFAULT_PROBE_LIMITS): VersionProbe {
  return async (file, args) => {
    // No shell: the path and the arguments are data, never command text.
    const running = execFile(file, [...args], {
      timeout: limits.timeoutMs,
      // A child that traps SIGTERM must not outlive the timeout.
      killSignal: "SIGKILL",
      maxBuffer: limits.maxBuffer,
      windowsHide: true,
      encoding: "utf8",
    });
    // `--version` reads nothing, but a program that does read sees the end at once.
    running.child.stdin?.end();
    return (await running).stdout;
  };
}

/**
 * Ask the CLI at `cvcBinary` for its version with `cvc --version`. It resolves
 * with `undefined`, never an error, whenever the version cannot be established:
 * a path that is not absolute (the extension runs the CLI only by an absolute
 * path), a binary that cannot run, fails or hangs, or output that is not a
 * version report. Callers treat that as "unknown" and choose the conservative
 * behaviour.
 */
export async function readCvcVersion(
  cvcBinary: string,
  probe: VersionProbe = createExecFileProbe(),
): Promise<string | undefined> {
  if (!path.isAbsolute(cvcBinary)) {
    return undefined;
  }
  try {
    return parseCvcVersionOutput(await probe(cvcBinary, ["--version"]));
  } catch {
    return undefined;
  }
}

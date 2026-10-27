import * as assert from "assert";
import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import test from "node:test";
import {
  createExecFileProbe,
  isReleaseOrLater,
  parseCvcVersionOutput,
  readCvcVersion,
  VersionProbe,
} from "../setup/cvcVersion";

const POSIX_ONLY = { skip: process.platform === "win32" ? "needs a POSIX shell for the stand-in cvc" : false };

/** An absolute path on every platform, for a CLI that is never actually run. */
const ABSOLUTE_CVC = path.join(os.tmpdir(), "cvc-plugin-unit", "cvc");

function recordingProbe(outcome: string | Error): {
  probe: VersionProbe;
  calls: Array<{ file: string; args: readonly string[] }>;
} {
  const calls: Array<{ file: string; args: readonly string[] }> = [];
  const probe: VersionProbe = async (file, args) => {
    calls.push({ file, args });
    if (outcome instanceof Error) {
      throw outcome;
    }
    return outcome;
  };
  return { probe, calls };
}

/** A stand-in `cvc` that runs `body` as a POSIX shell script. */
async function standInCvc(t: test.TestContext, body: string): Promise<string> {
  const directory = await fs.promises.mkdtemp(path.join(os.tmpdir(), "cvc-plugin-version-"));
  t.after(() => fs.promises.rm(directory, { recursive: true, force: true }));
  const file = path.join(directory, "cvc");
  await fs.promises.writeFile(file, `#!/bin/sh\n${body}\n`, { mode: 0o755 });
  return file;
}

test("reads the version from the first line of `cvc --version`", () => {
  const cases: Array<[string, string]> = [
    ["cvc 0.7.1\n", "0.7.1"],
    ["cvc 0.7.1", "0.7.1"],
    ["cvc 0.7.1\r\n", "0.7.1"],
    ["cvc 10.20.30\n", "10.20.30"],
    ["cvc 0.8.0-rc.1\n", "0.8.0-rc.1"],
    ["cvc 0.7.1+build.5\n", "0.7.1+build.5"],
    ["cvc 0.7.1 (abc1234 2026-10-25)\n", "0.7.1"],
    ["cvc 0.7.1\nsecond line is ignored\n", "0.7.1"],
  ];
  for (const [output, expected] of cases) {
    assert.strictEqual(parseCvcVersionOutput(output), expected, JSON.stringify(output));
  }
});

test("does not take anything else for a version report", () => {
  const outputs = [
    "",
    "\n",
    "cvc",
    "cvc ",
    "cvc  0.7.1",
    "cvc v0.7.1",
    "cvc 0.7",
    "cvc 0.7.1.2",
    "cvc 01.7.1",
    "cvc 0.7.1-",
    "cvc 0.7.1+",
    "cvc latest",
    "cvc-lsp 0.7.1",
    "0.7.1",
    "v26.8.1",
    "git version 2.50.1",
    "Cognitive Version Control CLI\ncvc 0.7.1\n",
    "error: unrecognized option '--version'",
  ];
  for (const output of outputs) {
    assert.strictEqual(parseCvcVersionOutput(output), undefined, JSON.stringify(output));
  }
});

test("orders versions by Semantic Versioning precedence against a minimum release", () => {
  const minimum = [0, 7, 1] as const;
  const atLeast = [
    "0.7.1",
    "0.7.1+build.5",
    "0.7.2",
    "0.7.10",
    "0.8.0",
    "0.10.0",
    "1.0.0",
    "1.0.0-rc.1",
    "0.8.0-rc.1",
    "10.0.0",
  ];
  const below = [
    "0.7.0",
    "0.6.9",
    "0.0.0",
    "0.7.0+build.9",
    "0.7.0-rc.1",
    "0.7.1-rc.1",
    "0.7.1-0",
  ];
  for (const version of atLeast) {
    assert.strictEqual(isReleaseOrLater(version, minimum), true, version);
  }
  for (const version of below) {
    assert.strictEqual(isReleaseOrLater(version, minimum), false, version);
  }
});

test("compares the major and minor numbers before the patch number", () => {
  assert.strictEqual(isReleaseOrLater("1.0.0", [0, 99, 99]), true);
  assert.strictEqual(isReleaseOrLater("0.99.0", [0, 7, 1]), true);
  assert.strictEqual(isReleaseOrLater("0.6.99", [0, 7, 1]), false);
  assert.strictEqual(isReleaseOrLater("2.3.4", [2, 3, 4]), true);
  assert.strictEqual(isReleaseOrLater("2.3.3", [2, 3, 4]), false);
  assert.strictEqual(isReleaseOrLater("1.9.9", [2, 0, 0]), false);
});

test("text that is not a version is below every minimum", () => {
  for (const text of ["", "latest", "0.7", "0.7.1.0", "v0.7.1", "00.7.1", "0.7.1-", " 0.7.1", "0.7.1\n", "cvc 0.7.1"]) {
    assert.strictEqual(isReleaseOrLater(text, [0, 0, 0]), false, JSON.stringify(text));
  }
});

test("asks the CLI with --version, directly and exactly once", async () => {
  const { probe, calls } = recordingProbe("cvc 0.7.1\n");
  assert.strictEqual(await readCvcVersion(ABSOLUTE_CVC, probe), "0.7.1");
  assert.deepStrictEqual(calls, [{ file: ABSOLUTE_CVC, args: ["--version"] }]);
});

test("never runs a CLI that is not an absolute path", async () => {
  for (const relative of ["cvc", "./cvc", "bin/cvc", "../cvc", "~/.cvc/bin/cvc", ""]) {
    const { probe, calls } = recordingProbe("cvc 0.7.1\n");
    assert.strictEqual(await readCvcVersion(relative, probe), undefined, JSON.stringify(relative));
    assert.strictEqual(calls.length, 0, JSON.stringify(relative));
  }
});

test("an unknown version is undefined, not an error", async () => {
  assert.strictEqual(await readCvcVersion(ABSOLUTE_CVC, recordingProbe(new Error("spawn ENOENT")).probe), undefined);
  assert.strictEqual(await readCvcVersion(ABSOLUTE_CVC, recordingProbe("").probe), undefined);
  assert.strictEqual(await readCvcVersion(ABSOLUTE_CVC, recordingProbe("something else\n").probe), undefined);
  assert.strictEqual(await readCvcVersion(ABSOLUTE_CVC, recordingProbe("cvc not-a-version\n").probe), undefined);
});

test("a binary that does not exist has no version", async () => {
  const missing = path.join(os.tmpdir(), `cvc-plugin-no-such-cvc-${process.pid}`, "cvc");
  assert.strictEqual(await readCvcVersion(missing), undefined);
});

test("a program that reports something else has no version", async () => {
  // `node --version` prints `v<number>`, which is no cvc report.
  assert.strictEqual(await readCvcVersion(process.execPath), undefined);
});

test("reads the version a real process prints", POSIX_ONLY, async (t) => {
  const cvc = await standInCvc(t, `[ "$1" = "--version" ] && echo "cvc 0.7.1" && exit 0\nexit 3`);
  assert.strictEqual(await readCvcVersion(cvc), "0.7.1");
});

test("a real process that fails has no version, even if it printed one", POSIX_ONLY, async (t) => {
  const cvc = await standInCvc(t, `echo "cvc 0.7.1"\nexit 1`);
  assert.strictEqual(await readCvcVersion(cvc), undefined);
});

test("a real process that never answers is stopped at the time limit", POSIX_ONLY, async (t) => {
  const cvc = await standInCvc(t, "exec sleep 30");
  const started = Date.now();
  await assert.rejects(createExecFileProbe({ timeoutMs: 200, maxBuffer: 4_096 })(cvc, ["--version"]));
  assert.ok(Date.now() - started < 10_000, "the probe waited for the process instead of stopping it");
});

test("a real process that says too much is stopped at the output limit", POSIX_ONLY, async (t) => {
  const cvc = await standInCvc(t, `echo "cvc 0.7.1 followed by far more text than a version report holds"`);
  await assert.rejects(createExecFileProbe({ timeoutMs: 5_000, maxBuffer: 16 })(cvc, ["--version"]));
  assert.strictEqual(await readCvcVersion(cvc, createExecFileProbe({ timeoutMs: 5_000, maxBuffer: 16 })), undefined);
});

test("a real process reads the end of its input at once", POSIX_ONLY, async (t) => {
  // `cat` returns only when stdin ends. If the probe left stdin open, this would hang until the time limit.
  const cvc = await standInCvc(t, `cat >/dev/null\necho "cvc 0.7.1"`);
  const started = Date.now();
  assert.strictEqual(await readCvcVersion(cvc, createExecFileProbe({ timeoutMs: 5_000, maxBuffer: 4_096 })), "0.7.1");
  assert.ok(Date.now() - started < 4_000, "stdin was left open");
});

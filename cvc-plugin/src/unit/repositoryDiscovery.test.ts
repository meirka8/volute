import * as assert from "assert";
import { execFile as execFileCallback } from "child_process";
import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import test from "node:test";
import { promisify } from "util";
import {
  discoverRepositoryInitialization,
  gitDiscoveryEnvironment,
  parseCommonGitDir,
} from "../setup/repositoryDiscovery";
import { selectSingleFileWorkspaceRoot } from "../setup/workspaceSelection";
import { BindingToken } from "../setup/bindingToken";

const execFile = promisify(execFileCallback);
const fixtureGit = (() => {
  const names = process.platform === "win32" ? ["git.exe", "git.cmd"] : ["git"];
  for (const directory of (process.env.PATH ?? "").split(path.delimiter)) {
    if (!path.isAbsolute(directory)) {continue;}
    for (const name of names) {
      const candidate = path.join(directory, name);
      if (fs.existsSync(candidate)) {return candidate;}
    }
  }
  throw new Error("Git fixture is unavailable");
})();

async function git(cwd: string, args: string[]): Promise<void> {
  await execFile("git", args, {
    cwd,
    env: {
      ...process.env,
      GIT_CONFIG_NOSYSTEM: "1",
      GIT_CONFIG_GLOBAL: os.devNull,
    },
    windowsHide: true,
  });
}

async function makeRepository(t: test.TestContext): Promise<string> {
  const root = await fs.promises.mkdtemp(path.join(os.tmpdir(), "cvc-plugin-git-"));
  t.after(() => fs.promises.rm(root, { recursive: true, force: true }));
  await git(root, ["init"]);
  await fs.promises.writeFile(path.join(root, "README.md"), "fixture\n");
  await git(root, ["add", "README.md"]);
  await git(root, ["-c", "user.name=CVC test", "-c", "user.email=cvc@example.invalid", "commit", "-m", "fixture"]);
  return root;
}

async function createDatabase(repositoryRoot: string): Promise<string> {
  const dbPath = path.join(repositoryRoot, ".git", "cvc", "index.db");
  await fs.promises.mkdir(path.dirname(dbPath), { recursive: true });
  await fs.promises.writeFile(dbPath, "");
  return dbPath;
}

test("discovers an initialized main worktree root", async (t) => {
  const root = await makeRepository(t);
  await createDatabase(root);
  assert.strictEqual(await discoverRepositoryInitialization(root, fixtureGit), "initialized");
});

test("discovers an initialized repository from a nested main-worktree folder", async (t) => {
  const root = await makeRepository(t);
  await createDatabase(root);
  const nested = path.join(root, "nested", "folder");
  await fs.promises.mkdir(nested, { recursive: true });
  assert.strictEqual(await discoverRepositoryInitialization(nested, fixtureGit), "initialized");
});

test("discovers an initialized linked worktree root", async (t) => {
  const root = await makeRepository(t);
  await createDatabase(root);
  const linked = path.join(path.dirname(root), `${path.basename(root)}-linked`);
  t.after(() => fs.promises.rm(linked, { recursive: true, force: true }));
  await git(root, ["worktree", "add", "-b", "linked-test", linked]);
  assert.strictEqual(await discoverRepositoryInitialization(linked, fixtureGit), "initialized");
});

test("discovers an initialized linked worktree from a nested folder", async (t) => {
  const root = await makeRepository(t);
  await createDatabase(root);
  const linked = path.join(path.dirname(root), `${path.basename(root)}-linked-nested`);
  t.after(() => fs.promises.rm(linked, { recursive: true, force: true }));
  await git(root, ["worktree", "add", "-b", "linked-nested-test", linked]);
  const nested = path.join(linked, "nested");
  await fs.promises.mkdir(nested);
  assert.strictEqual(await discoverRepositoryInitialization(nested, fixtureGit), "initialized");
});

test("reports a Git repository without a CVC database as not initialized", async (t) => {
  const root = await makeRepository(t);
  assert.strictEqual(await discoverRepositoryInitialization(root, fixtureGit), "not-initialized");
});

test("reports malformed Git metadata without claiming initialization", async (t) => {
  const root = await fs.promises.mkdtemp(path.join(os.tmpdir(), "cvc-plugin-malformed-"));
  t.after(() => fs.promises.rm(root, { recursive: true, force: true }));
  await fs.promises.writeFile(path.join(root, ".git"), "not a gitfile\n");
  assert.strictEqual(await discoverRepositoryInitialization(root, fixtureGit), "not-repository");
});

test("reports a non-repository without claiming initialization", async (t) => {
  const root = await fs.promises.mkdtemp(path.join(os.tmpdir(), "cvc-plugin-not-repo-"));
  t.after(() => fs.promises.rm(root, { recursive: true, force: true }));
  assert.strictEqual(await discoverRepositoryInitialization(root, fixtureGit), "not-repository");
});

test("rejects a symlinked CVC database", async (t) => {
  if (process.platform === "win32") {
    t.skip("symlink permissions are not reliably available on Windows CI");
    return;
  }
  const root = await makeRepository(t);
  const dbPath = await createDatabase(root);
  const target = path.join(root, "database-target");
  await fs.promises.writeFile(target, "");
  await fs.promises.unlink(dbPath);
  await fs.promises.symlink(target, dbPath);
  assert.strictEqual(await discoverRepositoryInitialization(root, fixtureGit), "invalid-storage");
});

test("rejects a symlinked CVC directory", async (t) => {
  if (process.platform === "win32") {
    t.skip("symlink permissions are not reliably available on Windows CI");
    return;
  }
  const root = await makeRepository(t);
  const cvcDir = path.join(root, ".git", "cvc");
  const target = path.join(root, "cvc-target");
  await fs.promises.mkdir(target);
  await fs.promises.symlink(target, cvcDir);
  assert.strictEqual(await discoverRepositoryInitialization(root, fixtureGit), "invalid-storage");
});

test("removes Git overrides and unsafe PATH entries", () => {
  const environment = gitDiscoveryEnvironment({
    PATH: `relative${path.delimiter}.${path.delimiter}/usr/bin${path.delimiter}`,
    GIT_DIR: "/other/repository", git_dir: "/other/lowercase",
    GIT_WORK_TREE: "/other/worktree",
    GIT_COMMON_DIR: "/other/common",
    GIT_CEILING_DIRECTORIES: "/",
    GIT_CONFIG_COUNT: "1",
  });
  assert.strictEqual(environment.PATH, `relative${path.delimiter}.${path.delimiter}/usr/bin${path.delimiter}`);
  assert.strictEqual(Object.keys(environment).some((key) => key.startsWith("GIT_") && !["GIT_CONFIG_NOSYSTEM", "GIT_CONFIG_GLOBAL"].includes(key)), false);
});

test("does not execute when no trusted Git path is supplied", async (t) => {
  const root = await makeRepository(t);
  assert.strictEqual(await discoverRepositoryInitialization(root, undefined), "git-unavailable");
});

test("accepts only a single absolute Git output line", () => {
  assert.strictEqual(parseCommonGitDir("/repo/.git\n"), "/repo/.git");
  for (const output of ["", "relative\n", "/repo\r\n", "/repo\nextra", "/repo\0\n"]) {
    assert.strictEqual(parseCommonGitDir(output), undefined);
  }
});

test("selects only one file-backed workspace folder", () => {
  const folder = { uri: { scheme: "file" } };
  assert.strictEqual(selectSingleFileWorkspaceRoot([folder]), folder);
  assert.strictEqual(selectSingleFileWorkspaceRoot([]), undefined);
  assert.strictEqual(selectSingleFileWorkspaceRoot([folder, folder]), undefined);
  assert.strictEqual(selectSingleFileWorkspaceRoot([{ uri: { scheme: "vscode-remote" } }]), undefined);
});

test("binding token makes delayed work inert after cancellation", () => {
  const token = new BindingToken();
  assert.strictEqual(token.isActive(), true);
  token.cancel();
  assert.strictEqual(token.isActive(), false);
});

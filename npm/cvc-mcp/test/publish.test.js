'use strict';

// Tests for publishing the verified release binaries into ~/.cvc/bin.
//
// Every test builds its own temporary home directory under os.tmpdir() and
// passes it to the launcher explicitly; the real home directory is never read
// or written. The "binaries" are tiny /bin/sh scripts that answer `--version`.

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { compareSemver, getPlatformDetails, _ensurePublishedForTest: ensurePublished, _launchTargetForTest: launchTarget } = require('../bin/release');
const packageVersion = require('../package.json').version;

const UNIX_ONLY = { skip: process.platform === 'win32' };
const PACKAGE = '0.7.1';
const NAMES = ['cvc', 'cvc-mcp', 'cvc-lsp'];

function fakeScript(version, tag) {
  return `#!/bin/sh\n# ${tag}\nif [ "$1" = "--version" ]; then echo "cvc ${version}"; exit 0; fi\nexit 0\n`;
}

function writeBinaries(dir, { version, tag, suffix = '', skip = [], scripts = {} }) {
  fs.mkdirSync(dir, { recursive: true });
  for (const name of NAMES) {
    if (skip.includes(name)) continue;
    const file = path.join(dir, name + suffix);
    fs.writeFileSync(file, scripts[name] ?? fakeScript(version, `${tag}:${name}`), { mode: 0o755 });
    fs.chmodSync(file, 0o755);
  }
}

function sandbox(homeName = 'home') {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'cvc-publish-test-'));
  const home = path.join(root, homeName);
  fs.mkdirSync(home);
  return {
    root,
    home,
    stable: path.join(home, '.cvc', 'bin'),
    cache: path.join(home, '.cvc', 'mcp-cache', `v${PACKAGE}-x86_64-unknown-linux-gnu`),
    cleanup() { fs.rmSync(root, { recursive: true, force: true }); },
  };
}

async function capture(fn) {
  const lines = [];
  const original = console.error;
  console.error = (...args) => lines.push(args.join(' '));
  try {
    return { result: await fn(), lines };
  } finally {
    console.error = original;
  }
}

function publish(sb, executable = 'cvc-mcp', options = {}) {
  const suffix = options.platform === 'win32' ? '.exe' : '';
  return capture(() => ensurePublished(path.join(sb.cache, executable + suffix), executable, {
    home: sb.home,
    packageVersion: PACKAGE,
    lockWaitMs: 0, // tests that exercise waiting say so explicitly
    ...options,
  }));
}

function snapshot(dir) {
  const out = {};
  (function walk(current, relative) {
    for (const entry of fs.readdirSync(current).sort()) {
      const full = path.join(current, entry);
      const key = path.join(relative, entry);
      const stat = fs.lstatSync(full);
      if (stat.isSymbolicLink()) out[key] = `link:${fs.readlinkSync(full)}`;
      else if (stat.isDirectory()) { out[key] = `dir:${(stat.mode & 0o777).toString(8)}`; walk(full, key); }
      else out[key] = `file:${(stat.mode & 0o777).toString(8)}:${fs.readFileSync(full, 'utf8')}`;
    }
  })(dir, '');
  return out;
}

function leftovers(stable) {
  return fs.readdirSync(stable).filter(entry => entry.startsWith('.cvc-install.') || entry.startsWith('.cvc-old-'));
}

async function withPatched(method, replacement, body) {
  const original = fs.promises[method];
  fs.promises[method] = (...args) => replacement(original.bind(fs.promises), ...args);
  try {
    return await body();
  } finally {
    fs.promises[method] = original;
  }
}

function withPatchedRename(replacement, body) {
  return withPatched('rename', replacement, body);
}

test('first publish creates a private ~/.cvc/bin with three executable binaries', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    const { result, lines } = await publish(sb, 'cvc-mcp');
    assert.equal(result, path.join(sb.stable, 'cvc-mcp'));
    assert.deepEqual(lines, [`Installed CVC ${PACKAGE} to ${sb.stable}`]);
    assert.equal(fs.statSync(sb.stable).mode & 0o777, 0o700);
    assert.deepEqual(fs.readdirSync(sb.stable).sort(), ['cvc', 'cvc-lsp', 'cvc-mcp']);
    for (const name of NAMES) {
      const stat = fs.lstatSync(path.join(sb.stable, name));
      assert.equal(stat.isFile() && !stat.isSymbolicLink(), true);
      assert.equal(stat.mode & 0o777, 0o755, name);
      assert.equal(fs.readFileSync(path.join(sb.stable, name), 'utf8'), fs.readFileSync(path.join(sb.cache, name), 'utf8'));
    }
    const probe = spawnSync(path.join(sb.stable, 'cvc'), ['--version'], { encoding: 'utf8' });
    assert.equal(probe.stdout, `cvc ${PACKAGE}\n`);
  } finally {
    sb.cleanup();
  }
});

test('the requested binary decides which stable path is returned', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    const { result } = await publish(sb, 'cvc');
    assert.equal(result, path.join(sb.stable, 'cvc'));
  } finally {
    sb.cleanup();
  }
});

test('an older installation is upgraded in place', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    writeBinaries(sb.stable, { version: '0.7.0', tag: 'old' });
    const { result, lines } = await publish(sb, 'cvc-mcp');
    assert.equal(result, path.join(sb.stable, 'cvc-mcp'));
    assert.deepEqual(lines, [`Installed CVC ${PACKAGE} to ${sb.stable}`]);
    for (const name of NAMES) {
      assert.equal(fs.readFileSync(path.join(sb.stable, name), 'utf8'), fs.readFileSync(path.join(sb.cache, name), 'utf8'), name);
    }
    assert.deepEqual(leftovers(sb.stable), []);
  } finally {
    sb.cleanup();
  }
});

test('publishes cvc last so a version probe never sees the new cvc before its siblings', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    writeBinaries(sb.stable, { version: '0.7.0', tag: 'old' });
    const moved = [];
    await withPatchedRename(async (rename, from, to) => {
      if (path.dirname(to) === sb.stable) moved.push(path.basename(to));
      return rename(from, to);
    }, () => publish(sb));
    assert.deepEqual(moved, ['cvc-lsp', 'cvc-mcp', 'cvc']);
  } finally {
    sb.cleanup();
  }
});

test('a newer installation is never touched or downgraded', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    writeBinaries(sb.stable, { version: '0.8.0', tag: 'newer' });
    const before = snapshot(sb.stable);
    const { result, lines } = await publish(sb, 'cvc-mcp');
    assert.equal(result, path.join(sb.stable, 'cvc-mcp'));
    assert.deepEqual(lines, []);
    assert.deepEqual(snapshot(sb.stable), before);
  } finally {
    sb.cleanup();
  }
});

test('a newer installation with an unusable requested binary falls back to the cache without touching it', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    writeBinaries(sb.stable, { version: '0.8.0', tag: 'newer', skip: ['cvc-mcp'] });
    const before = snapshot(sb.stable);
    const { result, lines } = await publish(sb, 'cvc-mcp');
    assert.equal(result, path.join(sb.cache, 'cvc-mcp'));
    assert.deepEqual(lines, []);
    assert.deepEqual(snapshot(sb.stable), before);
  } finally {
    sb.cleanup();
  }
});

test('a prerelease installation of a higher core version is newer than the package', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    writeBinaries(sb.stable, { version: '0.8.0-rc.1', tag: 'newer-pre' });
    const before = snapshot(sb.stable);
    const { result, lines } = await publish(sb);
    assert.equal(result, path.join(sb.stable, 'cvc-mcp'));
    assert.deepEqual(lines, []);
    assert.deepEqual(snapshot(sb.stable), before);
  } finally {
    sb.cleanup();
  }
});

test('the package version is a floor: a prerelease of the same version is replaced', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    writeBinaries(sb.stable, { version: `${PACKAGE}-rc.1`, tag: 'prerelease' });
    const { result, lines } = await publish(sb);
    assert.equal(result, path.join(sb.stable, 'cvc-mcp'));
    assert.deepEqual(lines, [`Installed CVC ${PACKAGE} to ${sb.stable}`]);
    assert.equal(fs.readFileSync(path.join(sb.stable, 'cvc'), 'utf8'), fs.readFileSync(path.join(sb.cache, 'cvc'), 'utf8'));
  } finally {
    sb.cleanup();
  }
});

test('an equal, complete installation with different bytes is left alone', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    writeBinaries(sb.stable, { version: PACKAGE, tag: 'local-build' });
    const before = snapshot(sb.stable);
    const { result, lines } = await publish(sb, 'cvc-lsp');
    assert.equal(result, path.join(sb.stable, 'cvc-lsp'));
    assert.deepEqual(lines, []);
    assert.deepEqual(snapshot(sb.stable), before);
  } finally {
    sb.cleanup();
  }
});

test('an equal installation with a missing sibling is completed', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    writeBinaries(sb.stable, { version: PACKAGE, tag: 'local-build', skip: ['cvc-lsp'] });
    const { result, lines } = await publish(sb);
    assert.equal(result, path.join(sb.stable, 'cvc-mcp'));
    assert.deepEqual(lines, [`Installed CVC ${PACKAGE} to ${sb.stable}`]);
    assert.equal(fs.readFileSync(path.join(sb.stable, 'cvc-lsp'), 'utf8'), fs.readFileSync(path.join(sb.cache, 'cvc-lsp'), 'utf8'));
    assert.deepEqual(leftovers(sb.stable), []);
  } finally {
    sb.cleanup();
  }
});

test('an installation with only cvc-mcp and cvc-lsp (cvc missing) is published', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    writeBinaries(sb.stable, { version: '0.9.0', tag: 'orphans', skip: ['cvc'] });
    const { result } = await publish(sb);
    assert.equal(result, path.join(sb.stable, 'cvc-mcp'));
    assert.equal(spawnSync(path.join(sb.stable, 'cvc'), ['--version'], { encoding: 'utf8' }).stdout, `cvc ${PACKAGE}\n`);
  } finally {
    sb.cleanup();
  }
});

test('a lock held by someone else blocks publishing, is named, and is never removed', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    writeBinaries(sb.stable, { version: '0.7.0', tag: 'old' });
    const lock = path.join(sb.stable, '.cvc-install.lock');
    fs.mkdirSync(lock);
    const before = snapshot(sb.stable);
    const { result, lines } = await publish(sb, 'cvc-mcp');
    assert.equal(result, path.join(sb.cache, 'cvc-mcp'));
    assert.equal(lines.length, 1);
    assert.equal(lines[0], `CVC: could not install into ${sb.stable}: another CVC installation holds ${lock}. Running the verified cached copy this time; the next launch tries again.`);
    assert.ok(!/stale/.test(lines[0]), lines[0]);
    assert.deepEqual(snapshot(sb.stable), before);
    assert.equal(fs.statSync(lock).isDirectory(), true);
  } finally {
    sb.cleanup();
  }
});

test('a lock older than ten minutes is reported as possibly stale but is not removed', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    writeBinaries(sb.stable, { version: '0.7.0', tag: 'old' });
    const lock = path.join(sb.stable, '.cvc-install.lock');
    fs.mkdirSync(lock);
    const past = new Date(Date.now() - 60 * 60 * 1000);
    fs.utimesSync(lock, past, past);
    const { result, lines } = await publish(sb);
    assert.equal(result, path.join(sb.cache, 'cvc-mcp'));
    assert.equal(lines.length, 1);
    assert.ok(lines[0].includes(lock), lines[0]);
    assert.ok(/may be stale; if no installation is running, remove it with: rm -r/.test(lines[0]), lines[0]);
    assert.equal(fs.statSync(lock).isDirectory(), true);
  } finally {
    sb.cleanup();
  }
});

test('a lock is not needed (or taken) when nothing has to be published', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    writeBinaries(sb.stable, { version: '0.8.0', tag: 'newer' });
    fs.mkdirSync(path.join(sb.stable, '.cvc-install.lock'));
    const { result, lines } = await publish(sb);
    assert.equal(result, path.join(sb.stable, 'cvc-mcp'));
    assert.deepEqual(lines, []);
  } finally {
    sb.cleanup();
  }
});

test('a symlinked ~/.cvc/bin is refused and its target is untouched', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    const elsewhere = path.join(sb.root, 'elsewhere');
    writeBinaries(elsewhere, { version: '0.7.0', tag: 'elsewhere' });
    fs.mkdirSync(path.dirname(sb.stable), { recursive: true });
    fs.symlinkSync(elsewhere, sb.stable);
    const before = snapshot(elsewhere);
    const { result, lines } = await publish(sb);
    assert.equal(result, path.join(sb.cache, 'cvc-mcp'));
    assert.deepEqual(lines, [`CVC: could not install into ${sb.stable}: ${sb.stable} is not a real directory (symlinks and files are refused). Running the verified cached copy this time; the next launch tries again.`]);
    assert.deepEqual(snapshot(elsewhere), before);
    assert.equal(fs.lstatSync(sb.stable).isSymbolicLink(), true);
  } finally {
    sb.cleanup();
  }
});

test('a regular file in place of ~/.cvc/bin is refused', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    fs.writeFileSync(sb.stable, 'not a directory');
    const { result, lines } = await publish(sb);
    assert.equal(result, path.join(sb.cache, 'cvc-mcp'));
    assert.equal(lines.length, 1);
    assert.equal(fs.readFileSync(sb.stable, 'utf8'), 'not a directory');
  } finally {
    sb.cleanup();
  }
});

test('an existing symlink target is refused and nothing is replaced', UNIX_ONLY, async () => {
  for (const linked of NAMES) {
    const sb = sandbox();
    try {
      writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
      writeBinaries(sb.stable, { version: '0.7.0', tag: 'old', skip: [linked] });
      const payload = path.join(sb.root, 'payload');
      fs.writeFileSync(payload, 'payload', { mode: 0o755 });
      fs.symlinkSync(payload, path.join(sb.stable, linked));
      const before = snapshot(sb.stable);
      const { result, lines } = await publish(sb, 'cvc-mcp');
      assert.equal(result, path.join(sb.cache, 'cvc-mcp'), linked);
      assert.equal(lines.length, 1, linked);
      assert.ok(lines[0].includes(path.join(sb.stable, linked)), lines[0]);
      assert.deepEqual(snapshot(sb.stable), before, linked);
      assert.equal(fs.readFileSync(payload, 'utf8'), 'payload');
    } finally {
      sb.cleanup();
    }
  }
});

test('an existing directory where a binary should be is refused', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    writeBinaries(sb.stable, { version: '0.7.0', tag: 'old', skip: ['cvc-lsp'] });
    fs.mkdirSync(path.join(sb.stable, 'cvc-lsp'));
    const before = snapshot(sb.stable);
    const { result, lines } = await publish(sb);
    assert.equal(result, path.join(sb.cache, 'cvc-mcp'));
    assert.equal(lines.length, 1);
    assert.deepEqual(snapshot(sb.stable), before);
  } finally {
    sb.cleanup();
  }
});

test('a cache copy that reports the wrong version is never published', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    // The cache claims a different version than the package, so the staged check fails.
    writeBinaries(sb.cache, { version: '0.6.0', tag: 'wrong-cache' });
    const { result, lines } = await publish(sb);
    assert.equal(result, path.join(sb.cache, 'cvc-mcp'));
    assert.deepEqual(lines, [`CVC: could not install into ${sb.stable}: the staged cvc reports 0.6.0 instead of ${PACKAGE}. ${STABLE_LINE_SUFFIX}`]);
    assert.deepEqual(fs.readdirSync(sb.stable), [], 'nothing was published, staged or locked');
  } finally {
    sb.cleanup();
  }
});

test('a missing cached binary falls back without creating staging or lock leftovers', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache', skip: ['cvc-lsp'] });
    writeBinaries(sb.stable, { version: '0.7.0', tag: 'old' });
    const before = snapshot(sb.stable);
    const { result, lines } = await publish(sb);
    assert.equal(result, path.join(sb.cache, 'cvc-mcp'));
    assert.equal(lines.length, 1);
    assert.deepEqual(snapshot(sb.stable), before);
  } finally {
    sb.cleanup();
  }
});

test('a failure part-way through the moves falls back, cleans up, and the next launch finishes the job', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    writeBinaries(sb.stable, { version: '0.7.0', tag: 'old' });
    const oldCvc = fs.readFileSync(path.join(sb.stable, 'cvc'), 'utf8');
    const failed = await withPatchedRename(async (rename, from, to) => {
      if (to === path.join(sb.stable, 'cvc')) throw Object.assign(new Error('injected rename failure'), { code: 'EIO' });
      return rename(from, to);
    }, () => publish(sb));
    assert.equal(failed.result, path.join(sb.cache, 'cvc-mcp'));
    assert.equal(failed.lines.length, 1);
    assert.ok(failed.lines[0].includes('injected rename failure'), failed.lines[0]);
    assert.deepEqual(leftovers(sb.stable), []);
    assert.equal(fs.existsSync(path.join(sb.stable, '.cvc-install.lock')), false);
    // cvc is published last, so it still reports the old version and the next launch republishes.
    assert.equal(fs.readFileSync(path.join(sb.stable, 'cvc'), 'utf8'), oldCvc);
    const retry = await publish(sb);
    assert.equal(retry.result, path.join(sb.stable, 'cvc-mcp'));
    assert.deepEqual(retry.lines, [`Installed CVC ${PACKAGE} to ${sb.stable}`]);
    assert.deepEqual(leftovers(sb.stable), []);
  } finally {
    sb.cleanup();
  }
});

test('an unwritable stable directory falls back and leaves no lock behind', { skip: process.platform === 'win32' || (process.getuid && process.getuid() === 0) }, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    writeBinaries(sb.stable, { version: '0.7.0', tag: 'old' });
    fs.chmodSync(sb.stable, 0o500);
    const { result, lines } = await publish(sb);
    assert.equal(result, path.join(sb.cache, 'cvc-mcp'));
    assert.equal(lines.length, 1);
    fs.chmodSync(sb.stable, 0o700);
    assert.deepEqual(fs.readdirSync(sb.stable).sort(), ['cvc', 'cvc-lsp', 'cvc-mcp']);
  } finally {
    try { fs.chmodSync(sb.stable, 0o700); } catch (_) { /* may not exist */ }
    sb.cleanup();
  }
});

test('no staging directory or lock remains after a successful publish', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    await publish(sb);
    assert.deepEqual(fs.readdirSync(sb.stable).sort(), ['cvc', 'cvc-lsp', 'cvc-mcp']);
  } finally {
    sb.cleanup();
  }
});

test('concurrent launchers publish once and every one ends up with a usable stable binary', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    // Console capture is global, so capture once around all the launchers.
    const { result: results, lines } = await capture(() => Promise.all([1, 2, 3, 4].map(() => (
      ensurePublished(path.join(sb.cache, 'cvc-mcp'), 'cvc-mcp', { home: sb.home, packageVersion: PACKAGE })
    ))));
    assert.deepEqual(results, Array(4).fill(path.join(sb.stable, 'cvc-mcp')));
    assert.deepEqual(lines, [`Installed CVC ${PACKAGE} to ${sb.stable}`]);
    assert.deepEqual(fs.readdirSync(sb.stable).sort(), ['cvc', 'cvc-lsp', 'cvc-mcp']);
    assert.equal(spawnSync(path.join(sb.stable, 'cvc'), ['--version'], { encoding: 'utf8' }).stdout, `cvc ${PACKAGE}\n`);
  } finally {
    sb.cleanup();
  }
});

const CHILD_SCRIPT = [
  "const [, releasePath, cachePath, home, resultFile] = process.argv;",
  "require(releasePath)._ensurePublishedForTest(cachePath, 'cvc-mcp', { home, packageVersion: '" + PACKAGE + "' })",
  "  .then(target => require('fs').writeFileSync(resultFile, target));",
].join('\n');

function runInChild(sb) {
  const resultFile = path.join(sb.root, 'result.txt');
  const child = spawnSync(process.execPath, ['-e', CHILD_SCRIPT, path.resolve(__dirname, '..', 'bin', 'release.js'), path.join(sb.cache, 'cvc-mcp'), sb.home, resultFile], { encoding: 'utf8' });
  return { child, result: fs.existsSync(resultFile) ? fs.readFileSync(resultFile, 'utf8') : undefined };
}

test('the publish path never writes to stdout (published and fallback outcomes)', UNIX_ONLY, () => {
  const published = sandbox();
  try {
    writeBinaries(published.cache, { version: PACKAGE, tag: 'cache' });
    const { child, result } = runInChild(published);
    assert.equal(child.status, 0, child.stderr);
    assert.equal(child.stdout, '');
    assert.equal(child.stderr, `Installed CVC ${PACKAGE} to ${published.stable}\n`);
    assert.equal(result, path.join(published.stable, 'cvc-mcp'));
  } finally {
    published.cleanup();
  }

  const refused = sandbox();
  try {
    writeBinaries(refused.cache, { version: PACKAGE, tag: 'cache' });
    writeBinaries(refused.stable, { version: '0.7.0', tag: 'old' });
    fs.mkdirSync(path.join(refused.stable, '.cvc-install.lock'));
    const { child, result } = runInChild(refused);
    assert.equal(child.status, 0, child.stderr);
    assert.equal(child.stdout, '');
    assert.equal(child.stderr.trim().split('\n').length, 1);
    assert.equal(result, path.join(refused.cache, 'cvc-mcp'));
  } finally {
    refused.cleanup();
  }

  const untouched = sandbox();
  try {
    writeBinaries(untouched.cache, { version: PACKAGE, tag: 'cache' });
    writeBinaries(untouched.stable, { version: '0.8.0', tag: 'newer' });
    const { child } = runInChild(untouched);
    assert.equal(child.status, 0, child.stderr);
    assert.equal(child.stdout, '');
    assert.equal(child.stderr, '');
  } finally {
    untouched.cleanup();
  }
});

function supportedPlatform() {
  try { getPlatformDetails(); return true; } catch (_) { return false; }
}

// End to end through the real shim, entirely inside a temporary HOME. A valid
// pre-populated cache means installRelease() returns without any network access.
test('the cvc shim publishes the cache into ~/.cvc/bin, runs it from there, and mirrors its exit code', { skip: process.platform === 'win32' || !supportedPlatform() }, () => {
  const sb = sandbox();
  try {
    const details = getPlatformDetails();
    const cache = path.join(sb.home, '.cvc', 'mcp-cache', `v${packageVersion}-${details.archTag}-${details.osTag}`);
    const script = `#!/bin/sh\nif [ "$1" = "--version" ]; then echo "cvc ${packageVersion}"; exit 0; fi\necho "ran:$0 args:$*"\nexit 7\n`;
    writeBinaries(cache, { version: packageVersion, tag: 'cache', scripts: { cvc: script } });
    // Any attempted download fails immediately (connection refused) instead of reaching the network.
    const env = { ...process.env, HOME: sb.home, USERPROFILE: sb.home, CVC_RELEASE_BASE_URL: 'https://127.0.0.1:9' };
    // Never proceed if the child would resolve the real home directory.
    const resolved = spawnSync(process.execPath, ['-e', "process.stdout.write(require('os').homedir())"], { env, encoding: 'utf8' });
    assert.equal(resolved.stdout, sb.home);

    const shim = path.resolve(__dirname, '..', 'bin', 'cvc.js');
    const first = spawnSync(process.execPath, [shim, 'hello', 'world'], { env, encoding: 'utf8' });
    const stableCvc = path.join(sb.stable, 'cvc');
    assert.equal(first.status, 7, first.stderr);
    assert.equal(first.stdout, `ran:${stableCvc} args:hello world\n`);
    assert.equal(first.stderr, `Installed CVC ${packageVersion} to ${sb.stable}\n`);
    assert.deepEqual(fs.readdirSync(sb.stable).sort(), ['cvc', 'cvc-lsp', 'cvc-mcp']);

    // The stable copy is already current: no cache and no download are needed, so it works offline.
    fs.rmSync(path.join(sb.home, '.cvc', 'mcp-cache'), { recursive: true, force: true });
    const second = spawnSync(process.execPath, [shim, 'again'], { env, encoding: 'utf8' });
    assert.equal(second.status, 7, second.stderr);
    assert.equal(second.stdout, `ran:${stableCvc} args:again\n`);
    assert.equal(second.stderr, '');
    assert.equal(fs.existsSync(path.join(sb.home, '.cvc', 'mcp-cache')), false);
  } finally {
    sb.cleanup();
  }
});

// The Windows branch (.exe names, file lock, rename-aside) is exercised on Unix
// by passing platform: 'win32' with shell-script stand-ins named *.exe.

test('simulated Windows: first publish uses .exe names and leaves no lock', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache', suffix: '.exe' });
    const outcome = await publish(sb, 'cvc-mcp', { platform: 'win32' });
    assert.equal(outcome.result, path.join(sb.stable, 'cvc-mcp.exe'));
    assert.deepEqual(outcome.lines, [`Installed CVC ${PACKAGE} to ${sb.stable}`]);
    assert.deepEqual(fs.readdirSync(sb.stable).sort(), ['cvc-lsp.exe', 'cvc-mcp.exe', 'cvc.exe']);
  } finally {
    sb.cleanup();
  }
});

test('simulated Windows: upgrade renames old images aside, removes them, and clears earlier leftovers', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache', suffix: '.exe' });
    writeBinaries(sb.stable, { version: '0.7.0', tag: 'old', suffix: '.exe' });
    const stale = path.join(sb.stable, '.cvc-old-cvc.exe-0123456789abcdef');
    fs.writeFileSync(stale, 'left over from an earlier publish');
    const unrelated = path.join(sb.stable, '.cvc-old-notes.txt');
    fs.writeFileSync(unrelated, 'keep me');
    const asides = [];
    const outcome = await withPatchedRename(async (rename, from, to) => {
      if (path.basename(to).startsWith('.cvc-old-')) asides.push(path.basename(from));
      return rename(from, to);
    }, () => publish(sb, 'cvc', { platform: 'win32' }));
    assert.equal(outcome.result, path.join(sb.stable, 'cvc.exe'));
    assert.deepEqual(outcome.lines, [`Installed CVC ${PACKAGE} to ${sb.stable}`]);
    assert.deepEqual(asides, ['cvc-lsp.exe', 'cvc-mcp.exe', 'cvc.exe']);
    assert.equal(fs.existsSync(stale), false);
    assert.equal(fs.readFileSync(unrelated, 'utf8'), 'keep me');
    assert.deepEqual(leftovers(sb.stable), ['.cvc-old-notes.txt']);
    for (const name of NAMES) {
      assert.equal(fs.readFileSync(path.join(sb.stable, `${name}.exe`), 'utf8'), fs.readFileSync(path.join(sb.cache, `${name}.exe`), 'utf8'));
    }
  } finally {
    sb.cleanup();
  }
});

test('simulated Windows: a failed move restores the retired image', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache', suffix: '.exe' });
    writeBinaries(sb.stable, { version: '0.7.0', tag: 'old', suffix: '.exe' });
    const oldCvc = fs.readFileSync(path.join(sb.stable, 'cvc.exe'), 'utf8');
    const outcome = await withPatchedRename(async (rename, from, to) => {
      if (to === path.join(sb.stable, 'cvc.exe') && path.basename(path.dirname(from)).startsWith('.cvc-install.')) {
        throw Object.assign(new Error('injected sharing violation'), { code: 'EBUSY' });
      }
      return rename(from, to);
    }, () => publish(sb, 'cvc', { platform: 'win32' }));
    assert.equal(outcome.result, path.join(sb.cache, 'cvc.exe'));
    assert.equal(outcome.lines.length, 1);
    assert.ok(outcome.lines[0].includes('injected sharing violation'), outcome.lines[0]);
    assert.equal(fs.readFileSync(path.join(sb.stable, 'cvc.exe'), 'utf8'), oldCvc);
    assert.deepEqual(leftovers(sb.stable), []);
    assert.equal(fs.existsSync(path.join(sb.stable, '.cvc-install.lock')), false);
  } finally {
    sb.cleanup();
  }
});

test('simulated Windows: a held lock blocks publishing and is never removed', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache', suffix: '.exe' });
    writeBinaries(sb.stable, { version: '0.7.0', tag: 'old', suffix: '.exe' });
    const lock = plantLock(sb, null);
    const before = snapshot(sb.stable);
    const outcome = await publish(sb, 'cvc-mcp', { platform: 'win32' });
    assert.equal(outcome.result, path.join(sb.cache, 'cvc-mcp.exe'));
    assert.equal(outcome.lines.length, 1);
    assert.ok(outcome.lines[0].includes(lock), outcome.lines[0]);
    assert.deepEqual(snapshot(sb.stable), before);
  } finally {
    sb.cleanup();
  }
});

test('rejects an invalid injected package version without throwing', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    const { result, lines } = await publish(sb, 'cvc-mcp', { packageVersion: 'not-a-version' });
    assert.equal(result, path.join(sb.cache, 'cvc-mcp'));
    assert.equal(lines.length, 1);
    assert.equal(fs.existsSync(sb.stable), false);
  } finally {
    sb.cleanup();
  }
});

test('compareSemver follows SemVer 2.0.0 precedence', () => {
  // The ordering example from the SemVer 2.0.0 specification, plus release cases.
  const ascending = [
    '0.7.0-alpha', '0.7.0-alpha.1', '0.7.0-alpha.beta', '0.7.0-beta', '0.7.0-beta.2',
    '0.7.0-beta.11', '0.7.0-rc.1', '0.7.0', '0.7.1', '0.9.0', '0.10.0', '1.0.0', '1.0.1', '2.0.0',
  ];
  for (let i = 0; i < ascending.length; i++) {
    for (let j = 0; j < ascending.length; j++) {
      const expected = i === j ? 0 : (i < j ? -1 : 1);
      assert.equal(compareSemver(ascending[i], ascending[j]), expected, `${ascending[i]} vs ${ascending[j]}`);
    }
  }

  const cases = [
    ['1.2.3', '1.2.3', 0],
    ['1.2.3', '1.2.4', -1],
    ['1.3.0', '1.2.9', 1],
    ['0.9.0', '0.10.0', -1], // numeric, not lexical
    ['1.0.0-1', '1.0.0-a', -1], // numeric identifiers sort below alphanumeric
    ['1.0.0-2', '1.0.0-10', -1], // numeric identifiers compare numerically
    ['1.0.0-alpha', '1.0.0-alpha.1', -1], // shorter list is lower when all else is equal
    ['1.0.0-alpha.1', '1.0.0-alpha.1', 0],
    ['1.0.0-alpha-1', '1.0.0-alpha-2', -1], // hyphen belongs to the identifier
    ['1.0.0-alpha', '1.0.0-alpha-1', -1],
    ['1.0.0-Beta', '1.0.0-alpha', -1], // ASCII order: uppercase sorts before lowercase
    ['1.0.0-rc.1', '1.0.0', -1], // a prerelease is lower than the release
    ['1.0.1-rc.1', '1.0.0', 1], // ...but only for the same core version
    ['01.0.0', '1.0.0', 0], // leading zeros do not change the numeric value
    ['9007199254740993.0.0', '9007199254740992.0.0', 1], // beyond Number precision
  ];
  for (const [a, b, expected] of cases) {
    assert.equal(compareSemver(a, b), expected, `${a} vs ${b}`);
    assert.equal(compareSemver(b, a), expected === 0 ? 0 : -expected, `${b} vs ${a}`);
  }

  for (const invalid of ['v1.0.0', '1.0', '1.0.0+build', '1.0.0-', '', 'latest', '1.0.0-a..b']) {
    assert.throws(() => compareSemver(invalid, '1.0.0'), TypeError, invalid);
    assert.throws(() => compareSemver('1.0.0', invalid), TypeError, invalid);
  }
});

// ---------------------------------------------------------------------------
// Stable directory is checked before anything is downloaded
// ---------------------------------------------------------------------------

const DETAILS = { platform: 'linux', osTag: 'unknown-linux-gnu', archTag: 'x86_64' };
const LOCK_NAME = '.cvc-install.lock';

// A stand-in for installRelease(): records the call and lays out a valid cache.
// The real installRelease is never reachable from these tests.
function fakeInstall(sb, calls, { version = PACKAGE, fail } = {}) {
  return async (details, executable) => {
    calls.push(executable);
    if (fail) throw new Error(fail);
    writeBinaries(sb.cache, { version, tag: 'cache' });
    return path.join(sb.cache, executable);
  };
}

function resolveLaunch(sb, executable, calls, extra = {}) {
  return capture(() => launchTarget(executable, {
    home: sb.home,
    packageVersion: PACKAGE,
    details: DETAILS,
    installRelease: fakeInstall(sb, calls),
    lockWaitMs: 0,
    ...extra,
  }));
}

test('launch: a newer installation runs without downloading or touching the cache', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.stable, { version: '0.8.0', tag: 'newer' });
    const before = snapshot(sb.stable);
    const calls = [];
    const { result, lines } = await resolveLaunch(sb, 'cvc-mcp', calls);
    assert.equal(result, path.join(sb.stable, 'cvc-mcp'));
    assert.deepEqual(calls, []);
    assert.deepEqual(lines, []);
    assert.deepEqual(snapshot(sb.stable), before);
    assert.equal(fs.existsSync(path.dirname(sb.cache)), false);
  } finally {
    sb.cleanup();
  }
});

test('launch: an equal, complete installation runs without downloading', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.stable, { version: PACKAGE, tag: 'local-build' });
    const before = snapshot(sb.stable);
    const calls = [];
    const { result, lines } = await resolveLaunch(sb, 'cvc', calls);
    assert.equal(result, path.join(sb.stable, 'cvc'));
    assert.deepEqual(calls, []);
    assert.deepEqual(lines, []);
    assert.deepEqual(snapshot(sb.stable), before);
  } finally {
    sb.cleanup();
  }
});

test('launch: a newer installation missing the requested binary needs the cache', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.stable, { version: '0.8.0', tag: 'newer', skip: ['cvc-mcp'] });
    const before = snapshot(sb.stable);
    const calls = [];
    const { result, lines } = await resolveLaunch(sb, 'cvc-mcp', calls);
    assert.deepEqual(calls, ['cvc-mcp']);
    assert.equal(result, path.join(sb.cache, 'cvc-mcp'));
    assert.deepEqual(lines, []);
    assert.deepEqual(snapshot(sb.stable), before);
  } finally {
    sb.cleanup();
  }
});

test('launch: a lower installation downloads and is upgraded', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.stable, { version: '0.7.0', tag: 'old' });
    const calls = [];
    const { result, lines } = await resolveLaunch(sb, 'cvc-mcp', calls);
    assert.deepEqual(calls, ['cvc-mcp']);
    assert.equal(result, path.join(sb.stable, 'cvc-mcp'));
    assert.deepEqual(lines, [`Installed CVC ${PACKAGE} to ${sb.stable}`]);
    assert.equal(fs.readFileSync(path.join(sb.stable, 'cvc'), 'utf8'), fs.readFileSync(path.join(sb.cache, 'cvc'), 'utf8'));
  } finally {
    sb.cleanup();
  }
});

test('launch: an absent or incomplete stable directory downloads and publishes', UNIX_ONLY, async () => {
  for (const prepare of [() => {}, sb => writeBinaries(sb.stable, { version: PACKAGE, tag: 'local-build', skip: ['cvc-lsp'] })]) {
    const sb = sandbox();
    try {
      prepare(sb);
      const calls = [];
      const { result, lines } = await resolveLaunch(sb, 'cvc', calls);
      assert.deepEqual(calls, ['cvc']);
      assert.equal(result, path.join(sb.stable, 'cvc'));
      assert.deepEqual(lines, [`Installed CVC ${PACKAGE} to ${sb.stable}`]);
    } finally {
      sb.cleanup();
    }
  }
});

test('launch: an unsafe stable directory still downloads and falls back with the usual line', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    const elsewhere = path.join(sb.root, 'elsewhere');
    writeBinaries(elsewhere, { version: '0.9.0', tag: 'elsewhere' });
    fs.mkdirSync(path.dirname(sb.stable), { recursive: true });
    fs.symlinkSync(elsewhere, sb.stable);
    const before = snapshot(elsewhere);
    const calls = [];
    const { result, lines } = await resolveLaunch(sb, 'cvc-mcp', calls);
    assert.deepEqual(calls, ['cvc-mcp']);
    assert.equal(result, path.join(sb.cache, 'cvc-mcp'));
    assert.deepEqual(lines, [`CVC: could not install into ${sb.stable}: ${sb.stable} is not a real directory (symlinks and files are refused). Running the verified cached copy this time; the next launch tries again.`]);
    assert.deepEqual(snapshot(elsewhere), before);
  } finally {
    sb.cleanup();
  }
});

test('launch: a failed download is still reported as before, not swallowed', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.stable, { version: '0.7.0', tag: 'old' });
    const calls = [];
    await assert.rejects(resolveLaunch(sb, 'cvc', calls, { installRelease: fakeInstall(sb, calls, { fail: 'Download failed (offline)' }) }), /Download failed \(offline\)/);
    assert.deepEqual(calls, ['cvc']);
  } finally {
    sb.cleanup();
  }
});

test('launch: refuses an unknown executable before doing anything', async () => {
  const calls = [];
  await assert.rejects(launchTarget('cvc-evil', { installRelease: async () => { calls.push('called'); } }), /unknown CVC executable/);
  assert.deepEqual(calls, []);
});

// ---------------------------------------------------------------------------
// Recovering a lock left behind by a launcher that was killed mid-publish
// ---------------------------------------------------------------------------

function deadPid() {
  return spawnSync(process.execPath, ['-e', '']).pid;
}

// What this host would write into a lock record: the real boot id and PID namespace on
// Linux, null elsewhere. Planted records carry them too, as a launcher's own would;
// tests that need them absent or different say so explicitly.
const REAL_BOOT = process.platform === 'linux'
  ? (() => { try { return fs.readFileSync('/proc/sys/kernel/random/boot_id', 'utf8').trim() || null; } catch (_) { return null; } })()
  : null;
const REAL_PIDNS = process.platform === 'linux'
  ? (() => { try { return fs.readlinkSync('/proc/self/ns/pid') || null; } catch (_) { return null; } })()
  : null;

function ownerJson(overrides = {}) {
  return JSON.stringify({ tool: 'cvc-npm-launcher', pid: process.pid, host: os.hostname(), created: new Date().toISOString(), pidns: REAL_PIDNS, boot: REAL_BOOT, ...overrides });
}

const OWNER_ENTRY = /^owner-[0-9a-f]{16}$/;

// Plants a lock the way the launcher lays it out on every platform: a directory
// holding one `owner-<nonce>` file. `content` null means an installer-style
// lock: an empty directory with no record.
function plantLock(sb, content, entry = 'owner-0123456789abcdef') {
  const lock = path.join(sb.stable, LOCK_NAME);
  fs.mkdirSync(sb.stable, { recursive: true });
  fs.mkdirSync(lock);
  if (content !== null) fs.writeFileSync(path.join(lock, entry), content);
  // Planted locks look a minute old: a lock this launcher has only just created gets a
  // short grace period for its record to appear, which the real-launcher tests cover.
  const minuteAgo = new Date(Date.now() - 60 * 1000);
  fs.utimesSync(lock, minuteAgo, minuteAgo);
  return lock;
}

function readOwner(lock) {
  const entries = fs.readdirSync(lock);
  assert.equal(entries.length, 1, entries.join(','));
  assert.match(entries[0], OWNER_ENTRY);
  return { entry: entries[0], record: JSON.parse(fs.readFileSync(path.join(lock, entries[0]), 'utf8')) };
}

test('a lock whose launcher process is dead is broken and the publish proceeds', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    writeBinaries(sb.stable, { version: '0.7.0', tag: 'old' });
    const lock = plantLock(sb, ownerJson({ pid: deadPid() }));
    const { result, lines } = await publish(sb);
    assert.equal(result, path.join(sb.stable, 'cvc-mcp'));
    assert.deepEqual(lines, [`Installed CVC ${PACKAGE} to ${sb.stable}`]);
    assert.equal(fs.existsSync(lock), false);
    assert.deepEqual(leftovers(sb.stable), []);
    assert.equal(fs.readFileSync(path.join(sb.stable, 'cvc'), 'utf8'), fs.readFileSync(path.join(sb.cache, 'cvc'), 'utf8'));
  } finally {
    sb.cleanup();
  }
});

test('locks that are not provably a dead launcher\'s are never broken', UNIX_ONLY, async () => {
  const oversized = ownerJson({ pid: deadPid(), padding: 'x'.repeat(8192) });
  const cases = {
    'live launcher (this process)': () => ownerJson({ pid: process.pid }),
    'live launcher on another host': () => ownerJson({ pid: process.pid, host: 'some-other-host.invalid' }),
    'dead pid but another host': () => ownerJson({ pid: deadPid(), host: 'some-other-host.invalid' }),
    'dead pid but another tool': () => ownerJson({ pid: deadPid(), tool: 'install.sh' }),
    'installer-style lock without any record': () => null,
    'garbage owner file': () => 'this is not json',
    'empty owner file': () => '',
    'oversized owner file': () => oversized,
    'owner that is a JSON array': () => '[1,2,3]',
    'zero pid': () => ownerJson({ pid: 0 }),
    'negative pid': () => ownerJson({ pid: -1 }),
    'fractional pid': () => ownerJson({ pid: 1.5 }),
    'string pid': () => ownerJson({ pid: String(deadPid()) }),
    'missing pid': () => ownerJson({ pid: undefined }),
  };
  for (const [label, content] of Object.entries(cases)) {
    const sb = sandbox();
    try {
      writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
      writeBinaries(sb.stable, { version: '0.7.0', tag: 'old' });
      const lock = plantLock(sb, content());
      const before = snapshot(sb.stable);
      const { result, lines } = await publish(sb);
      assert.equal(result, path.join(sb.cache, 'cvc-mcp'), label);
      assert.equal(lines.length, 1, label);
      assert.ok(lines[0].startsWith(`CVC: could not install into ${sb.stable}: another CVC installation holds ${lock}`), `${label}: ${lines[0]}`);
      assert.ok(lines[0].endsWith('Running the verified cached copy this time; the next launch tries again.'), label);
      assert.deepEqual(snapshot(sb.stable), before, label);
    } finally {
      sb.cleanup();
    }
  }
});

test('an owner record that is a symlink is not followed or trusted', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    writeBinaries(sb.stable, { version: '0.7.0', tag: 'old' });
    const planted = path.join(sb.root, 'dead-record.json');
    fs.writeFileSync(planted, ownerJson({ pid: deadPid() }));
    const lock = path.join(sb.stable, LOCK_NAME);
    fs.mkdirSync(lock);
    fs.symlinkSync(planted, path.join(lock, 'owner-0123456789abcdef'));
    const before = snapshot(sb.stable);
    const { result, lines } = await publish(sb);
    assert.equal(result, path.join(sb.cache, 'cvc-mcp'));
    assert.equal(lines.length, 1);
    assert.deepEqual(snapshot(sb.stable), before);
  } finally {
    sb.cleanup();
  }
});

test('an unbreakable lock older than ten minutes keeps the stale hint', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    writeBinaries(sb.stable, { version: '0.7.0', tag: 'old' });
    const lock = plantLock(sb, null);
    const past = new Date(Date.now() - 60 * 60 * 1000);
    fs.utimesSync(lock, past, past);
    const { lines } = await publish(sb);
    assert.equal(lines.length, 1);
    assert.ok(lines[0].includes('may be stale; if no installation is running, remove it with: rm -r'), lines[0]);
  } finally {
    sb.cleanup();
  }
});

test('concurrent launchers facing one dead lock publish exactly once and leave nothing behind', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    writeBinaries(sb.stable, { version: '0.7.0', tag: 'old' });
    plantLock(sb, ownerJson({ pid: deadPid() }));
    const { result: results, lines } = await capture(() => Promise.all([1, 2, 3, 4].map(() => (
      ensurePublished(path.join(sb.cache, 'cvc-mcp'), 'cvc-mcp', { home: sb.home, packageVersion: PACKAGE })
    ))));
    assert.deepEqual(results, Array(4).fill(path.join(sb.stable, 'cvc-mcp')));
    assert.deepEqual(lines, [`Installed CVC ${PACKAGE} to ${sb.stable}`]);
    assert.deepEqual(fs.readdirSync(sb.stable).sort(), ['cvc', 'cvc-lsp', 'cvc-mcp']);
    assert.equal(fs.readFileSync(path.join(sb.stable, 'cvc'), 'utf8'), fs.readFileSync(path.join(sb.cache, 'cvc'), 'utf8'));
  } finally {
    sb.cleanup();
  }
});

test('simulated Windows: a lock whose launcher is dead is broken', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache', suffix: '.exe' });
    writeBinaries(sb.stable, { version: '0.7.0', tag: 'old', suffix: '.exe' });
    const lock = plantLock(sb, ownerJson({ pid: deadPid() }));
    const outcome = await publish(sb, 'cvc-mcp', { platform: 'win32' });
    assert.equal(outcome.result, path.join(sb.stable, 'cvc-mcp.exe'));
    assert.deepEqual(outcome.lines, [`Installed CVC ${PACKAGE} to ${sb.stable}`]);
    assert.equal(fs.existsSync(lock), false);
    assert.deepEqual(leftovers(sb.stable), []);
  } finally {
    sb.cleanup();
  }
});

test('simulated Windows: a lock of a live launcher or an installer is not broken', UNIX_ONLY, async () => {
  for (const content of [ownerJson({ pid: process.pid }), null, 'garbage']) {
    const sb = sandbox();
    try {
      writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache', suffix: '.exe' });
      writeBinaries(sb.stable, { version: '0.7.0', tag: 'old', suffix: '.exe' });
      plantLock(sb, content);
      const before = snapshot(sb.stable);
      const outcome = await publish(sb, 'cvc-mcp', { platform: 'win32' });
      assert.equal(outcome.result, path.join(sb.cache, 'cvc-mcp.exe'));
      assert.equal(outcome.lines.length, 1);
      assert.deepEqual(snapshot(sb.stable), before);
    } finally {
      sb.cleanup();
    }
  }
});

test('the launcher labels the lock it creates while publishing', UNIX_ONLY, async () => {
  for (const platform of ['linux', 'win32']) {
    const sb = sandbox();
    try {
      const suffix = platform === 'win32' ? '.exe' : '';
      writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache', suffix });
      const lock = path.join(sb.stable, LOCK_NAME);
      let seen;
      await withPatched('copyFile', async (copyFile, ...args) => {
        if (!seen) {
          const { entry, record } = readOwner(lock);
          seen = {
            isDirectory: fs.lstatSync(lock).isDirectory(),
            mode: fs.lstatSync(lock).mode & 0o777,
            ownerMode: fs.lstatSync(path.join(lock, entry)).mode & 0o777,
            entry,
            record,
          };
        }
        return copyFile(...args);
      }, () => publish(sb, 'cvc-mcp', { platform }));
      assert.ok(seen, platform);
      assert.equal(seen.isDirectory, true, platform);
      assert.equal(seen.mode, 0o700, platform);
      assert.equal(seen.ownerMode, 0o600, platform);
      assert.equal(seen.record.tool, 'cvc-npm-launcher');
      assert.equal(seen.record.pid, process.pid);
      assert.equal(seen.record.host, os.hostname());
      assert.equal(new Date(seen.record.created).toISOString(), seen.record.created);
      assert.match(seen.record.nonce, /^[0-9a-f]{16}$/);
      assert.equal(seen.entry, `owner-${seen.record.nonce}`);
      assert.equal(fs.existsSync(lock), false, platform);
    } finally {
      sb.cleanup();
    }
  }
});

test('a lock that was swapped for someone else\'s during the publish is not released', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    const lock = path.join(sb.stable, LOCK_NAME);
    const foreign = ownerJson({ host: 'some-other-host.invalid', nonce: 'theirs' });
    let swapped = false;
    const { result, lines } = await withPatched('copyFile', async (copyFile, ...args) => {
      if (!swapped) {
        swapped = true;
        fs.rmSync(lock, { recursive: true });
        fs.mkdirSync(lock);
        fs.writeFileSync(path.join(lock, 'owner-aaaaaaaaaaaaaaaa'), foreign);
      }
      return copyFile(...args);
    }, () => publish(sb));
    assert.equal(result, path.join(sb.stable, 'cvc-mcp'));
    assert.deepEqual(lines, [`Installed CVC ${PACKAGE} to ${sb.stable}`]);
    assert.equal(fs.readFileSync(path.join(lock, 'owner-aaaaaaaaaaaaaaaa'), 'utf8'), foreign);
  } finally {
    sb.cleanup();
  }
});

test('a live lock that replaces the dead one before the claim is left alone', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    writeBinaries(sb.stable, { version: '0.7.0', tag: 'old' });
    const lock = plantLock(sb, ownerJson({ pid: deadPid() }));
    const live = ownerJson({ pid: process.pid, nonce: 'rival' });
    let raced = false;
    const { result, lines } = await withPatchedRename(async (rename, from, to) => {
      if (path.dirname(from) === lock && !raced) {
        // A rival launcher breaks the dead lock and takes a fresh one first.
        raced = true;
        fs.rmSync(lock, { recursive: true });
        fs.mkdirSync(lock);
        fs.writeFileSync(path.join(lock, 'owner-bbbbbbbbbbbbbbbb'), live);
      }
      return rename(from, to);
    }, () => publish(sb));
    assert.equal(raced, true);
    assert.equal(result, path.join(sb.cache, 'cvc-mcp'));
    assert.equal(lines.length, 1);
    assert.ok(lines[0].includes(lock), lines[0]);
    assert.deepEqual(fs.readdirSync(lock), ['owner-bbbbbbbbbbbbbbbb']);
    assert.equal(fs.readFileSync(path.join(lock, 'owner-bbbbbbbbbbbbbbbb'), 'utf8'), live);
    assert.equal(fs.readFileSync(path.join(sb.stable, 'cvc'), 'utf8').includes('old:cvc'), true);
  } finally {
    sb.cleanup();
  }
});

test('a dead lock that vanishes while being broken is simply retried', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    writeBinaries(sb.stable, { version: '0.7.0', tag: 'old' });
    const lock = plantLock(sb, ownerJson({ pid: deadPid() }));
    let raced = false;
    const { result, lines } = await withPatchedRename(async (rename, from, to) => {
      if (path.dirname(from) === lock && !raced) {
        raced = true;
        fs.rmSync(lock, { recursive: true }); // another process already broke it
      }
      return rename(from, to);
    }, () => publish(sb));
    assert.equal(raced, true);
    assert.equal(result, path.join(sb.stable, 'cvc-mcp'));
    assert.deepEqual(lines, [`Installed CVC ${PACKAGE} to ${sb.stable}`]);
    assert.deepEqual(leftovers(sb.stable), []);
  } finally {
    sb.cleanup();
  }
});

test('a dead lock already claimed by a rival breaker that then died is simply broken again', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    writeBinaries(sb.stable, { version: '0.7.0', tag: 'old' });
    const lock = plantLock(sb, ownerJson({ pid: deadPid() }));
    let raced = false;
    const { result, lines } = await withPatchedRename(async (rename, from, to) => {
      if (path.dirname(from) === lock && !raced) {
        // The rival claims the dead record first, then is killed before cleaning up.
        raced = true;
        await rename(from, path.join(lock, 'owner-cccccccccccccccc'));
      }
      return rename(from, to);
    }, () => publish(sb));
    assert.equal(raced, true);
    // The claimed record is still the dead launcher's, so the retry breaks the lock.
    assert.equal(result, path.join(sb.stable, 'cvc-mcp'));
    assert.deepEqual(lines, [`Installed CVC ${PACKAGE} to ${sb.stable}`]);
    assert.deepEqual(leftovers(sb.stable), []);
  } finally {
    sb.cleanup();
  }
});

test('a lock with unexpected contents is never broken, even with a dead owner record in it', UNIX_ONLY, async () => {
  const layouts = {
    'two owner records': lock => {
      fs.writeFileSync(path.join(lock, 'owner-0123456789abcdef'), ownerJson({ pid: deadPid() }));
      fs.writeFileSync(path.join(lock, 'owner-fedcba9876543210'), ownerJson({ pid: deadPid() }));
    },
    'old-style plain "owner" entry': lock => fs.writeFileSync(path.join(lock, 'owner'), ownerJson({ pid: deadPid() })),
    'record plus unrelated file': lock => {
      fs.writeFileSync(path.join(lock, 'owner-0123456789abcdef'), ownerJson({ pid: deadPid() }));
      fs.writeFileSync(path.join(lock, 'note.txt'), 'someone else');
    },
    'owner entry is a directory': lock => fs.mkdirSync(path.join(lock, 'owner-0123456789abcdef')),
  };
  for (const [label, build] of Object.entries(layouts)) {
    const sb = sandbox();
    try {
      writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
      writeBinaries(sb.stable, { version: '0.7.0', tag: 'old' });
      const lock = plantLock(sb, null);
      build(lock);
      const before = snapshot(sb.stable);
      const { result, lines } = await publish(sb);
      assert.equal(result, path.join(sb.cache, 'cvc-mcp'), label);
      assert.equal(lines.length, 1, label);
      assert.deepEqual(snapshot(sb.stable), before, label);
    } finally {
      sb.cleanup();
    }
  }
});

test('separate processes racing on one dead lock publish exactly once and leave nothing behind', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    writeBinaries(sb.stable, { version: '0.7.0', tag: 'old' });
    plantLock(sb, ownerJson({ pid: deadPid() }));
    const outcomes = await launchChildren(sb, 6);
    for (const outcome of outcomes) {
      assert.equal(outcome.status, 0, outcome.stderr);
      assert.equal(outcome.stdout, '');
      assert.equal(outcome.result, path.join(sb.stable, 'cvc-mcp'), outcome.stderr);
    }
    const lines = outcomes.flatMap(outcome => outcome.stderr.split('\n').filter(Boolean));
    assert.deepEqual(lines, [`Installed CVC ${PACKAGE} to ${sb.stable}`]);
    assert.deepEqual(fs.readdirSync(sb.stable).sort(), ['cvc', 'cvc-lsp', 'cvc-mcp']);
    assert.equal(fs.readFileSync(path.join(sb.stable, 'cvc'), 'utf8'), fs.readFileSync(path.join(sb.cache, 'cvc'), 'utf8'));
  } finally {
    sb.cleanup();
  }
});

// ---------------------------------------------------------------------------
// Round 3: private ~/.cvc, unknown versions, host identity, waiting, reaping
// ---------------------------------------------------------------------------

const STABLE_LINE_SUFFIX = 'Running the verified cached copy this time; the next launch tries again.';

function leftAloneLine(sb, reason, suffix = '') {
  return `CVC: could not install into ${sb.stable}: ${path.join(sb.stable, `cvc${suffix}`)} did not report a usable version (${reason}), so it was left in place; delete it to have the launcher replace it. ${STABLE_LINE_SUFFIX}`;
}

// Stable directory with the given script as `cvc` and ordinary 0.7.0 siblings.
function stableWithCvc(sb, cvcScript, { mode = 0o755 } = {}) {
  writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
  writeBinaries(sb.stable, { version: '0.7.0', tag: 'old' });
  fs.writeFileSync(path.join(sb.stable, 'cvc'), cvcScript);
  fs.chmodSync(path.join(sb.stable, 'cvc'), mode);
}

test('launch: a symlinked ~/.cvc is refused before anything is probed or run', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    const shared = path.join(sb.root, 'shared');
    fs.mkdirSync(shared, { mode: 0o777 });
    fs.chmodSync(shared, 0o777);
    const marker = path.join(sb.root, 'was-run');
    const evil = `#!/bin/sh\ntouch "${marker}"\nif [ "$1" = "--version" ]; then echo "cvc 9.9.9"; fi\nexit 0\n`;
    writeBinaries(path.join(shared, 'bin'), { version: '9.9.9', tag: 'evil', scripts: { cvc: evil, 'cvc-mcp': evil, 'cvc-lsp': evil } });
    fs.symlinkSync(shared, path.join(sb.home, '.cvc'));
    const calls = [];
    await assert.rejects(resolveLaunch(sb, 'cvc-mcp', calls), /Refusing unsafe cache directory/);
    assert.equal(fs.existsSync(marker), false, 'nothing under the symlinked directory may be probed or run');
    assert.deepEqual(calls, []);
    assert.equal(fs.lstatSync(path.join(sb.home, '.cvc')).isSymbolicLink(), true);
    assert.equal(fs.statSync(shared).mode & 0o777, 0o777, 'the link target must not be chmod-ed');
  } finally {
    sb.cleanup();
  }
});

test('launch: ~/.cvc left at 0755 is tightened to 0700 and the launch proceeds', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.stable, { version: '0.8.0', tag: 'newer' });
    fs.chmodSync(path.join(sb.home, '.cvc'), 0o755);
    const calls = [];
    const { result, lines } = await resolveLaunch(sb, 'cvc-mcp', calls);
    assert.equal(result, path.join(sb.stable, 'cvc-mcp'));
    assert.deepEqual(calls, []);
    assert.deepEqual(lines, []);
    assert.equal(fs.statSync(path.join(sb.home, '.cvc')).mode & 0o777, 0o700);
  } finally {
    sb.cleanup();
  }
});

test('launch: a missing ~/.cvc is created private before the release is fetched', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    const calls = [];
    const { result } = await resolveLaunch(sb, 'cvc', calls);
    assert.deepEqual(calls, ['cvc']);
    assert.equal(result, path.join(sb.stable, 'cvc'));
    assert.equal(fs.statSync(path.join(sb.home, '.cvc')).mode & 0o777, 0o700);
  } finally {
    sb.cleanup();
  }
});

// A stand-in for child_process.spawnSync that answers one call for `file` with `fake` and
// otherwise runs the real thing, so a failure the sandbox cannot produce can be injected.
function spawnOnce(file, fake) {
  let used = false;
  return (target, args, options) => {
    if (target === file && !used) { used = true; return fake; }
    return spawnSync(target, args, options);
  };
}

test('a cvc whose own file cannot execute is replaced', UNIX_ONLY, async () => {
  // Faults of the file itself: a format the OS rejects (ENOEXEC, EFTYPE, the shell's
  // exit 126), no execute permission at all, or nothing in it. Plain text and truncated
  // binaries are not listed: depending on the C library they surface as exit 126, exit 127
  // or ENOEXEC, and only the first and last are conclusive.
  const real = {
    'a script with no execute permission (EACCES, no execute bits)': () => [fakeScript('0.9.0', 'noexec'), 0o644],
    'a zero-byte file': () => ['', 0o755],
    'a program exiting 126 (cannot execute)': () => ['#!/bin/sh\nexit 126\n', 0o755],
  };
  const injected = {
    'ENOEXEC (exec format error)': { error: Object.assign(new Error('spawnSync ENOEXEC'), { code: 'ENOEXEC' }) },
    'EFTYPE (inappropriate file type)': { error: Object.assign(new Error('spawnSync EFTYPE'), { code: 'EFTYPE' }) },
  };
  const cases = [
    ...Object.entries(real).map(([label, make]) => [label, make, undefined]),
    ...Object.entries(injected).map(([label, fake]) => [label, () => [fakeScript('0.9.0', 'broken'), 0o755], fake]),
  ];
  for (const [label, make, fake] of cases) {
    const sb = sandbox();
    try {
      const [content, mode] = make();
      stableWithCvc(sb, content, { mode });
      const options = fake ? { spawnSync: spawnOnce(path.join(sb.stable, 'cvc'), fake) } : {};
      const { result, lines } = await publish(sb, 'cvc-mcp', options);
      assert.equal(result, path.join(sb.stable, 'cvc-mcp'), label);
      assert.deepEqual(lines, [`Installed CVC ${PACKAGE} to ${sb.stable}`], label);
      assert.equal(fs.readFileSync(path.join(sb.stable, 'cvc'), 'utf8'), fs.readFileSync(path.join(sb.cache, 'cvc'), 'utf8'), label);
    } finally {
      sb.cleanup();
    }
  }
});

test('a cvc that fails because of the environment, not the file, is left in place', UNIX_ONLY, async () => {
  // The same file may run fine on the host that installed it (a container or Flatpak
  // sharing $HOME can lack a library, a loader, or the right to execute).
  const cases = [
    { label: 'a program exiting 127 (missing shared library or loader)', script: '#!/bin/sh\nexit 127\n', reason: 'exit 127' },
    { label: 'a script whose interpreter is missing (ENOENT)', script: '#!/nonexistent/interpreter\n', reason: 'could not be started: ENOENT' },
    { label: 'EACCES on a file that has execute permission (noexec mount, AppArmor, SELinux)', script: fakeScript('0.8.0', 'host'), reason: 'could not be started: EACCES',
      fake: { error: Object.assign(new Error('spawnSync EACCES'), { code: 'EACCES' }) } },
    { label: 'EACCES on Windows, where there are no execute bits to consult', script: fakeScript('0.8.0', 'host'), mode: 0o644, reason: 'could not be started: EACCES', platform: 'win32',
      fake: { error: Object.assign(new Error('spawnSync EACCES'), { code: 'EACCES' }) } },
  ];
  for (const { label, script, reason, fake, mode = 0o755, platform = 'linux' } of cases) {
    const sb = sandbox();
    try {
      const suffix = platform === 'win32' ? '.exe' : '';
      writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache', suffix });
      writeBinaries(sb.stable, { version: '0.8.0', tag: 'host', suffix });
      const cvc = path.join(sb.stable, `cvc${suffix}`);
      fs.writeFileSync(cvc, script);
      fs.chmodSync(cvc, mode);
      const before = snapshot(sb.stable);
      const options = { platform, ...(fake ? { spawnSync: spawnOnce(cvc, fake) } : {}) };
      const { result, lines } = await publish(sb, 'cvc-mcp', options);
      assert.equal(result, path.join(sb.cache, `cvc-mcp${suffix}`), label);
      assert.deepEqual(lines, [leftAloneLine(sb, reason, suffix)], label);
      assert.deepEqual(snapshot(sb.stable), before, label);
    } finally {
      sb.cleanup();
    }
  }
});

test('a cache copy that cannot report the package version never replaces anything', UNIX_ONLY, async () => {
  const cases = [
    { label: 'exits 3', script: '#!/bin/sh\nexit 3\n', reports: 'no usable version (exit 3)' },
    { label: 'prints garbage', script: '#!/bin/sh\necho "hello"\n', reports: 'no usable version (unrecognised output)' },
    { label: 'exits 127 (cannot run in this environment)', script: '#!/bin/sh\nexit 127\n', reports: 'no usable version (exit 127)' },
    { label: 'exits 126', script: '#!/bin/sh\nexit 126\n', reports: 'no usable version (exit 126)' },
    { label: 'reports an older version', script: fakeScript('0.6.0', 'x'), reports: '0.6.0' },
    { label: 'reports a newer version', script: fakeScript('0.9.0', 'x'), reports: '0.9.0' },
    { label: 'hangs', script: '#!/bin/sh\nexec sleep 60\n', reports: 'no usable version (timed out)', probeTimeoutMs: 300 },
  ];
  for (const { label, script, reports, probeTimeoutMs } of cases) {
    for (const installed of ['0.7.0', null]) {
      const sb = sandbox();
      try {
        writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache', scripts: { cvc: script } });
        if (installed) writeBinaries(sb.stable, { version: installed, tag: 'installed' });
        const before = installed ? snapshot(sb.stable) : null;
        const { result, lines } = await publish(sb, 'cvc-mcp', probeTimeoutMs ? { probeTimeoutMs } : {});
        const where = `${label} / ${installed ? 'over 0.7.0' : 'fresh'}`;
        assert.equal(result, path.join(sb.cache, 'cvc-mcp'), where);
        assert.deepEqual(lines, [`CVC: could not install into ${sb.stable}: the staged cvc reports ${reports} instead of ${PACKAGE}. ${STABLE_LINE_SUFFIX}`], where);
        if (installed) assert.deepEqual(snapshot(sb.stable), before, where); // byte-identical: no stage dir, no lock
        else assert.deepEqual(fs.readdirSync(sb.stable), [], where);
      } finally {
        sb.cleanup();
      }
    }
  }
});

test('a published cvc that does not report the package version is still caught after the move', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    const published = path.join(sb.stable, 'cvc');
    const spawnLying = (target, args, options) => (target === published
      ? { status: 0, stdout: 'nope\n', stderr: '', signal: null }
      : spawnSync(target, args, options));
    const { result, lines } = await publish(sb, 'cvc-mcp', { spawnSync: spawnLying });
    assert.equal(result, path.join(sb.cache, 'cvc-mcp'));
    assert.deepEqual(lines, [`CVC: could not install into ${sb.stable}: the published cvc reports no usable version (unrecognised output) instead of ${PACKAGE}. ${STABLE_LINE_SUFFIX}`]);
    assert.deepEqual(leftovers(sb.stable), [], 'the lock and staging directory are gone');
  } finally {
    sb.cleanup();
  }
});

test('a cvc that starts but gives no usable version is left in place and the cache copy runs', UNIX_ONLY, async () => {
  const longOutput = `#!/bin/sh\nprintf 'cvc 9.9.9%s\\n' "$(head -c 200000 /dev/zero | tr '\\0' x)"\n`;
  const cases = {
    'exit 1 with an error on stderr': ['#!/bin/sh\necho "boom" >&2\nexit 1\n', 'exit 1'],
    'exit 1 with the version on stdout (not the legacy shape)': ['#!/bin/sh\necho "cvc 9.9.9"\nexit 1\n', 'exit 1'],
    'exit 2 (a usage error)': ['#!/bin/sh\necho "cvc 9.9.9"\nexit 2\n', 'exit 2'],
    'another non-zero exit': ['#!/bin/sh\necho "cvc 9.9.9"\nexit 3\n', 'exit 3'],
    'exit 125': ['#!/bin/sh\nexit 125\n', 'exit 125'],
    'exit 128': ['#!/bin/sh\nexit 128\n', 'exit 128'],
    'garbage output': ['#!/bin/sh\necho "hello world"\n', 'unrecognised output'],
    'no output': ['#!/bin/sh\nexit 0\n', 'unrecognised output'],
    'another program': ['#!/bin/sh\necho "git version 2.40.0"\n', 'unrecognised output'],
    'a non-version after the name': ['#!/bin/sh\necho "cvc latest"\n', 'unrecognised output'],
    'a banner before the version': ['#!/bin/sh\necho "banner"\necho "cvc 9.9.9"\n', 'unrecognised output'],
    'output beyond the read limit': [longOutput, 'unrecognised output'],
    'killed by a signal': ['#!/bin/sh\nkill -9 $$\n', 'terminated by SIGKILL'],
  };
  for (const [label, [script, reason]] of Object.entries(cases)) {
    const sb = sandbox();
    try {
      stableWithCvc(sb, script);
      const before = snapshot(sb.stable);
      const { result, lines } = await publish(sb);
      assert.equal(result, path.join(sb.cache, 'cvc-mcp'), label);
      assert.deepEqual(lines, [leftAloneLine(sb, reason)], label);
      assert.deepEqual(snapshot(sb.stable), before, label);
    } finally {
      sb.cleanup();
    }
  }
});

test('a --version slower than the probe timeout does not get a newer installation replaced', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    // A busy loop stands in for a cold disk or an antivirus scan delaying `cvc`.
    stableWithCvc(sb, '#!/bin/sh\nwhile :; do :; done\necho "cvc 0.8.0"\n');
    const before = snapshot(sb.stable);
    const { result, lines } = await publish(sb, 'cvc-mcp', { probeTimeoutMs: 300 });
    assert.equal(result, path.join(sb.cache, 'cvc-mcp'));
    assert.deepEqual(lines, [leftAloneLine(sb, 'timed out')]);
    assert.deepEqual(snapshot(sb.stable), before);
  } finally {
    sb.cleanup();
  }
});

test('a hung or SIGTERM-trapping --version is killed at the timeout and left in place', { skip: process.platform === 'win32', timeout: 30000 }, async () => {
  for (const [label, script] of Object.entries({
    'hung': '#!/bin/sh\nexec sleep 60\n',
    'trapping SIGTERM': "#!/bin/sh\ntrap '' TERM\nexec sleep 60\n",
  })) {
    const sb = sandbox();
    try {
      stableWithCvc(sb, script);
      const before = snapshot(sb.stable);
      const started = Date.now();
      const { result, lines } = await publish(sb, 'cvc-mcp', { probeTimeoutMs: 300 });
      assert.ok(Date.now() - started < 5000, `${label}: the probe must end at its timeout, not when the child decides to`);
      assert.equal(result, path.join(sb.cache, 'cvc-mcp'), label);
      assert.deepEqual(lines, [leftAloneLine(sb, 'timed out')], label);
      assert.deepEqual(snapshot(sb.stable), before, label);
    } finally {
      sb.cleanup();
    }
  }
});

test('a newer version followed by build information in parentheses is recognised and left alone', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    writeBinaries(sb.stable, { version: '0.8.0', tag: 'newer', scripts: { cvc: '#!/bin/sh\necho "cvc 0.8.0 (abc1234 2026-11-01)"\n' } });
    const before = snapshot(sb.stable);
    const { result, lines } = await publish(sb);
    assert.equal(result, path.join(sb.stable, 'cvc-mcp'));
    assert.deepEqual(lines, []);
    assert.deepEqual(snapshot(sb.stable), before);
  } finally {
    sb.cleanup();
  }
});

test('a newer version with +build metadata is recognised and left alone', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    writeBinaries(sb.stable, { version: '0.8.0', tag: 'newer', scripts: { cvc: '#!/bin/sh\necho "cvc 0.8.0+local"\n' } });
    const before = snapshot(sb.stable);
    const { result, lines } = await publish(sb);
    assert.equal(result, path.join(sb.stable, 'cvc-mcp'));
    assert.deepEqual(lines, []);
    assert.deepEqual(snapshot(sb.stable), before);
  } finally {
    sb.cleanup();
  }
});

test('version lines with build metadata or trailing text are compared by their version', UNIX_ONLY, async () => {
  const lines = {
    older: ['cvc 0.7.0 (abc1234 2026-11-01)', 'cvc 0.7.0+local', 'cvc 0.7.0-rc.1+build.5 (dirty)', 'cvc 0.7.1-rc.2'],
    newer: ['cvc 0.8.0-rc.1+build.5 (dirty)', 'cvc 0.7.2+x', 'cvc 1.0.0 (release)'],
    equal: ['cvc 0.7.1+local', 'cvc 0.7.1 (abc1234)', 'cvc 0.7.1+a.b (c)'],
  };
  for (const line of lines.older) {
    const sb = sandbox();
    try {
      stableWithCvc(sb, `#!/bin/sh\necho "${line}"\n`);
      const outcome = await publish(sb);
      assert.deepEqual(outcome.lines, [`Installed CVC ${PACKAGE} to ${sb.stable}`], line);
    } finally {
      sb.cleanup();
    }
  }
  for (const [kind, group] of [['newer', lines.newer], ['equal', lines.equal]]) {
    for (const line of group) {
      const sb = sandbox();
      try {
        writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
        writeBinaries(sb.stable, { version: '0.7.0', tag: 'installed', scripts: { cvc: `#!/bin/sh\necho "${line}"\n` } });
        const before = snapshot(sb.stable);
        const outcome = await publish(sb);
        assert.equal(outcome.result, path.join(sb.stable, 'cvc-mcp'), `${kind}: ${line}`);
        assert.deepEqual(outcome.lines, [], `${kind}: ${line}`);
        assert.deepEqual(snapshot(sb.stable), before, `${kind}: ${line}`);
      } finally {
        sb.cleanup();
      }
    }
  }
});

test('launch: a cvc that gives no usable version needs the cache but is never replaced', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.stable, { version: '0.7.0', tag: 'old', scripts: { cvc: '#!/bin/sh\necho "mystery"\n' } });
    const before = snapshot(sb.stable);
    const calls = [];
    const { result, lines } = await resolveLaunch(sb, 'cvc-mcp', calls);
    assert.deepEqual(calls, ['cvc-mcp']);
    assert.equal(result, path.join(sb.cache, 'cvc-mcp'));
    assert.deepEqual(lines, [leftAloneLine(sb, 'unrecognised output')]);
    assert.deepEqual(snapshot(sb.stable), before);
  } finally {
    sb.cleanup();
  }
});

test('only cvc is ever executed by the version checks; cvc-mcp and cvc-lsp are never run', UNIX_ONLY, async () => {
  for (const installed of [PACKAGE, '0.7.0', '0.8.0']) {
    const sb = sandbox();
    try {
      const marker = path.join(sb.root, 'sibling-was-run');
      const tripwire = `#!/bin/sh\ntouch "${marker}"\nexit 0\n`;
      writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
      writeBinaries(sb.stable, { version: installed, tag: 'installed', scripts: { 'cvc-mcp': tripwire, 'cvc-lsp': tripwire } });
      await publish(sb);
      assert.equal(fs.existsSync(marker), false, `installed ${installed}`);
    } finally {
      sb.cleanup();
    }
  }
});

// -- host identity: boot id, PID namespace, uptime ---------------------------

test('lock owner liveness follows boot id, PID namespace, pid and age, in that order', UNIX_ONLY, async () => {
  const hoursAgo = hours => new Date(Date.now() - hours * 3600 * 1000).toISOString();
  const minutesAgo = minutes => new Date(Date.now() - minutes * 60 * 1000).toISOString();
  const tenMinutes = 10 * 60 * 1000;
  const alive = () => process.pid;
  const dead = () => deadPid();
  // [label, record overrides, injected system, expected]. Boot id and PID namespace are
  // null on both sides unless a row says otherwise.
  const rows = [
    ['boot ids differ, pid alive: the machine rebooted', () => ({ pid: alive(), boot: 'boot-A' }), { boot: 'boot-B' }, 'broken'],
    ['boot ids differ beats a PID namespace mismatch', () => ({ pid: alive(), boot: 'boot-A', pidns: 'pid:[1]' }), { boot: 'boot-B', pidns: 'pid:[2]' }, 'broken'],
    ['boot ids differ beats a PID namespace known on one side only', () => ({ pid: alive(), boot: 'boot-A', pidns: 'pid:[1]' }), { boot: 'boot-B' }, 'broken'],
    ['boot ids equal, pid alive, recent', () => ({ pid: alive(), boot: 'boot-A' }), { boot: 'boot-A' }, 'kept'],
    ['boot ids equal wins over an old creation time', () => ({ pid: alive(), boot: 'boot-A', created: hoursAgo(2) }), { boot: 'boot-A', uptimeMs: tenMinutes }, 'kept'],
    ['no boot ids on either side, created before this boot', () => ({ pid: alive(), created: hoursAgo(2) }), { uptimeMs: tenMinutes }, 'broken'],
    ['no boot ids, created after this boot', () => ({ pid: alive(), created: minutesAgo(1) }), { uptimeMs: tenMinutes }, 'kept'],
    ['no boot ids, created just inside the 60 s margin', () => ({ pid: alive(), created: minutesAgo(10.5) }), { uptimeMs: tenMinutes }, 'kept'],
    ['no boot ids, created clearly before the margin', () => ({ pid: alive(), created: minutesAgo(12) }), { uptimeMs: tenMinutes }, 'broken'],
    ['unparseable creation time falls through to the pid (dead)', () => ({ pid: dead(), created: 'yesterday' }), { uptimeMs: tenMinutes }, 'broken'],
    ['unparseable creation time and a live pid is not trusted', () => ({ pid: alive(), created: 'yesterday' }), { uptimeMs: tenMinutes }, 'kept'],
    ['boot id known only in the record (another kind of environment) is foreign', () => ({ pid: dead(), boot: 'boot-A', created: hoursAgo(2) }), { uptimeMs: tenMinutes }, 'kept'],
    ['boot id known only on this side (another kind of environment) is foreign', () => ({ pid: dead(), created: hoursAgo(2) }), { boot: 'boot-B', uptimeMs: tenMinutes }, 'kept'],
    ['PID namespace known only in the record is foreign', () => ({ pid: dead(), boot: 'boot-A', pidns: 'pid:[1]' }), { boot: 'boot-A' }, 'kept'],
    ['PID namespace known only on this side is foreign', () => ({ pid: dead(), boot: 'boot-A' }), { boot: 'boot-A', pidns: 'pid:[1]' }, 'kept'],
    ['a different PID namespace with a dead-looking pid is not judged', () => ({ pid: dead(), boot: 'boot-A', pidns: 'pid:[1]' }), { boot: 'boot-A', pidns: 'pid:[2]' }, 'kept'],
    ['the same PID namespace, dead pid', () => ({ pid: dead(), boot: 'boot-A', pidns: 'pid:[1]' }), { boot: 'boot-A', pidns: 'pid:[1]' }, 'broken'],
    ['no PID namespace on either side, dead pid', () => ({ pid: dead(), boot: 'boot-A' }), { boot: 'boot-A' }, 'broken'],
  ];
  for (const [label, record, system, expected] of rows) {
    const sb = sandbox();
    try {
      writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
      writeBinaries(sb.stable, { version: '0.7.0', tag: 'old' });
      const lock = plantLock(sb, ownerJson({ boot: null, pidns: null, ...record() }));
      const before = snapshot(sb.stable);
      const { result, lines } = await publish(sb, 'cvc-mcp', { system: { boot: null, pidns: null, ...system } });
      if (expected === 'broken') {
        assert.equal(result, path.join(sb.stable, 'cvc-mcp'), label);
        assert.deepEqual(lines, [`Installed CVC ${PACKAGE} to ${sb.stable}`], label);
        assert.equal(fs.existsSync(lock), false, label);
      } else {
        assert.equal(result, path.join(sb.cache, 'cvc-mcp'), label);
        assert.equal(lines.length, 1, label);
        assert.ok(lines[0].includes(lock), `${label}: ${lines[0]}`);
        assert.deepEqual(snapshot(sb.stable), before, label);
      }
    } finally {
      sb.cleanup();
    }
  }
});

test('the lock record carries the PID namespace and boot id when they are known', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    const lock = path.join(sb.stable, LOCK_NAME);
    let injected;
    let real;
    await withPatched('copyFile', async (copyFile, ...args) => {
      if (!injected) injected = readOwner(lock).record;
      return copyFile(...args);
    }, () => publish(sb, 'cvc-mcp', { system: { boot: 'boot-test', pidns: 'pid:[4026531836]' } }));
    assert.equal(injected.boot, 'boot-test');
    assert.equal(injected.pidns, 'pid:[4026531836]');

    const sb2 = sandbox();
    try {
      writeBinaries(sb2.cache, { version: PACKAGE, tag: 'cache' });
      const lock2 = path.join(sb2.stable, LOCK_NAME);
      await withPatched('copyFile', async (copyFile, ...args) => {
        if (!real) real = readOwner(lock2).record;
        return copyFile(...args);
      }, () => publish(sb2));
      if (process.platform === 'linux') {
        assert.equal(real.boot, fs.readFileSync('/proc/sys/kernel/random/boot_id', 'utf8').trim());
        assert.equal(real.pidns, fs.readlinkSync('/proc/self/ns/pid'));
      } else {
        assert.equal(real.boot, null);
        assert.equal(real.pidns, null);
      }
    } finally {
      sb2.cleanup();
    }
  } finally {
    sb.cleanup();
  }
});

// -- stale-lock hint and Windows rmdir retries ---------------------------------

test('the stale hint names the exact command that removes the lock', UNIX_ONLY, async () => {
  for (const platform of ['linux', 'win32']) {
    const sb = sandbox();
    try {
      const suffix = platform === 'win32' ? '.exe' : '';
      writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache', suffix });
      writeBinaries(sb.stable, { version: '0.7.0', tag: 'old', suffix });
      const lock = plantLock(sb, null);
      const past = new Date(Date.now() - 60 * 60 * 1000);
      fs.utimesSync(lock, past, past);
      const command = platform === 'win32' ? `Remove-Item -Recurse -Force '${lock}'` : `rm -r '${lock}'`;
      const { lines } = await publish(sb, 'cvc-mcp', { platform });
      assert.deepEqual(lines, [`CVC: could not install into ${sb.stable}: another CVC installation holds ${lock} (older than 10 minutes, so it may be stale; if no installation is running, remove it with: ${command}). ${STABLE_LINE_SUFFIX}`], platform);
    } finally {
      sb.cleanup();
    }
  }
});

test('single quotes in the lock path are escaped in the removal command', UNIX_ONLY, async () => {
  for (const platform of ['linux', 'win32']) {
    const sb = sandbox("it's");
    try {
      const suffix = platform === 'win32' ? '.exe' : '';
      writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache', suffix });
      writeBinaries(sb.stable, { version: '0.7.0', tag: 'old', suffix });
      const lock = plantLock(sb, null);
      const past = new Date(Date.now() - 60 * 60 * 1000);
      fs.utimesSync(lock, past, past);
      const { lines } = await publish(sb, 'cvc-mcp', { platform });
      const expected = platform === 'win32'
        ? `Remove-Item -Recurse -Force '${lock.replace(/'/g, "''")}'`
        : `rm -r '${lock.replace(/'/g, "'\\''")}'`;
      assert.ok(lines[0].includes(`remove it with: ${expected})`), lines[0]);
    } finally {
      sb.cleanup();
    }
  }
});

test('simulated Windows: removing the lock directory is retried through transient failures', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache', suffix: '.exe' });
    const lock = path.join(sb.stable, LOCK_NAME);
    let failures = 0;
    let attempts = 0;
    const outcome = await withPatched('rmdir', async (rmdir, target, ...rest) => {
      if (target === lock) {
        attempts++;
        if (failures < 3) { failures++; throw Object.assign(new Error('delete pending'), { code: ['ENOTEMPTY', 'EBUSY', 'EPERM'][failures - 1] }); }
      }
      return rmdir(target, ...rest);
    }, () => publish(sb, 'cvc-mcp', { platform: 'win32' }));
    assert.deepEqual(outcome.lines, [`Installed CVC ${PACKAGE} to ${sb.stable}`]);
    assert.equal(attempts, 4);
    assert.equal(fs.existsSync(lock), false);
  } finally {
    sb.cleanup();
  }
});

test('simulated Windows: breaking a dead lock also retries the directory removal', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache', suffix: '.exe' });
    writeBinaries(sb.stable, { version: '0.7.0', tag: 'old', suffix: '.exe' });
    const lock = plantLock(sb, ownerJson({ pid: deadPid() }));
    let failures = 0;
    const outcome = await withPatched('rmdir', async (rmdir, target, ...rest) => {
      if (target === lock && failures < 2) { failures++; throw Object.assign(new Error('delete pending'), { code: 'ENOTEMPTY' }); }
      return rmdir(target, ...rest);
    }, () => publish(sb, 'cvc-mcp', { platform: 'win32' }));
    assert.deepEqual(outcome.lines, [`Installed CVC ${PACKAGE} to ${sb.stable}`]);
    assert.equal(failures, 2);
  } finally {
    sb.cleanup();
  }
});

test('on Unix the lock directory removal is attempted once', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    const lock = path.join(sb.stable, LOCK_NAME);
    let attempts = 0;
    const outcome = await withPatched('rmdir', async (rmdir, target, ...rest) => {
      if (target === lock) { attempts++; throw Object.assign(new Error('not permitted'), { code: 'EPERM' }); }
      return rmdir(target, ...rest);
    }, () => publish(sb));
    assert.deepEqual(outcome.lines, [`Installed CVC ${PACKAGE} to ${sb.stable}`]);
    assert.equal(attempts, 1);
  } finally {
    try { fs.rmdirSync(path.join(sb.stable, LOCK_NAME)); } catch (_) { /* already gone */ }
    sb.cleanup();
  }
});

// -- waiting for a launcher that is publishing right now ----------------------

test('a launcher waits for a live launcher to finish and then runs what it published, silently', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    writeBinaries(sb.stable, { version: '0.7.0', tag: 'old' });
    const lock = plantLock(sb, ownerJson({ pid: process.pid }));
    const timer = setTimeout(() => {
      writeBinaries(sb.stable, { version: PACKAGE, tag: 'by-the-other-launcher' });
      fs.rmSync(lock, { recursive: true });
    }, 300);
    const started = Date.now();
    const { result, lines } = await publish(sb, 'cvc-mcp', { lockWaitMs: 4000 });
    clearTimeout(timer);
    assert.ok(Date.now() - started >= 250, 'it must have waited for the lock');
    assert.equal(result, path.join(sb.stable, 'cvc-mcp'));
    assert.deepEqual(lines, []);
    assert.match(fs.readFileSync(path.join(sb.stable, 'cvc'), 'utf8'), /by-the-other-launcher/);
  } finally {
    sb.cleanup();
  }
});

test('a launcher whose wait ends with the work still undone takes the lock and publishes', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    writeBinaries(sb.stable, { version: '0.7.0', tag: 'old' });
    const lock = plantLock(sb, ownerJson({ pid: process.pid }));
    const timer = setTimeout(() => fs.rmSync(lock, { recursive: true }), 300);
    const { result, lines } = await publish(sb, 'cvc-mcp', { lockWaitMs: 4000 });
    clearTimeout(timer);
    assert.equal(result, path.join(sb.stable, 'cvc-mcp'));
    assert.deepEqual(lines, [`Installed CVC ${PACKAGE} to ${sb.stable}`]);
    assert.equal(fs.readFileSync(path.join(sb.stable, 'cvc'), 'utf8'), fs.readFileSync(path.join(sb.cache, 'cvc'), 'utf8'));
    assert.deepEqual(leftovers(sb.stable), []);
  } finally {
    sb.cleanup();
  }
});

test('a lock that is still held when the wait budget ends gives the usual fallback line', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    writeBinaries(sb.stable, { version: '0.7.0', tag: 'old' });
    const lock = plantLock(sb, ownerJson({ pid: process.pid }));
    const before = snapshot(sb.stable);
    const started = Date.now();
    const { result, lines } = await publish(sb, 'cvc-mcp', { lockWaitMs: 400 });
    assert.ok(Date.now() - started >= 350, 'it must have waited out the budget');
    assert.equal(result, path.join(sb.cache, 'cvc-mcp'));
    assert.deepEqual(lines, [`CVC: could not install into ${sb.stable}: another CVC installation holds ${lock}. ${STABLE_LINE_SUFFIX}`]);
    assert.deepEqual(snapshot(sb.stable), before);
  } finally {
    sb.cleanup();
  }
});

test('a launcher whose holder dies during the wait breaks the lock and publishes', UNIX_ONLY, async () => {
  const sb = sandbox();
  let holder;
  let timer;
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    writeBinaries(sb.stable, { version: '0.7.0', tag: 'old' });
    // A real child process holds the "lock" and is killed while the launcher waits.
    holder = require('node:child_process').spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], { stdio: 'ignore' });
    await new Promise(resolve => holder.once('spawn', resolve));
    plantLock(sb, ownerJson({ pid: holder.pid }));
    timer = setTimeout(() => holder.kill('SIGKILL'), 300);
    const exited = new Promise(resolve => holder.once('exit', resolve));
    const { result, lines } = await publish(sb, 'cvc-mcp', { lockWaitMs: 4000 });
    holder.kill('SIGKILL');
    await exited;
    assert.equal(result, path.join(sb.stable, 'cvc-mcp'));
    assert.deepEqual(lines, [`Installed CVC ${PACKAGE} to ${sb.stable}`]);
    assert.deepEqual(leftovers(sb.stable), []);
  } finally {
    clearTimeout(timer);
    if (holder) holder.kill('SIGKILL');
    sb.cleanup();
  }
});

test('installer locks, foreign locks and unlabelled locks are not waited for', UNIX_ONLY, async () => {
  const layouts = {
    'an installer-style empty lock': () => null,
    'a record from another host': () => ownerJson({ host: 'some-other-host.invalid' }),
    'a record from another PID namespace': () => ownerJson({ pidns: 'pid:[1]' }),
  };
  for (const [label, content] of Object.entries(layouts)) {
    const sb = sandbox();
    try {
      writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
      writeBinaries(sb.stable, { version: '0.7.0', tag: 'old' });
      const lock = plantLock(sb, content());
      const past = new Date(Date.now() - 2 * 60 * 1000); // too old to be a lock still being labelled
      fs.utimesSync(lock, past, past);
      const started = Date.now();
      const { result, lines } = await publish(sb, 'cvc-mcp', { lockWaitMs: 4000, system: { pidns: 'pid:[2]' } });
      assert.ok(Date.now() - started < 1500, `${label} must not be waited for`);
      assert.equal(result, path.join(sb.cache, 'cvc-mcp'), label);
      assert.equal(lines.length, 1, label);
    } finally {
      sb.cleanup();
    }
  }
});

test('a brand-new lock that is still being labelled is waited for, not reported as a conflict', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
    const launcher = () => ensurePublished(path.join(sb.cache, 'cvc-mcp'), 'cvc-mcp', { home: sb.home, packageVersion: PACKAGE });
    const { result: results, lines } = await capture(() => withPatched('writeFile', async (writeFile, target, ...rest) => {
      // Slow the first launcher's record write so the second finds an empty, brand-new lock.
      if (String(target).includes(`${LOCK_NAME}${path.sep}owner-`)) await pause(250);
      return writeFile(target, ...rest);
    }, () => Promise.all([launcher(), pause(40).then(launcher)])));
    assert.deepEqual(results, [path.join(sb.stable, 'cvc-mcp'), path.join(sb.stable, 'cvc-mcp')]);
    assert.deepEqual(lines, [`Installed CVC ${PACKAGE} to ${sb.stable}`]);
  } finally {
    sb.cleanup();
  }
});

test('simultaneous first launches in one process publish once with no fallback lines', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    const { result: results, lines } = await capture(() => Promise.all(Array.from({ length: 6 }, () => (
      ensurePublished(path.join(sb.cache, 'cvc-mcp'), 'cvc-mcp', { home: sb.home, packageVersion: PACKAGE })
    ))));
    assert.deepEqual(results, Array(6).fill(path.join(sb.stable, 'cvc-mcp')));
    assert.deepEqual(lines, [`Installed CVC ${PACKAGE} to ${sb.stable}`]);
    assert.deepEqual(fs.readdirSync(sb.stable).sort(), ['cvc', 'cvc-lsp', 'cvc-mcp']);
  } finally {
    sb.cleanup();
  }
});

function launchChildren(sb, count) {
  const releasePath = path.resolve(__dirname, '..', 'bin', 'release.js');
  return Promise.all(Array.from({ length: count }, (_, index) => new Promise(resolve => {
    const resultFile = path.join(sb.root, `result-${index}.txt`);
    const child = require('node:child_process').spawn(process.execPath, ['-e', CHILD_SCRIPT, releasePath, path.join(sb.cache, 'cvc-mcp'), sb.home, resultFile], { stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', chunk => { stdout += chunk; });
    child.stderr.on('data', chunk => { stderr += chunk; });
    child.on('close', status => resolve({ status, stdout, stderr, result: fs.existsSync(resultFile) ? fs.readFileSync(resultFile, 'utf8') : undefined }));
  })));
}

test('simultaneous first launches in separate processes publish once with no fallback lines', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    const outcomes = await launchChildren(sb, 6);
    for (const outcome of outcomes) {
      assert.equal(outcome.status, 0, outcome.stderr);
      assert.equal(outcome.stdout, '');
      assert.equal(outcome.result, path.join(sb.stable, 'cvc-mcp'), outcome.stderr);
    }
    const lines = outcomes.flatMap(outcome => outcome.stderr.split('\n').filter(Boolean));
    assert.deepEqual(lines, [`Installed CVC ${PACKAGE} to ${sb.stable}`]);
    assert.deepEqual(fs.readdirSync(sb.stable).sort(), ['cvc', 'cvc-lsp', 'cvc-mcp']);
  } finally {
    sb.cleanup();
  }
});

// -- orphaned staging directories ------------------------------------------------

test('orphaned staging directories are reaped under the lock; look-alikes and fresh ones are kept', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    writeBinaries(sb.stable, { version: '0.7.0', tag: 'old' });
    const past = new Date(Date.now() - 60 * 60 * 1000);
    const oldDir = name => {
      const dir = path.join(sb.stable, name);
      fs.mkdirSync(dir);
      fs.writeFileSync(path.join(dir, 'cvc'), 'x'.repeat(1000));
      fs.utimesSync(dir, past, past);
      return dir;
    };
    const orphan = oldDir('.cvc-install.AbC123');
    const longOrphan = oldDir('.cvc-install.zQ9xK2');
    const fresh = path.join(sb.stable, '.cvc-install.fresh01');
    fs.mkdirSync(fresh);
    const kept = [
      oldDir('.cvc-install.lockabc'), // starts with the lock's name
      oldDir('.cvc-install.lock.stale-abcdef'),
      oldDir('.cvc-install.abc12'), // too short for an mkdtemp suffix
      oldDir('.cvc-install.abcdefg'), // too long for an mkdtemp suffix
      oldDir('.cvc-install.userbackup'), // someone's own directory
      oldDir('.cvc-install.ab-cdef1'), // not alphanumeric
      oldDir('.cvc-installX123456'), // different prefix
      oldDir('.cvc-old-cvc-0123456789abcdef'),
      fresh,
    ];
    const linkTarget = path.join(sb.root, 'link-target');
    fs.mkdirSync(linkTarget);
    fs.writeFileSync(path.join(linkTarget, 'precious'), 'keep me');
    const link = path.join(sb.stable, '.cvc-install.Link99');
    fs.symlinkSync(linkTarget, link);
    const file = path.join(sb.stable, '.cvc-install.FileAA');
    fs.writeFileSync(file, 'a file, not a directory');
    fs.utimesSync(file, past, past);

    const lock = path.join(sb.stable, LOCK_NAME);
    let duringPublish;
    const { result, lines } = await withPatched('copyFile', async (copyFile, ...args) => {
      if (!duringPublish) duringPublish = { lockHeld: fs.statSync(lock).isDirectory(), orphanGone: !fs.existsSync(orphan), longOrphanGone: !fs.existsSync(longOrphan) };
      return copyFile(...args);
    }, () => publish(sb));
    assert.equal(result, path.join(sb.stable, 'cvc-mcp'));
    assert.deepEqual(lines, [`Installed CVC ${PACKAGE} to ${sb.stable}`]);
    assert.deepEqual(duringPublish, { lockHeld: true, orphanGone: true, longOrphanGone: true });
    assert.equal(fs.existsSync(orphan), false);
    assert.equal(fs.existsSync(longOrphan), false);
    for (const entry of [...kept, link, file]) assert.equal(fs.existsSync(entry) || fs.lstatSync(entry, { throwIfNoEntry: false }) !== undefined, true, entry);
    assert.equal(fs.lstatSync(link).isSymbolicLink(), true);
    assert.equal(fs.readFileSync(path.join(linkTarget, 'precious'), 'utf8'), 'keep me');
    assert.equal(fs.existsSync(lock), false, 'the launcher released its own lock');
  } finally {
    sb.cleanup();
  }
});

test('nothing is reaped when nothing needs publishing', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    writeBinaries(sb.stable, { version: '0.8.0', tag: 'newer' });
    const orphan = path.join(sb.stable, '.cvc-install.AbC123');
    fs.mkdirSync(orphan);
    const past = new Date(Date.now() - 60 * 60 * 1000);
    fs.utimesSync(orphan, past, past);
    const { lines } = await publish(sb);
    assert.deepEqual(lines, []);
    assert.equal(fs.existsSync(orphan), true);
  } finally {
    sb.cleanup();
  }
});

test('the Windows extraction never inherits the MCP client\'s stdin or stdout', () => {
  const source = fs.readFileSync(path.resolve(__dirname, '..', 'bin', 'release.js'), 'utf8');
  const powershell = source.split('\n').filter(line => line.includes("'-File', script"));
  assert.equal(powershell.length, 1);
  assert.ok(powershell[0].includes("stdio: ['ignore', 'ignore', 'inherit']"), powershell[0]);
  assert.ok(!powershell[0].includes("stdio: 'inherit'"), powershell[0]);
});

// -- environments that share a home: a boot id or PID namespace known on one side only --

test('a record whose boot id or PID namespace is known on only one side is neither waited for nor broken', UNIX_ONLY, async () => {
  // Windows and WSL (or a host and a Flatpak) can share a host name and HOME while
  // describing themselves differently; the pid in such a record means nothing here.
  const cases = [
    ['boot id only in the record', { boot: 'boot-from-elsewhere', pidns: null }, { boot: null, pidns: null }],
    ['boot id only on this side', { boot: null, pidns: null }, { boot: 'boot-here', pidns: null }],
    ['PID namespace only in the record', { boot: 'boot-A', pidns: 'pid:[1]' }, { boot: 'boot-A', pidns: null }],
    ['PID namespace only on this side', { boot: 'boot-A', pidns: null }, { boot: 'boot-A', pidns: 'pid:[1]' }],
  ];
  for (const [label, record, system] of cases) {
    for (const pid of [process.pid, deadPid()]) {
      const sb = sandbox();
      try {
        writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
        writeBinaries(sb.stable, { version: '0.7.0', tag: 'old' });
        const lock = plantLock(sb, ownerJson({ ...record, pid }));
        const before = snapshot(sb.stable);
        const started = Date.now();
        const { result, lines } = await publish(sb, 'cvc-mcp', { lockWaitMs: 4000, system });
        const where = `${label} (pid ${pid === process.pid ? 'alive' : 'dead'})`;
        assert.ok(Date.now() - started < 1500, `${where}: must not wait`);
        assert.equal(result, path.join(sb.cache, 'cvc-mcp'), where);
        assert.deepEqual(lines, [`CVC: could not install into ${sb.stable}: another CVC installation holds ${lock}. ${STABLE_LINE_SUFFIX}`], where);
        assert.deepEqual(snapshot(sb.stable), before, where);
      } finally {
        sb.cleanup();
      }
    }
  }
});

// -- a live-looking lock older than any real publish is a reused pid, not a launcher --

test('a live-looking launcher lock whose record is older than a real publish is not waited for', UNIX_ONLY, async () => {
  for (const [label, ageMs, waits] of [['5 minutes old', 5 * 60 * 1000, false], ['90 seconds old', 90 * 1000, false], ['30 seconds old', 30 * 1000, true]]) {
    const sb = sandbox();
    try {
      writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
      writeBinaries(sb.stable, { version: '0.7.0', tag: 'old' });
      const lock = plantLock(sb, ownerJson({ pid: process.pid, created: new Date(Date.now() - ageMs).toISOString() }));
      const before = snapshot(sb.stable);
      const started = Date.now();
      const { result, lines } = await publish(sb, 'cvc-mcp', { lockWaitMs: waits ? 400 : 4000 });
      const elapsed = Date.now() - started;
      assert.equal(result, path.join(sb.cache, 'cvc-mcp'), label);
      assert.deepEqual(lines, [`CVC: could not install into ${sb.stable}: another CVC installation holds ${lock}. ${STABLE_LINE_SUFFIX}`], label);
      assert.deepEqual(snapshot(sb.stable), before, label);
      if (waits) assert.ok(elapsed >= 350, `${label}: a recent record is waited for (took ${elapsed} ms)`);
      else assert.ok(elapsed < 1500, `${label}: an old record must not be waited for (took ${elapsed} ms)`);
    } finally {
      sb.cleanup();
    }
  }
});

test('an old live-looking lock still gets the stale hint once the lock itself is old enough', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    writeBinaries(sb.stable, { version: '0.7.0', tag: 'old' });
    const lock = plantLock(sb, ownerJson({ pid: process.pid, created: new Date(Date.now() - 3600 * 1000).toISOString() }));
    const past = new Date(Date.now() - 3600 * 1000);
    fs.utimesSync(lock, past, past);
    const { lines } = await publish(sb, 'cvc-mcp', { lockWaitMs: 4000 });
    assert.deepEqual(lines, [`CVC: could not install into ${sb.stable}: another CVC installation holds ${lock} (older than 10 minutes, so it may be stale; if no installation is running, remove it with: rm -r '${lock}'). ${STABLE_LINE_SUFFIX}`]);
  } finally {
    sb.cleanup();
  }
});

// -- the wait runs on a monotonic clock --

test('the lock wait is measured on a monotonic clock, so a wall-clock jump cannot cut it short', UNIX_ONLY, async () => {
  const sb = sandbox();
  const realNow = Date.now;
  const timers = [];
  try {
    writeBinaries(sb.cache, { version: PACKAGE, tag: 'cache' });
    writeBinaries(sb.stable, { version: '0.7.0', tag: 'old' });
    const lock = plantLock(sb, ownerJson({ pid: process.pid }));
    let offset = 0;
    Date.now = () => realNow() + offset;
    // 30 s is enough to blow through a 3 s wall-clock deadline, not enough to age the record out.
    timers.push(setTimeout(() => { offset = 30 * 1000; }, 100));
    timers.push(setTimeout(() => {
      writeBinaries(sb.stable, { version: PACKAGE, tag: 'by-the-other-launcher' });
      fs.rmSync(lock, { recursive: true });
    }, 450));
    const started = realNow();
    const { result, lines } = await publish(sb, 'cvc-mcp', { lockWaitMs: 3000 });
    Date.now = realNow;
    assert.ok(realNow() - started >= 400, 'it kept waiting after the clock jumped');
    assert.equal(result, path.join(sb.stable, 'cvc-mcp'));
    assert.deepEqual(lines, []);
  } finally {
    Date.now = realNow;
    timers.forEach(clearTimeout);
    sb.cleanup();
  }
});

// -- releases before v0.7.0 print --version to stderr and exit 1 --

test('the legacy --version shape (stderr, exit 1, empty stdout) is read as a version', UNIX_ONLY, async () => {
  const legacy = version => `#!/bin/sh\necho "cvc ${version}" >&2\nexit 1\n`;

  // An older release is recognised as older and replaced.
  for (const line of ['cvc 0.6.0', 'cvc 0.6.0 (abc1234 2026-01-01)', 'cvc 0.6.0+local']) {
    const sb = sandbox();
    try {
      stableWithCvc(sb, `#!/bin/sh\necho "${line}" >&2\nexit 1\n`);
      const { result, lines } = await publish(sb);
      assert.equal(result, path.join(sb.stable, 'cvc-mcp'), line);
      assert.deepEqual(lines, [`Installed CVC ${PACKAGE} to ${sb.stable}`], line);
      assert.equal(fs.readFileSync(path.join(sb.stable, 'cvc'), 'utf8'), fs.readFileSync(path.join(sb.cache, 'cvc'), 'utf8'), line);
    } finally {
      sb.cleanup();
    }
  }

  // A newer version in the same shape is handled consistently as a known version.
  const newer = sandbox();
  try {
    writeBinaries(newer.cache, { version: PACKAGE, tag: 'cache' });
    writeBinaries(newer.stable, { version: '0.7.0', tag: 'installed', scripts: { cvc: legacy('0.9.0') } });
    const before = snapshot(newer.stable);
    const { result, lines } = await publish(newer);
    assert.equal(result, path.join(newer.stable, 'cvc-mcp'));
    assert.deepEqual(lines, []);
    assert.deepEqual(snapshot(newer.stable), before);
  } finally {
    newer.cleanup();
  }
});

test('only the exact legacy shape counts; every other failure with a version in it stays unknown', UNIX_ONLY, async () => {
  const cases = {
    'exit 1 with garbage on stderr': ['#!/bin/sh\necho "error: something broke" >&2\nexit 1\n', 'exit 1'],
    'exit 1 with nothing on either stream': ['#!/bin/sh\nexit 1\n', 'exit 1'],
    'exit 2 with the version on stderr': ['#!/bin/sh\necho "cvc 0.6.0" >&2\nexit 2\n', 'exit 2'],
    'exit 1 with the version on stderr but output on stdout': ['#!/bin/sh\necho "usage" \necho "cvc 0.6.0" >&2\nexit 1\n', 'exit 1'],
    'exit 0 with the version only on stderr': ['#!/bin/sh\necho "cvc 0.6.0" >&2\nexit 0\n', 'unrecognised output'],
    'exit 1 with a banner before the version on stderr': ['#!/bin/sh\necho "banner" >&2\necho "cvc 0.6.0" >&2\nexit 1\n', 'exit 1'],
    'exit 1 with the legacy line beyond the read limit': [`#!/bin/sh\nprintf 'cvc 0.6.0%s\\n' "$(head -c 200000 /dev/zero | tr '\\0' x)" >&2\nexit 1\n`, 'unrecognised output'],
  };
  for (const [label, [script, reason]] of Object.entries(cases)) {
    const sb = sandbox();
    try {
      stableWithCvc(sb, script);
      const before = snapshot(sb.stable);
      const { result, lines } = await publish(sb);
      assert.equal(result, path.join(sb.cache, 'cvc-mcp'), label);
      assert.deepEqual(lines, [leftAloneLine(sb, reason)], label);
      assert.deepEqual(snapshot(sb.stable), before, label);
    } finally {
      sb.cleanup();
    }
  }
});

test('a pre-0.7.0 installation is repaired by launch, so hooks stop pointing at a cvc without ingest', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    writeBinaries(sb.stable, { version: '0.6.0', tag: 'legacy', scripts: { cvc: '#!/bin/sh\necho "cvc 0.6.0" >&2\nexit 1\n' } });
    const calls = [];
    const { result, lines } = await resolveLaunch(sb, 'cvc', calls);
    assert.deepEqual(calls, ['cvc']);
    assert.equal(result, path.join(sb.stable, 'cvc'));
    assert.deepEqual(lines, [`Installed CVC ${PACKAGE} to ${sb.stable}`]);
    assert.equal(spawnSync(path.join(sb.stable, 'cvc'), ['--version'], { encoding: 'utf8' }).stdout, `cvc ${PACKAGE}\n`);
  } finally {
    sb.cleanup();
  }
});

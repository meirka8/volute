'use strict';

// The launcher exposes one npm command per CVC executable: cvc, cvc-mcp and
// cvc-lsp. These tests pin the contract between package.json's `bin`, the shims
// in bin/, and the BINARIES list in bin/release.js, so adding or renaming a
// command in one place cannot leave the others behind. Nothing here touches the
// real home directory or the network: the "binaries" are tiny /bin/sh scripts
// in a throwaway home.

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { _launchTargetForTest: launchTarget } = require('../bin/release');
const manifest = require('../package.json');

const UNIX_ONLY = { skip: process.platform === 'win32' };
const packageRoot = path.resolve(__dirname, '..');
const COMMANDS = ['cvc', 'cvc-mcp', 'cvc-lsp'];
const DETAILS = { platform: 'linux', osTag: 'unknown-linux-gnu', archTag: 'x86_64' };
const NEWER = '99.0.0';

// Answers `--version` like cvc (the launcher probes `cvc`) and otherwise reports
// which executable ran with which arguments, exiting 7 so the exit code travels.
function fakeBinary(name, version) {
  return `#!/bin/sh\nif [ "$1" = "--version" ]; then echo "cvc ${version}"; exit 0; fi\necho "ran ${name} $*"\nexit 7\n`;
}

function writeBinaries(dir, version) {
  fs.mkdirSync(dir, { recursive: true });
  for (const name of COMMANDS) fs.writeFileSync(path.join(dir, name), fakeBinary(name, version), { mode: 0o755 });
}

function sandbox() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'cvc-commands-test-'));
  const home = path.join(root, 'home');
  fs.mkdirSync(home);
  return {
    root,
    home,
    stable: path.join(home, '.cvc', 'bin'),
    cache: path.join(home, '.cvc', 'mcp-cache', `v${manifest.version}-x86_64-unknown-linux-gnu`),
    cleanup() { fs.rmSync(root, { recursive: true, force: true }); },
  };
}

test('the manifest exposes exactly cvc, cvc-mcp and cvc-lsp', () => {
  assert.deepEqual(Object.keys(manifest.bin).sort(), [...COMMANDS].sort());
});

test('each command maps to a shim that launches the executable of the same name', () => {
  for (const command of COMMANDS) {
    // No "./" prefix: npm rewrites it while publishing, which the CI manifest guards reject.
    assert.equal(manifest.bin[command], `bin/${command}.js`);
    const file = path.join(packageRoot, manifest.bin[command]);
    const source = fs.readFileSync(file, 'utf8');
    assert.ok(source.startsWith('#!/usr/bin/env node\n'), `${command}: node shebang`);
    assert.match(source, new RegExp(`require\\('\\./release'\\)\\.launch\\('${command}'\\)`), `${command}: launches the executable of the same name`);
    const check = spawnSync(process.execPath, ['--check', file], { encoding: 'utf8' });
    assert.equal(check.status, 0, `${command}: ${check.stderr}`);
    if (process.platform !== 'win32') assert.notEqual(fs.statSync(file).mode & 0o111, 0, `${command}: executable in the repository`);
  }
});

test('every shim in bin/ is a declared command', () => {
  const shims = fs.readdirSync(path.join(packageRoot, 'bin')).filter(file => file !== 'release.js').map(file => file.replace(/\.js$/, ''));
  assert.deepEqual(shims.sort(), [...COMMANDS].sort());
});

test('launch resolves each command to its own binary in an existing installation without downloading', UNIX_ONLY, async () => {
  for (const command of COMMANDS) {
    const sb = sandbox();
    try {
      writeBinaries(sb.stable, NEWER);
      const installRelease = async () => { throw new Error('must not download'); };
      const target = await launchTarget(command, { home: sb.home, packageVersion: manifest.version, details: DETAILS, installRelease, lockWaitMs: 0 });
      assert.equal(target, path.join(sb.stable, command));
    } finally {
      sb.cleanup();
    }
  }
});

test('launch publishes a fresh release and returns the stable cvc-lsp path', UNIX_ONLY, async () => {
  const sb = sandbox();
  try {
    const calls = [];
    const installRelease = async (details, executable) => {
      calls.push(executable);
      writeBinaries(sb.cache, manifest.version);
      return path.join(sb.cache, executable);
    };
    const originalError = console.error;
    console.error = () => {}; // "Installed CVC <version> to <dir>" goes to stderr
    let target;
    try {
      target = await launchTarget('cvc-lsp', { home: sb.home, packageVersion: manifest.version, details: DETAILS, installRelease, lockWaitMs: 0 });
    } finally {
      console.error = originalError;
    }
    assert.deepEqual(calls, ['cvc-lsp']);
    assert.equal(target, path.join(sb.stable, 'cvc-lsp'));
    assert.deepEqual(fs.readdirSync(sb.stable).sort(), [...COMMANDS].sort());
  } finally {
    sb.cleanup();
  }
});

test('the real shims run the matching executable, pass arguments and return its exit code', UNIX_ONLY, () => {
  for (const command of COMMANDS) {
    const sb = sandbox();
    try {
      writeBinaries(sb.stable, NEWER);
      const result = spawnSync(process.execPath, [path.join(packageRoot, manifest.bin[command]), 'one', '--two'], {
        encoding: 'utf8',
        // A download would be a bug here; this host never resolves, so it fails fast instead.
        env: { ...process.env, HOME: sb.home, USERPROFILE: sb.home, CVC_RELEASE_BASE_URL: 'https://download.invalid' },
      });
      assert.equal(result.stdout, `ran ${command} one --two\n`, result.stderr);
      assert.equal(result.stderr, '');
      assert.equal(result.status, 7);
    } finally {
      sb.cleanup();
    }
  }
});

'use strict';

// install.ps1 takes `.cvc-install.lock` as a plain FILE (a FileStream opened
// with CreateNew); install.sh takes it as a directory. publish.test.js plants
// directories only, so the PowerShell layout needs its own check: whatever
// occupies the lock path and is not a lock this launcher wrote must hold the
// launcher off, be named in the one fallback line, and be left exactly as it
// was. Like publish.test.js, every test uses its own temporary home and never
// touches the real one.

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { _ensurePublishedForTest: ensurePublished } = require('../bin/release');

const UNIX_ONLY = { skip: process.platform === 'win32' };
const PACKAGE = '0.7.1';
const NAMES = ['cvc', 'cvc-mcp', 'cvc-lsp'];

function writeBinaries(dir, version, suffix) {
  fs.mkdirSync(dir, { recursive: true });
  for (const name of NAMES) {
    const file = path.join(dir, name + suffix);
    fs.writeFileSync(file, `#!/bin/sh\nif [ "$1" = "--version" ]; then echo "cvc ${version}"; exit 0; fi\nexit 0\n`, { mode: 0o755 });
    fs.chmodSync(file, 0o755);
  }
}

async function publishWithLockPlanted(plant, platform) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'cvc-lock-interop-'));
  const suffix = platform === 'win32' ? '.exe' : '';
  const home = path.join(root, 'home');
  const stable = path.join(home, '.cvc', 'bin');
  const cache = path.join(home, '.cvc', 'mcp-cache', `v${PACKAGE}-test`);
  const lock = path.join(stable, '.cvc-install.lock');
  const lines = [];
  const original = console.error;
  try {
    writeBinaries(cache, PACKAGE, suffix);
    writeBinaries(stable, '0.7.0', suffix);
    plant(lock, root);
    const before = fs.readdirSync(stable).sort().map(name => [name, fs.lstatSync(path.join(stable, name)).ino]);
    console.error = (...args) => lines.push(args.join(' '));
    const result = await ensurePublished(path.join(cache, `cvc-mcp${suffix}`), 'cvc-mcp', { home, packageVersion: PACKAGE, platform });
    console.error = original;
    return {
      result, lines, lock, cache, stable, suffix,
      unchanged: JSON.stringify(before) === JSON.stringify(fs.readdirSync(stable).sort().map(name => [name, fs.lstatSync(path.join(stable, name)).ino])),
      oldCvc: fs.readFileSync(path.join(stable, `cvc${suffix}`), 'utf8'),
    };
  } finally {
    console.error = original;
    fs.rmSync(root, { recursive: true, force: true });
  }
}

const LAYOUTS = {
  'a regular file (install.ps1)': lock => fs.writeFileSync(lock, ''),
  'a regular file holding what looks like a dead launcher record': lock => fs.writeFileSync(lock, JSON.stringify({ tool: 'cvc-npm-launcher', pid: 2 ** 22 + 1, host: os.hostname() })),
  'a symlink to a directory': (lock, root) => {
    fs.mkdirSync(path.join(root, 'elsewhere'));
    fs.symlinkSync(path.join(root, 'elsewhere'), lock);
  },
  'a dangling symlink': (lock, root) => fs.symlinkSync(path.join(root, 'missing'), lock),
};

for (const platform of ['linux', 'win32']) {
  for (const [label, plant] of Object.entries(LAYOUTS)) {
    test(`${platform === 'win32' ? 'simulated Windows: ' : ''}a lock that is ${label} holds the launcher off and is left alone`, UNIX_ONLY, async () => {
      const outcome = await publishWithLockPlanted(plant, platform);
      assert.equal(outcome.result, path.join(outcome.cache, `cvc-mcp${outcome.suffix}`), 'runs the verified cache copy');
      assert.deepEqual(outcome.lines, [`CVC: could not install into ${outcome.stable}: another CVC installation holds ${outcome.lock}. Running the verified cached copy this time; the next launch tries again.`]);
      assert.equal(outcome.unchanged, true, 'nothing in the stable directory was replaced, added or removed');
      assert.match(outcome.oldCvc, /cvc 0\.7\.0/, 'the older installation was not touched');
    });
  }
}

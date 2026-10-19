'use strict';

// Cross-component contract for issue #115, installer side.
//
// The launcher decides whether ~/.cvc/bin needs publishing by running
// `cvc --version` WITHOUT taking the shared lock. That is only safe if a new
// `cvc` implies its sibling binaries are already new, so every writer of
// ~/.cvc/bin must move `cvc` into place last: the launcher (PUBLISH_ORDER) and
// both installers. The installers must also drop the shared lock before their
// interactive PATH prompt, or an unanswered prompt would keep the launcher from
// publishing. The shell and PowerShell scripts have no other automated test, so
// this reads the three sources and fails when one of them drifts.

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const root = path.resolve(__dirname, '..', '..', '..');
const launcher = fs.readFileSync(path.join(__dirname, '..', 'bin', 'release.js'), 'utf8');
const installSh = fs.readFileSync(path.join(root, 'install.sh'), 'utf8');
const installPs1 = fs.readFileSync(path.join(root, 'install.ps1'), 'utf8');

function required(source, pattern, what, where) {
  const match = pattern.exec(source);
  assert.ok(match, `cannot find ${what} in ${where}; if it moved, update the matching script and this contract test`);
  return match;
}

const quoted = (text, quote) => [...text.matchAll(new RegExp(`${quote}([^${quote}]+)${quote}`, 'g'))].map(match => match[1]);
const publishOrder = quoted(required(launcher, /const PUBLISH_ORDER = \[([^\]]+)\];/, 'PUBLISH_ORDER', 'release.js')[1], "'");

test('the launcher itself publishes cvc last', () => {
  assert.equal(publishOrder.at(-1), 'cvc');
  assert.deepEqual([...publishOrder].sort(), ['cvc', 'cvc-lsp', 'cvc-mcp']);
});

test('install.sh moves the binaries into place in the launcher\'s order, cvc last', () => {
  const loop = required(installSh, /for binary in ([a-z -]+); do\s*\n\s*mv -f "\$PUBLISH_STAGE\/\$binary" "\$INSTALL_DIR\/\$binary"\s*\n\s*done/, 'the move loop', 'install.sh');
  assert.deepEqual(loop[1].trim().split(/\s+/), publishOrder);
});

test('install.sh releases the stage and the shared lock before the PATH prompt', () => {
  const loop = required(installSh, /mv -f "\$PUBLISH_STAGE\/\$binary" "\$INSTALL_DIR\/\$binary"/, 'the move loop', 'install.sh');
  const stage = required(installSh, /rmdir "\$PUBLISH_STAGE"[^\n]*\n\s*PUBLISH_STAGE=\s*\n/, 'the release of the publish stage', 'install.sh');
  const lock = required(installSh, /rmdir "\$LOCK_DIR"[^\n]*\n\s*LOCK_DIR=\s*\n/, 'the release of the shared lock', 'install.sh');
  const prompt = required(installSh, /read -r ADD_PATH/, 'the PATH prompt', 'install.sh');
  assert.ok(loop.index < stage.index && stage.index < lock.index, 'release comes right after the moves');
  assert.ok(lock.index < prompt.index, 'the lock is released before the prompt waits for an answer');
});

test('install.ps1 moves the binaries into place in the launcher\'s order, cvc.exe last', () => {
  const order = quoted(required(installPs1, /\$MoveOrder = @\(([^)]*)\)/, '$MoveOrder', 'install.ps1')[1], '"');
  const declared = quoted(required(installPs1, /\$Binaries = @\(([^)]*)\)/, '$Binaries', 'install.ps1')[1], '"');
  assert.deepEqual(order, publishOrder.map(name => `${name}.exe`));
  assert.deepEqual([...order].sort(), [...declared].sort(), 'every prepared binary is moved');
  required(installPs1, /foreach \(\$Binary in \$MoveOrder\) \{ Move-Item /, 'the move loop over $MoveOrder', 'install.ps1');
});

test('install.ps1 releases the shared lock (finally block) before the PATH prompt', () => {
  const release = required(installPs1, /\nfinally \{[\s\S]*?\$InstallLock\.Dispose\(\)[\s\S]*?Remove-Item -LiteralPath \$InstallLockPath[\s\S]*?\n\}/, 'the finally block that releases the lock', 'install.ps1');
  const prompt = required(installPs1, /Read-Host /, 'the PATH prompt', 'install.ps1');
  assert.ok(release.index < prompt.index);
});

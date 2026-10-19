'use strict';

// Cross-component contract for issue #115.
//
// `cvc harness install claude-code` (Rust, cvc-cli/src/commands/harness.rs)
// steers the hook path away from the launcher's versioned cache and onto the
// stable binary the launcher publishes (this package, bin/release.js). The two
// sides only meet through a path layout, and each side's own tests use their
// own constants, so renaming a directory on one side would leave both
// suites green while the hooks silently pin to the cache again. This test
// reads both sources and fails when the layouts disagree.

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const launcherSource = fs.readFileSync(path.join(__dirname, '..', 'bin', 'release.js'), 'utf8');
const harnessFile = path.resolve(__dirname, '..', '..', '..', 'cvc-cli', 'src', 'commands', 'harness.rs');
const harnessSource = fs.readFileSync(harnessFile, 'utf8');

function required(source, pattern, what, where) {
  const match = pattern.exec(source);
  assert.ok(match, `cannot find ${what} in ${where}; if it moved, update the matching side and this contract test`);
  return match;
}

test('the Rust cache marker is the directory the launcher unpacks releases into', () => {
  const rust = required(harnessSource, /const LAUNCHER_CACHE_DIR: &str = "([^"]+)";/, 'LAUNCHER_CACHE_DIR', 'harness.rs')[1];
  const node = required(launcherSource, /const cacheRoot = path\.join\(os\.homedir\(\), '\.cvc', '([^']+)'\);/, 'the cache root', 'release.js')[1];
  assert.equal(rust, node);
});

test('the Rust stable binary path is the one the launcher publishes to', () => {
  const rustFn = required(harnessSource, /fn stable_binary\(home: &Path\) -> PathBuf \{([\s\S]*?)\n\}/, 'stable_binary()', 'harness.rs')[1];
  const rustParts = [...rustFn.matchAll(/\.join\("([^"]+)"\)/g)].map(match => match[1]);
  const rustBinary = required(rustFn, /format!\("([a-z-]+)\{\}", env::consts::EXE_SUFFIX\)/, 'the binary file name', 'harness.rs')[1];
  const nodeParts = [...required(launcherSource, /stableDir: path\.join\(home, ((?:'[^']+',? ?)+)\)/, 'stableDir', 'release.js')[1].matchAll(/'([^']+)'/g)].map(match => match[1]);

  assert.deepEqual(rustParts, nodeParts, 'directory components under the home directory');
  assert.equal(rustBinary, 'cvc', 'the hooks run `cvc`');
  assert.ok(required(launcherSource, /const BINARIES = \[([^\]]+)\];/, 'BINARIES', 'release.js')[1].includes(`'${rustBinary}'`),
    'the launcher publishes the binary the hooks run');
});

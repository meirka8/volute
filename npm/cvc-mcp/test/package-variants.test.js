'use strict';

// The launcher is published under two names from one source directory. These
// tests pin what scripts/stage-package.mjs promises: the second package differs
// from the source in its name and its README and in nothing else, both expose
// the same three commands, the third-party notices still reach both tarballs,
// and the file the release-version gate checks is the file both names derive
// their version from. Packing happens only in staged copies in temporary
// directories, never in the source directory, because release.test.js packs
// there and the two would race over THIRD-PARTY-NOTICES.md under `node --test`.

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { pathToFileURL } = require('node:url');

const UNIX_ONLY = { skip: process.platform === 'win32' };
const packageRoot = path.resolve(__dirname, '..');
const repoRoot = path.resolve(packageRoot, '..', '..');
const scriptFile = path.join(packageRoot, 'scripts', 'stage-package.mjs');
const source = file => fs.readFileSync(path.join(packageRoot, file));
const sourceManifest = JSON.parse(source('package.json'));

let loaded;
const loadStage = () => (loaded ??= import(pathToFileURL(scriptFile).href));

async function withStaged(name, options, body) {
  const { stagePackage } = await loadStage();
  const staged = stagePackage(name, options);
  try {
    return await body(staged);
  } finally {
    fs.rmSync(staged.root, { recursive: true, force: true });
  }
}

function filesUnder(dir) {
  const found = [];
  (function walk(current) {
    for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
      const full = path.join(current, entry.name);
      if (entry.isDirectory()) walk(full);
      else found.push(path.relative(dir, full));
    }
  })(dir);
  return found.sort();
}

// What `npm pack --dry-run` would put in the tarball (running prepack and
// postpack like a real pack). npm 12 keys the JSON by package name, older
// versions return an array.
function packList(dir) {
  const result = spawnSync('npm', ['pack', '--dry-run', '--json'], { cwd: dir, encoding: 'utf8' });
  assert.equal(result.status, 0, result.stderr);
  const parsed = JSON.parse(result.stdout);
  const [entry] = Array.isArray(parsed) ? parsed : Object.values(parsed);
  return new Map(entry.files.map(file => [file.path, file.size]));
}

function changedLines(a, b) {
  const left = a.toString().split('\n');
  const right = b.toString().split('\n');
  assert.equal(left.length, right.length, 'same number of lines');
  return left.map((line, index) => [line, right[index]]).filter(([x, y]) => x !== y);
}

test('the names, in publish order, start with the one the source manifest carries', async () => {
  const { PACKAGES, packageNames } = await loadStage();
  assert.deepEqual(packageNames(), ['@volute_cvc/cvc-mcp', '@volute_cvc/cvc']);
  assert.equal(PACKAGES[0].name, sourceManifest.name);
  assert.equal(PACKAGES[0].readme, 'README.md');
});

test('staging the source name copies it byte for byte', async () => {
  await withStaged('@volute_cvc/cvc-mcp', {}, ({ dir, manifest }) => {
    assert.equal(manifest.name, '@volute_cvc/cvc-mcp');
    assert.deepEqual(fs.readFileSync(path.join(dir, 'package.json')), source('package.json'));
    assert.deepEqual(fs.readFileSync(path.join(dir, 'README.md')), source('README.md'));
    for (const file of ['LICENSE', '.gitignore', 'bin/cvc.js', 'bin/cvc-mcp.js', 'bin/cvc-lsp.js', 'bin/release.js', 'scripts/stage-third-party-notices.mjs']) {
      assert.deepEqual(fs.readFileSync(path.join(dir, file)), source(file), file);
    }
  });
});

test('the staged package leaves out what is not part of the package', async () => {
  await withStaged('@volute_cvc/cvc', {}, ({ root, dir }) => {
    const files = filesUnder(dir);
    for (const absent of ['test', 'readmes', 'node_modules', 'THIRD-PARTY-NOTICES.md', 'test-local-install.sh']) {
      assert.equal(files.some(file => file === absent || file.startsWith(`${absent}${path.sep}`)), false, absent);
    }
    assert.deepEqual(files.filter(file => /^readme/i.test(path.basename(file))), ['README.md']);
    // The notices source sits where prepack expects it: two levels above the package.
    assert.deepEqual(fs.readFileSync(path.join(root, 'THIRD-PARTY-NOTICES.md')), fs.readFileSync(path.join(repoRoot, 'THIRD-PARTY-NOTICES.md')));
    assert.equal(path.relative(root, dir), path.join('npm', 'cvc'));
  });
});

test('@volute_cvc/cvc differs from the source in its name and its README and nothing else', async () => {
  await withStaged('@volute_cvc/cvc', {}, ({ dir, manifest }) => {
    assert.equal(manifest.name, '@volute_cvc/cvc');
    const staged = JSON.parse(fs.readFileSync(path.join(dir, 'package.json')));
    assert.deepEqual(staged, { ...sourceManifest, name: '@volute_cvc/cvc' });
    assert.deepEqual(Object.keys(staged), Object.keys(sourceManifest), 'same key order');
    assert.deepEqual(changedLines(source('package.json'), fs.readFileSync(path.join(dir, 'package.json'))),
      [['  "name": "@volute_cvc/cvc-mcp",', '  "name": "@volute_cvc/cvc",']]);
    assert.equal(staged.version, sourceManifest.version, 'the version comes from the one source manifest');

    assert.deepEqual(fs.readFileSync(path.join(dir, 'README.md')), source('readmes/cvc.md'));
    assert.notDeepEqual(fs.readFileSync(path.join(dir, 'README.md')), source('README.md'));

    // Every other file the package could ship is the source file, byte for byte.
    for (const file of filesUnder(dir).filter(name => name !== 'package.json' && name !== 'README.md')) {
      assert.deepEqual(fs.readFileSync(path.join(dir, file)), source(file), file);
    }
  });
});

test('--version stamps the version and only the version', async () => {
  await withStaged('@volute_cvc/cvc', { version: '0.0.0-ci.123' }, ({ dir }) => {
    const staged = JSON.parse(fs.readFileSync(path.join(dir, 'package.json')));
    assert.deepEqual(staged, { ...sourceManifest, name: '@volute_cvc/cvc', version: '0.0.0-ci.123' });
    assert.equal(changedLines(source('package.json'), fs.readFileSync(path.join(dir, 'package.json'))).length, 2);
  });
  await withStaged('@volute_cvc/cvc-mcp', { version: sourceManifest.version }, ({ dir }) => {
    assert.deepEqual(fs.readFileSync(path.join(dir, 'package.json')), source('package.json'), 'restamping the same version is a no-op');
  });
});

test('staging rejects unknown names, bad versions and a used --out directory', async () => {
  const { stagePackage } = await loadStage();
  assert.throws(() => stagePackage('@volute_cvc/other'), /Unknown package/);
  assert.throws(() => stagePackage('cvc'), /Unknown package/);
  for (const version of ['v1.2.3', '1.2', '', 'latest', '1.2.3 ', '01.2.3', '1.2.3-', '1.2.3;touch x']) {
    assert.throws(() => stagePackage('@volute_cvc/cvc', { version }), /semantic version/, JSON.stringify(version));
  }
  const used = fs.mkdtempSync(path.join(os.tmpdir(), 'cvc-stage-used-'));
  try {
    fs.writeFileSync(path.join(used, 'occupied'), 'x');
    assert.throws(() => stagePackage('@volute_cvc/cvc', { out: used }), /absent or an empty directory/);
    assert.deepEqual(fs.readdirSync(used), ['occupied']);
    fs.rmSync(path.join(used, 'occupied'));
    const staged = stagePackage('@volute_cvc/cvc', { out: used });
    assert.equal(staged.root, used);
    assert.equal(fs.existsSync(path.join(used, 'npm', 'cvc', 'package.json')), true);
  } finally {
    fs.rmSync(used, { recursive: true, force: true });
  }
});

test('the command line lists the names, prints the staged directory and rejects misuse', UNIX_ONLY, () => {
  const run = (...args) => spawnSync(process.execPath, [scriptFile, ...args], { encoding: 'utf8' });
  assert.equal(run('--list').stdout, '@volute_cvc/cvc-mcp\n@volute_cvc/cvc\n');
  const staged = run('@volute_cvc/cvc', '--version', '1.2.3');
  assert.equal(staged.status, 0, staged.stderr);
  const dir = staged.stdout.trim();
  try {
    assert.equal(path.basename(dir), 'cvc');
    assert.equal(JSON.parse(fs.readFileSync(path.join(dir, 'package.json'))).version, '1.2.3');
  } finally {
    fs.rmSync(path.resolve(dir, '..', '..'), { recursive: true, force: true });
  }
  for (const args of [[], ['--list', '@volute_cvc/cvc'], ['--list', '--version', '1.2.3'], ['@volute_cvc/cvc', '@volute_cvc/cvc-mcp'], ['--nope'], ['@volute_cvc/other']]) {
    const result = run(...args);
    assert.equal(result.status, 1, `${args.join(' ')}: ${result.stdout}`);
    assert.equal(result.stdout, '');
    assert.notEqual(result.stderr, '');
  }
});

test('both names expose exactly cvc, cvc-mcp and cvc-lsp, all shipped in the tarball', UNIX_ONLY, async () => {
  const { packageNames } = await loadStage();
  for (const name of packageNames()) {
    await withStaged(name, {}, ({ dir }) => {
      const staged = JSON.parse(fs.readFileSync(path.join(dir, 'package.json')));
      assert.deepEqual(Object.keys(staged.bin).sort(), ['cvc', 'cvc-lsp', 'cvc-mcp'], name);
      const packed = packList(dir);
      for (const [command, target] of Object.entries(staged.bin)) {
        assert.equal(target, `bin/${command}.js`, `${name}: ${command}`);
        assert.equal(fs.existsSync(path.join(dir, target)), true, `${name}: ${target} exists`);
        assert.equal(packed.has(target), true, `${name}: ${target} is covered by "files"`);
      }
      assert.equal(packed.has('bin/release.js'), true, `${name}: the launcher itself ships`);
    });
  }
});

test('the two tarballs list the same files and the notices reach both', UNIX_ONLY, async () => {
  const { packageNames } = await loadStage();
  const lists = [];
  for (const name of packageNames()) {
    await withStaged(name, {}, ({ dir }) => {
      const packed = packList(dir);
      assert.equal([...packed.keys()].filter(file => file === 'THIRD-PARTY-NOTICES.md').length, 1, `${name}: notices staged by prepack`);
      assert.equal(fs.existsSync(path.join(dir, 'THIRD-PARTY-NOTICES.md')), false, `${name}: postpack removed the staged notices`);
      assert.equal(packed.get('THIRD-PARTY-NOTICES.md'), fs.statSync(path.join(repoRoot, 'THIRD-PARTY-NOTICES.md')).size, `${name}: the root notices, in full`);
      for (const file of packed.keys()) assert.doesNotMatch(file, /^(test|readmes|node_modules)\/|\.tgz$|stage-package|publish-convergent/, `${name}: ${file} must not ship`);
      lists.push(packed);
    });
  }
  const [mcp, cvc] = lists;
  assert.deepEqual([...cvc.keys()].sort(), [...mcp.keys()].sort());
  const differing = [...mcp.keys()].filter(file => mcp.get(file) !== cvc.get(file)).sort();
  assert.deepEqual(differing, ['README.md', 'package.json']);
});

test('each package page starts with its own name and the two READMEs point at each other', () => {
  const mcp = source('README.md').toString();
  const cvc = source('readmes/cvc.md').toString();
  assert.match(mcp, /^# @volute_cvc\/cvc-mcp\n/);
  assert.match(cvc, /^# @volute_cvc\/cvc\n/);
  assert.match(mcp, /npm install --global @volute_cvc\/cvc\b(?!-)/, 'cvc-mcp points CLI users at the new name');
  assert.match(cvc, /npm install --global @volute_cvc\/cvc\n/);
  assert.match(cvc, /cvc --version/);
  for (const command of ['cvc', 'cvc-mcp', 'cvc-lsp']) assert.match(cvc, new RegExp(`\`${command}\``), command);
  assert.match(cvc, /~\/\.cvc\/bin/);
  assert.match(cvc, /@volute_cvc\/cvc-mcp/, 'cvc points existing MCP users back at the old name');
});

test('the version gate checks the manifest both names take their version from', async () => {
  const { PACKAGES } = await loadStage();
  const gate = fs.readFileSync(path.join(repoRoot, '.github', 'scripts', 'check-release-version.mjs'), 'utf8');
  const sourceFile = path.relative(repoRoot, path.join(packageRoot, 'package.json')).split(path.sep).join('/');
  assert.ok(gate.includes(`'${sourceFile}'`), `check-release-version.mjs must check ${sourceFile}, the manifest every package name is staged from`);
  // Nothing else carries a version of its own: every name is derived from the source.
  assert.equal(PACKAGES.length, 2);
  const versions = [];
  for (const { name } of PACKAGES) await withStaged(name, {}, ({ manifest }) => versions.push(manifest.version));
  assert.deepEqual(versions, [sourceManifest.version, sourceManifest.version]);
});

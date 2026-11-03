'use strict';

// The launcher is published under two names from one source directory. These
// tests pin what scripts/stage-package.mjs promises: the second package differs
// from the source in its name, its description and its README and in nothing
// else, both expose the same three commands, the third-party notices still reach
// both tarballs, and the file the release-version gate checks is the file both
// names derive their version from. Packing happens only in staged copies in temporary
// directories, never in the source directory, because release.test.js packs
// there and the two would race over THIRD-PARTY-NOTICES.md under `node --test`.
// Staging does read the source directory while that pack runs, though. The tests
// that build a source directory of their own pin that staging never looks at what
// a pack leaves there (the staged notices, a tarball) and skips an entry that
// vanished after it was listed, which the real directory cannot reproduce on demand.

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const { syncBuiltinESMExports } = require('node:module');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { pathToFileURL } = require('node:url');

const { mock } = test;

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

// A throwaway repository holding a small package directory shaped like npm/cvc-mcp
// (stagePackage finds the notices two levels above it). It lets a test put into the
// source directory what a concurrent `npm pack` puts into the real one, which no
// test may touch. `body` gets the repository root and the package directory.
async function withSource(body) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'cvc-stage-source-'));
  const dir = path.join(root, 'npm', 'cvc-mcp');
  try {
    fs.mkdirSync(path.join(dir, 'bin'), { recursive: true });
    fs.mkdirSync(path.join(dir, 'readmes'));
    fs.writeFileSync(path.join(root, 'THIRD-PARTY-NOTICES.md'), 'notices\n');
    for (const file of ['package.json', 'README.md', 'readmes/cvc.md']) fs.writeFileSync(path.join(dir, file), source(file));
    fs.writeFileSync(path.join(dir, 'LICENSE'), 'license\n');
    fs.writeFileSync(path.join(dir, 'bin', 'cvc.js'), '// launcher\n');
    return await body({ root, dir });
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
}

// The files of a package staged from withSource's directory.
const FIXTURE_FILES = ['LICENSE', 'README.md', path.join('bin', 'cvc.js'), 'package.json'];

// Stages `name` from withSource's directory into the repository root, so that the
// stage goes with it even when staging throws.
async function stageFixture({ root, dir }, name = '@volute_cvc/cvc') {
  const { stagePackage } = await loadStage();
  return stagePackage(name, { sourceDir: dir, out: path.join(root, 'stage', name.split('/').pop()) });
}

test('the names, in publish order, start with the one the source manifest carries', async () => {
  const { PACKAGES, packageNames } = await loadStage();
  assert.deepEqual(packageNames(), ['@volute_cvc/cvc-mcp', '@volute_cvc/cvc']);
  assert.equal(PACKAGES[0].name, sourceManifest.name);
  assert.equal(PACKAGES[0].readme, 'README.md');
  assert.equal(PACKAGES[0].description, undefined, 'the source name keeps the source manifest description');
});

test('@volute_cvc/cvc describes itself as the command-line launcher, not as an MCP server', async () => {
  const { PACKAGES } = await loadStage();
  const { description } = PACKAGES.find(entry => entry.name === '@volute_cvc/cvc');
  assert.equal(typeof description, 'string');
  assert.notEqual(description, sourceManifest.description);
  assert.doesNotMatch(description, /\bMCP server\b/i);
  assert.match(description, /^Command-line launcher for Volute CVC/);
  for (const command of ['cvc', 'cvc-mcp', 'cvc-lsp']) assert.ok(description.includes(command), command);
  assert.match(sourceManifest.description, /^MCP server for CVC/, 'the cvc-mcp description is untouched');
});

test('every staged manifest differs from the source only in name, description and version', async () => {
  const { PACKAGES } = await loadStage();
  for (const entry of PACKAGES) {
    await withStaged(entry.name, {}, ({ dir }) => {
      const staged = JSON.parse(fs.readFileSync(path.join(dir, 'package.json')));
      assert.deepEqual(Object.keys(staged), Object.keys(sourceManifest), `${entry.name}: same keys, same order`);
      assert.equal(staged.name, entry.name);
      assert.equal(staged.description, entry.description ?? sourceManifest.description, `${entry.name}: description`);
      assert.equal(staged.version, sourceManifest.version, `${entry.name}: the version comes from the one source manifest`);
      for (const key of Object.keys(sourceManifest).filter(key => !['name', 'description', 'version'].includes(key))) {
        assert.deepEqual(staged[key], sourceManifest[key], `${entry.name}: ${key}`);
      }
    });
  }
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

test('@volute_cvc/cvc differs from the source in its name, description and README and nothing else', async () => {
  const { PACKAGES } = await loadStage();
  const { description } = PACKAGES.find(entry => entry.name === '@volute_cvc/cvc');
  await withStaged('@volute_cvc/cvc', {}, ({ dir, manifest }) => {
    assert.equal(manifest.name, '@volute_cvc/cvc');
    assert.equal(manifest.description, description);
    const staged = JSON.parse(fs.readFileSync(path.join(dir, 'package.json')));
    assert.deepEqual(staged, { ...sourceManifest, name: '@volute_cvc/cvc', description });
    assert.deepEqual(Object.keys(staged), Object.keys(sourceManifest), 'same key order');
    assert.deepEqual(changedLines(source('package.json'), fs.readFileSync(path.join(dir, 'package.json'))), [
      ['  "name": "@volute_cvc/cvc-mcp",', '  "name": "@volute_cvc/cvc",'],
      [`  "description": ${JSON.stringify(sourceManifest.description)},`, `  "description": ${JSON.stringify(description)},`],
    ]);
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
  const { PACKAGES } = await loadStage();
  const { description } = PACKAGES.find(entry => entry.name === '@volute_cvc/cvc');
  await withStaged('@volute_cvc/cvc', { version: '0.0.0-ci.123' }, ({ dir }) => {
    const staged = JSON.parse(fs.readFileSync(path.join(dir, 'package.json')));
    assert.deepEqual(staged, { ...sourceManifest, name: '@volute_cvc/cvc', description, version: '0.0.0-ci.123' });
    assert.equal(changedLines(source('package.json'), fs.readFileSync(path.join(dir, 'package.json'))).length, 3, 'name, description, version');
  });
  await withStaged('@volute_cvc/cvc-mcp', { version: '0.0.0-ci.123' }, ({ dir }) => {
    assert.deepEqual(changedLines(source('package.json'), fs.readFileSync(path.join(dir, 'package.json'))),
      [[`  "version": ${JSON.stringify(sourceManifest.version)},`, '  "version": "0.0.0-ci.123",']], 'cvc-mcp: only the version, name and description untouched');
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

test('the tarball and the staged notices a pack leaves in the source are left out without being looked at', UNIX_ONLY, async () => {
  const { packageNames } = await loadStage();
  await withSource(async fixture => {
    // Staging refuses a symbolic link wherever it would stage one. These are links
    // (to nowhere, too), so staging gets past them only if it never looks at them.
    const names = [`volute_cvc-cvc-mcp-${sourceManifest.version}.tgz`, 'another.tgz', 'THIRD-PARTY-NOTICES.md', 'node_modules', '.git', 'test', 'test-local-install.sh', 'README.txt'];
    for (const name of names) fs.symlinkSync(path.join(fixture.root, 'missing'), path.join(fixture.dir, name));
    for (const name of packageNames()) {
      assert.deepEqual(filesUnder((await stageFixture(fixture, name)).dir), FIXTURE_FILES, name);
    }
  });
});

test('an entry that vanishes after the directory was listed is skipped, not an error', async () => {
  await loadStage();
  await withSource(async fixture => {
    const vanishing = path.join(fixture.dir, 'vanishing.txt');
    fs.writeFileSync(vanishing, 'listed, then gone');
    // cpSync lists a directory and only then calls the filter on each entry, so
    // whatever someone else removes in between is gone when the filter looks at it.
    // Remove this entry right before the staging looks at it, as a concurrent
    // `npm pack` removes its tarball. The staging script takes lstatSync from
    // node:fs: replace it there and sync the live bindings to reach that script.
    const lstat = fs.lstatSync;
    let looked = false;
    mock.method(fs, 'lstatSync', (file, ...rest) => {
      if (file === vanishing) {
        fs.unlinkSync(file);
        looked = true;
      }
      return lstat(file, ...rest);
    });
    syncBuiltinESMExports();
    try {
      const staged = await stageFixture(fixture);
      assert.equal(looked, true, 'the staging looked at the entry after it was listed');
      assert.deepEqual(filesUnder(staged.dir), FIXTURE_FILES);
    } finally {
      mock.restoreAll();
      syncBuiltinESMExports();
    }
  });
});

test('a symbolic link that would be staged is still refused', UNIX_ONLY, async () => {
  for (const link of ['extra', path.join('bin', 'extra')]) {
    for (const live of [true, false]) {
      await withSource(async fixture => {
        fs.symlinkSync(live ? path.join(fixture.dir, 'LICENSE') : path.join(fixture.root, 'missing'), path.join(fixture.dir, link));
        await assert.rejects(stageFixture(fixture), /Refusing to stage a symbolic link: \S*extra$/, `${link}, ${live ? 'live' : 'dangling'}`);
      });
    }
  }
});

test('shouldStage decides the exclusions from the top-level name, before it looks at the entry', UNIX_ONLY, async () => {
  const { shouldStage } = await loadStage();
  await withSource(({ root, dir }) => {
    // Under a regular file every lookup fails with ENOTDIR, so an answer instead of
    // an exception means that the entry was left out by its name alone.
    fs.writeFileSync(path.join(root, 'a-file'), '');
    const unreachable = path.join(root, 'a-file', 'package');
    for (const name of ['volute_cvc-cvc-mcp-0.7.0.tgz', 'another.tgz', 'THIRD-PARTY-NOTICES.md', 'node_modules', '.git', 'test', 'readmes', 'test-local-install.sh', 'README.md', 'README', 'readme.txt']) {
      assert.equal(shouldStage(path.join(unreachable, name), unreachable), false, name);
      assert.equal(shouldStage(path.join(unreachable, name, 'deeper', 'file'), unreachable), false, `${name}/deeper/file`);
    }
    for (const name of ['package.json', 'bin', 'LICENSE', '.gitignore', 'readmes.md', 'notes.tgz.bak']) {
      assert.throws(() => shouldStage(path.join(unreachable, name), unreachable), { code: 'ENOTDIR' }, `${name} is looked at`);
    }
    // Only the top-level name counts: below a directory that is staged, so is everything.
    fs.writeFileSync(path.join(dir, 'bin', 'inner.tgz'), '');
    fs.writeFileSync(path.join(dir, 'bin', 'README.md'), '');
    assert.equal(shouldStage(path.join(dir, 'bin', 'inner.tgz'), dir), true);
    assert.equal(shouldStage(path.join(dir, 'bin', 'README.md'), dir), true);
  });
});

test('shouldStage skips an entry that is gone and answers for one that is there', async () => {
  const { shouldStage } = await loadStage();
  await withSource(({ dir }) => {
    assert.equal(shouldStage(dir, dir), true, 'the source directory itself');
    assert.equal(shouldStage(path.join(dir, 'bin'), dir), true, 'a directory');
    assert.equal(shouldStage(path.join(dir, 'LICENSE'), dir), true, 'a regular file');
    assert.equal(shouldStage(path.join(dir, 'gone.txt'), dir), false, 'gone from the top level');
    assert.equal(shouldStage(path.join(dir, 'bin', 'gone.js'), dir), false, 'gone from a directory');
    assert.equal(shouldStage(path.join(dir, 'gone', 'deeper.js'), dir), false, 'gone with its directory');
  });
});

test('shouldStage throws every error from looking at an entry but ENOENT', UNIX_ONLY, async () => {
  const { shouldStage } = await loadStage();
  await withSource(({ root, dir }) => {
    assert.throws(() => shouldStage(path.join(dir, 'LICENSE', 'inside'), dir), { code: 'ENOTDIR' }, 'a path through a file');
    fs.symlinkSync('loop', path.join(root, 'loop'));
    assert.throws(() => shouldStage(path.join(root, 'loop', 'inside'), root), { code: 'ELOOP' }, 'a path through a symbolic link loop');
  });
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

'use strict';

// scripts/publish-convergent.mjs decides, per package name, whether the release
// must publish it, and must be safe to rerun after a partial publication. Nothing
// here reaches the internet or runs `npm publish`. The registry is a stateful
// fake behind the script's injectable steps, except in one test that runs the
// real `npm view` against a registry served from this process. The replies that
// classifyView() is fed are real `npm view` outputs (npm 12.0.2) trimmed of their
// prose, so a change in npm's output shape shows up as a failing fixture here
// and, against the live registry, in the npm-wrapper CI job's lookup step.

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const http = require('node:http');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { pathToFileURL } = require('node:url');

const packageRoot = path.resolve(__dirname, '..');
const repoRoot = path.resolve(packageRoot, '..', '..');
const scriptFile = path.join(packageRoot, 'scripts', 'publish-convergent.mjs');

let loaded;
const load = () => (loaded ??= import(pathToFileURL(scriptFile).href));

const MCP = '@volute_cvc/cvc-mcp';
const CLI = '@volute_cvc/cvc';
const VERSION = '0.7.1';

const jsonError = (code, summary) => JSON.stringify({ error: { code, summary, detail: 'detail' } }, null, 2);
const failure = (stdout, stderr = 'npm error something') => ({ status: 1, stdout, stderr });

test('npm view: a listed version is published', async () => {
  const { classifyView, PUBLISHED } = await load();
  assert.equal(classifyView({ status: 0, stdout: '[\n  "0.7.0"\n]\n', stderr: '' }, MCP, '0.7.0'), PUBLISHED);
  assert.equal(classifyView({ status: 0, stdout: '"0.7.0"\n', stderr: '' }, MCP, '0.7.0'), PUBLISHED);
  assert.equal(classifyView({ status: 0, stdout: '["0.6.0", "0.7.0"]', stderr: '' }, MCP, '0.7.0'), PUBLISHED);
});

test('npm view: only an explicit E404 means not published', async () => {
  const { classifyView, UNPUBLISHED } = await load();
  // The two real shapes: the name exists but not this version, and the name does not exist.
  assert.equal(classifyView(failure(jsonError('E404', 'No match found for version 9.9.9')), MCP, '9.9.9'), UNPUBLISHED);
  assert.equal(classifyView(failure(jsonError('E404', 'Not Found - GET https://registry.npmjs.org/@volute_cvc%2fcvc - Not found')), CLI, VERSION), UNPUBLISHED);
});

test('npm view: every other outcome is an error, never "not published"', async () => {
  const { classifyView } = await load();
  const refuses = (result, pattern, what) => assert.throws(() => classifyView(result, MCP, VERSION), pattern, what);

  for (const code of ['ECONNREFUSED', 'ENOTFOUND', 'EAI_AGAIN', 'ETIMEDOUT', 'ECONNRESET', 'E401', 'E403', 'E429', 'E500', 'E502', 'E503', 'EINVALIDTAGNAME']) {
    refuses(failure(jsonError(code, 'x')), new RegExp(`failed \\(${code}\\); that is not E404`), code);
  }
  refuses(failure(jsonError('e404', 'x')), /not E404/, 'the code is matched exactly');
  refuses(failure(jsonError('E4040', 'x')), /not E404/, 'the code is matched exactly');
  refuses(failure(JSON.stringify({ error: {} })), /exit 1/, 'an error with no code');
  refuses(failure('', 'npm error code E404\nnpm error 404 Not Found'), /not E404/, 'human-readable E404 text without the JSON error is not trusted');
  refuses(failure('<html>404</html>', ''), /not E404/, 'a non-JSON body');
  refuses({ status: 1, stdout: '', stderr: '' }, /\(no output\)/, 'silent failure');
  refuses({ status: null, signal: 'SIGKILL', stdout: '', stderr: '' }, /signal SIGKILL/, 'killed');
  refuses({ status: null, stdout: '', error: Object.assign(new Error('spawnSync npm ETIMEDOUT'), { code: 'ETIMEDOUT' }) }, /could not run npm.*ETIMEDOUT/, 'timed out');
  refuses({ status: null, stdout: '', error: Object.assign(new Error('spawnSync npm ENOENT'), { code: 'ENOENT' }) }, /could not run npm.*ENOENT/, 'npm missing');

  // Success that does not name the version is not a verdict either way.
  refuses({ status: 0, stdout: '', stderr: '' }, /did not list that version.*\(no output\)/, 'empty success');
  refuses({ status: 0, stdout: '["0.7.0"]', stderr: '' }, /did not list that version/, 'another version');
  refuses({ status: 0, stdout: 'not json', stderr: '' }, /did not list that version/, 'garbage');
  refuses({ status: 0, stdout: '{"version":"0.7.1"}', stderr: '' }, /did not list that version/, 'an unexpected shape');
  refuses({ status: 0, stdout: jsonError('E404', 'x'), stderr: '' }, /did not list that version/, 'an error body with exit 0');
});

test('the npm invocations: anonymous uncached lookup, public publish, no --provenance', async () => {
  const { viewArguments, publishArguments, REGISTRY } = await load();
  assert.equal(REGISTRY, 'https://registry.npmjs.org/');
  assert.deepEqual(viewArguments(CLI, VERSION), [
    'view', `${CLI}@${VERSION}`, 'version', '--json', '--registry=https://registry.npmjs.org/', `--userconfig=${os.devNull}`, '--prefer-online', '--no-update-notifier',
  ]);
  assert.deepEqual(publishArguments(), ['publish', '--access', 'public', '--registry=https://registry.npmjs.org/']);
  assert.equal(publishArguments().some(argument => argument.includes('provenance')), false);
});

// The recorded replies above pin how classifyView reads npm's output; this runs
// the real npm 12 lookup (npmView) against a registry served from this process
// and checks the verdict for the replies a registry can give. Offline.
test('the real npm lookup tells a missing version from every registry failure', { skip: process.platform === 'win32' }, async () => {
  const { npmView, PUBLISHED, UNPUBLISHED } = await load();
  const json = (res, body, status = 200) => { res.statusCode = status; res.setHeader('content-type', 'application/json'); res.end(JSON.stringify(body)); };
  const packument = (name, versions) => ({ name, 'dist-tags': { latest: versions.at(-1) }, versions: Object.fromEntries(versions.map(version => [version, { name, version, dist: { tarball: `http://127.0.0.1/${version}.tgz`, shasum: 'x' } }])) });
  const replies = {
    listed: [(res, name) => json(res, packument(name, ['0.0.0', VERSION])), PUBLISHED],
    'other-version-only': [(res, name) => json(res, packument(name, ['0.0.0'])), UNPUBLISHED],
    'no-such-name': [res => json(res, { error: 'Not found' }, 404), UNPUBLISHED],
    'http-401': [res => json(res, { error: 'Unauthorized' }, 401), /E401/],
    'http-403': [res => json(res, { error: 'Forbidden' }, 403), /E403/],
    'http-429': [res => { res.statusCode = 429; res.end('slow down'); }, /E429/],
    'http-500': [res => { res.statusCode = 500; res.end('boom'); }, /E500/],
    'http-503': [res => { res.statusCode = 503; res.end('unavailable'); }, /E503/],
    html: [res => { res.setHeader('content-type', 'text/html'); res.end('<html>captive portal</html>'); }, /not E404/],
    empty: [res => res.end(''), /not E404/],
    dropped: [(res, name, req) => req.socket.destroy(), /not E404/],
  };
  const server = http.createServer((req, res) => {
    const name = decodeURIComponent(req.url.slice(1));
    const reply = replies[name.split('/')[1]];
    if (!reply) throw new Error(`unexpected request to the local registry: ${req.method} ${req.url}`);
    reply[0](res, name, req);
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const registry = `http://127.0.0.1:${server.address().port}/`;
  const cache = fs.mkdtempSync(path.join(os.tmpdir(), 'cvc-view-cache-'));
  const saved = { retries: process.env.npm_config_fetch_retries, cache: process.env.npm_config_cache };
  process.env.npm_config_fetch_retries = '0'; // a failing reply must not be retried for minutes
  process.env.npm_config_cache = cache;
  try {
    const outcomes = await Promise.all(Object.entries(replies).map(async ([key, [, expected]]) => {
      const name = `@volute_cvc/${key}`;
      const verdict = await npmView(name, VERSION, registry).catch(error => error);
      return { key, expected, verdict };
    }));
    for (const { key, expected, verdict } of outcomes) {
      if (typeof expected === 'string') assert.equal(verdict, expected, key);
      else {
        assert.ok(verdict instanceof Error, `${key}: must be an error, not the verdict ${String(verdict)}`);
        assert.match(verdict.message, expected, key);
        assert.match(verdict.message, /not read as "not published"/, key);
      }
    }
    // Nothing listening at all.
    await new Promise(resolve => server.close(resolve));
    await assert.rejects(npmView(`@volute_cvc/refused`, VERSION, registry), /not read as "not published"/);
  } finally {
    server.close();
    fs.rmSync(cache, { recursive: true, force: true });
    for (const [key, value] of [['npm_config_fetch_retries', saved.retries], ['npm_config_cache', saved.cache]]) {
      if (value === undefined) delete process.env[key]; else process.env[key] = value;
    }
  }
});

// A stateful fake of everything convergePublish touches. `registry` holds the
// name@version strings that exist. Options:
//   viewFails(spec, call)   -> an Error to throw from that lookup, or undefined
//   publishStatus(name, n)  -> exit status of the n-th publish of that name (default 0)
//   landsAnyway             -> names whose failed publish still reached the registry
//   stagedManifest(name, v) -> overrides the manifest the fake stage reports
function world({ registry = [], viewFails = () => undefined, publishStatus = () => 0, landsAnyway = [], stagedManifest } = {}) {
  const w = { registry: new Set(registry), calls: [], logs: [], warns: [], dirs: new Map(), publishes: new Map(), views: new Map() };
  w.deps = {
    stage: (name, version) => {
      w.calls.push(`stage ${name}`);
      const short = name.split('/').pop();
      const pkg = { root: `/stage/${short}`, dir: `/stage/${short}/npm/${short}`, manifest: stagedManifest?.(name, version) ?? { name, version } };
      w.dirs.set(pkg.dir, { name, version });
      return pkg;
    },
    view: (name, version, registryUrl) => {
      const spec = `${name}@${version}`;
      const call = (w.views.get(spec) ?? 0) + 1;
      w.views.set(spec, call);
      w.calls.push(`view ${spec}`);
      assert.equal(registryUrl, 'https://registry.npmjs.org/');
      const error = viewFails(spec, call);
      if (error) throw error;
      return w.registry.has(spec) ? 'published' : 'unpublished';
    },
    publish: (dir, registryUrl) => {
      const staged = w.dirs.get(dir);
      assert.ok(staged, `publish ran in a staged directory: ${dir}`);
      const { name, version } = staged;
      assert.equal(registryUrl, 'https://registry.npmjs.org/');
      const attempt = (w.publishes.get(name) ?? 0) + 1;
      w.publishes.set(name, attempt);
      w.calls.push(`publish ${name}@${version}`);
      const status = publishStatus(name, attempt);
      if (status === 0 || landsAnyway.includes(name)) w.registry.add(`${name}@${version}`);
      return status;
    },
    cleanup: root => w.calls.push(`cleanup ${root}`),
    log: message => w.logs.push(message),
    warn: message => w.warns.push(message),
  };
  w.run = async (config = {}) => (await load()).convergePublish({ version: VERSION, ...config }, w.deps);
  w.published = () => w.calls.filter(call => call.startsWith('publish ')).map(call => call.slice('publish '.length));
  return w;
}

test('nothing published yet: both names are published, cvc-mcp first, after both were looked up', async () => {
  const w = world();
  const outcome = await w.run();
  assert.deepEqual(outcome.results, [{ name: MCP, action: 'publish' }, { name: CLI, action: 'publish' }]);
  assert.deepEqual(w.calls, [
    `stage ${MCP}`, `view ${MCP}@${VERSION}`, `stage ${CLI}`, `view ${CLI}@${VERSION}`,
    `publish ${MCP}@${VERSION}`, `publish ${CLI}@${VERSION}`,
    'cleanup /stage/cvc-mcp', 'cleanup /stage/cvc',
  ]);
  assert.deepEqual([...w.registry].sort(), [`${MCP}@${VERSION}`, `${CLI}@${VERSION}`].sort());
});

test('both names get the release version', async () => {
  const w = world();
  await w.run({ version: '1.2.3' });
  assert.deepEqual(w.published(), [`${MCP}@1.2.3`, `${CLI}@1.2.3`]);
  assert.deepEqual(w.calls.filter(call => call.startsWith('view ')), [`view ${MCP}@1.2.3`, `view ${CLI}@1.2.3`]);
});

test('a name that is already published is skipped, the other is published', async () => {
  const w = world({ registry: [`${MCP}@${VERSION}`, `${MCP}@0.7.0`, `${CLI}@0.0.0`] });
  const outcome = await w.run();
  assert.deepEqual(outcome.results, [{ name: MCP, action: 'skip' }, { name: CLI, action: 'publish' }]);
  assert.deepEqual(w.published(), [`${CLI}@${VERSION}`]);
  assert.ok(w.logs.some(line => line.includes(`skip ${MCP}@${VERSION}`) && line.includes('already on the registry')));
});

test('a rerun of a fully published release publishes nothing', async () => {
  const w = world({ registry: [`${MCP}@${VERSION}`, `${CLI}@${VERSION}`] });
  const outcome = await w.run();
  assert.deepEqual(outcome.results, [{ name: MCP, action: 'skip' }, { name: CLI, action: 'skip' }]);
  assert.deepEqual(w.published(), []);
});

test('"Re-run failed jobs" completes a release whose second name failed', async () => {
  // First run: cvc-mcp goes out, cvc is rejected.
  const w = world({ publishStatus: name => (name === CLI ? 1 : 0) });
  await assert.rejects(w.run(), /npm publish @volute_cvc\/cvc@0\.7\.1 failed with exit 1, and the registry does not list the version/);
  assert.deepEqual([...w.registry], [`${MCP}@${VERSION}`]);
  // Rerun after the cause is fixed: the published name is left alone, the missing one is published.
  const rerun = world({ registry: [...w.registry] });
  const outcome = await rerun.run();
  assert.deepEqual(outcome.results, [{ name: MCP, action: 'skip' }, { name: CLI, action: 'publish' }]);
  assert.deepEqual(rerun.published(), [`${CLI}@${VERSION}`]);
  assert.deepEqual([...rerun.registry].sort(), [`${MCP}@${VERSION}`, `${CLI}@${VERSION}`].sort());
});

test('a lookup error fails the job before anything is published', async () => {
  for (const [label, failing] of [['the first name', MCP], ['the second name', CLI]]) {
    const w = world({ viewFails: spec => (spec.startsWith(failing) ? new Error('npm view failed (ECONNRESET)') : undefined) });
    await assert.rejects(w.run(), /ECONNRESET/, label);
    assert.deepEqual(w.published(), [], `${label}: nothing may be published on a guess`);
    assert.deepEqual([...w.registry], [], label);
    assert.ok(w.calls.some(call => call.startsWith('cleanup ')), `${label}: staged copies are removed`);
  }
});

test('a failed publish stops the run unless the registry shows the version', async () => {
  const w = world({ publishStatus: name => (name === MCP ? 1 : 0) });
  await assert.rejects(w.run(), /failed with exit 1/);
  assert.deepEqual(w.published(), [`${MCP}@${VERSION}`], 'cvc is not attempted after cvc-mcp failed');
  assert.equal(w.views.get(`${MCP}@${VERSION}`), 2, 'the registry is asked again before the failure is believed');
  assert.deepEqual(w.calls.filter(call => call.startsWith('cleanup ')), ['cleanup /stage/cvc-mcp', 'cleanup /stage/cvc']);
});

test('an upload that failed client-side but reached the registry counts as published', async () => {
  const w = world({ publishStatus: name => (name === MCP ? 1 : 0), landsAnyway: [MCP] });
  const outcome = await w.run();
  assert.deepEqual(outcome.results, [{ name: MCP, action: 'publish-despite-error' }, { name: CLI, action: 'publish' }]);
  assert.equal(w.warns.length, 1);
  assert.match(w.warns[0], /failed with exit 1, but the registry lists @volute_cvc\/cvc-mcp@0\.7\.1 now/);
  assert.deepEqual(w.published(), [`${MCP}@${VERSION}`, `${CLI}@${VERSION}`]);
});

test('a failed publish whose re-check also fails is reported with both causes', async () => {
  const w = world({ publishStatus: () => 1, viewFails: (spec, call) => (call === 2 ? new Error('npm view failed (ENOTFOUND)') : undefined) });
  await assert.rejects(w.run(), /exit 1, and the registry could not be re-checked \(npm view failed \(ENOTFOUND\)\)/);
  assert.deepEqual(w.published(), [`${MCP}@${VERSION}`]);
});

test('decide-only reports the decisions and never publishes', async () => {
  const w = world({ registry: [`${MCP}@${VERSION}`] });
  const outcome = await w.run({ decideOnly: true });
  assert.deepEqual(outcome.results, [{ name: MCP, action: 'skip' }, { name: CLI, action: 'would-publish' }]);
  assert.equal(outcome.decideOnly, true);
  assert.deepEqual(w.published(), []);
  assert.deepEqual([...w.registry], [`${MCP}@${VERSION}`]);
  assert.ok(w.logs.some(line => line.startsWith(`would publish ${CLI}@${VERSION}`)));
});

test('what is looked up is what is staged: a mismatched staged manifest is refused', async () => {
  const cases = [
    { label: 'wrong version on the first name', staged: name => ({ name, version: '0.0.0' }), pattern: /staged @volute_cvc\/cvc-mcp carries @volute_cvc\/cvc-mcp@0\.0\.0, expected @volute_cvc\/cvc-mcp@0\.7\.1/, views: 0 },
    { label: 'wrong name on the second name', staged: (name, version) => ({ name: name === CLI ? MCP : name, version }), pattern: /staged @volute_cvc\/cvc carries @volute_cvc\/cvc-mcp@0\.7\.1, expected @volute_cvc\/cvc@0\.7\.1/, views: 1 },
  ];
  for (const { label, staged, pattern, views } of cases) {
    const w = world({ stagedManifest: staged });
    await assert.rejects(w.run(), pattern, label);
    assert.equal(w.calls.filter(call => call.startsWith('view ')).length, views, label);
    assert.deepEqual(w.published(), [], `${label}: nothing is published`);
  }
});

test('only an exact stable version is accepted, before anything is staged', async () => {
  for (const version of ['0.7.1-rc.1', '0.7.1+build', 'v0.7.1', '0.7', '', undefined, '0.7.1 ']) {
    const w = world();
    await assert.rejects(w.run({ version }), /exact MAJOR\.MINOR\.PATCH/, String(version));
    assert.deepEqual(w.calls, [], String(version));
  }
});

test('the command line needs a version and rejects unknown arguments before doing anything', () => {
  for (const args of [[], ['--version'], ['--version', '0.7.1-rc.1'], ['--version', '0.7.1', '--publish'], ['--decide-only']]) {
    const result = spawnSync(process.execPath, [scriptFile, ...args], { encoding: 'utf8' });
    assert.equal(result.status, 1, args.join(' '));
    assert.equal(result.stdout, '');
    assert.notEqual(result.stderr, '', args.join(' '));
  }
});

// The workflows are where the pieces meet; a text-level contract keeps them from drifting apart.
function job(workflow, id) {
  const text = fs.readFileSync(path.join(repoRoot, '.github', 'workflows', workflow), 'utf8');
  const match = new RegExp(`^  ${id}:\\n([\\s\\S]*?)(?=^  [a-z][a-z0-9-]*:\\n|(?![\\s\\S]))`, 'm').exec(text);
  assert.ok(match, `job ${id} in ${workflow}`);
  return match[1].split('\n').filter(line => !line.trim().startsWith('#')).join('\n');
}

test('the release job publishes through the convergent script, with the pinned npm and no provenance flag', () => {
  const release = job('release.yml', 'publish-npm');
  assert.match(release, /node npm\/cvc-mcp\/scripts\/publish-convergent\.mjs --version "\$\{GITHUB_REF_NAME#v\}"/);
  assert.doesNotMatch(release, /npm publish|--provenance|npm version/, 'publishing and stamping live in the script');
  assert.match(release, /npm install --global npm@12\.0\.2/);
  assert.match(release, /environment: npm-publish/);
  assert.match(release, /id-token: write/);
});

test('release and CI pin the same npm, and CI takes the names from the staging script', () => {
  const pin = text => /npm install --global npm@(\S+)/.exec(text)?.[1];
  const release = job('release.yml', 'publish-npm');
  const ci = job('test.yml', 'npm-wrapper');
  assert.ok(pin(release));
  assert.equal(pin(ci), pin(release), 'npm pin in lockstep');
  assert.match(ci, /stage-package\.mjs --list/);
  assert.doesNotMatch(ci, /@volute_cvc\//, 'CI loops over the listed names instead of hard-coding them');
});

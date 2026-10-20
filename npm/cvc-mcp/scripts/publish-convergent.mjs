#!/usr/bin/env node
// Convergent npm publication of the launcher under every package name.
//
// A release publishes the launcher under two names (see stage-package.mjs). A
// plain `npm publish` per name cannot be rerun after a partial failure: if
// @volute_cvc/cvc-mcp went out and @volute_cvc/cvc did not, republishing the
// first fails with "cannot publish over the previously published version" and
// masks the real state. So, like the Marketplace publish
// (.github/scripts/publish-vsix-convergent.mjs), this makes the end state the
// contract: for each name, in publish order, look the exact name@version up on
// the registry, skip it if it is already there, and publish it otherwise.
// "Re-run failed jobs" therefore completes a partial publication.
//
// The lookup is `npm view name@version`, and its failure modes are kept apart:
//   - exit 0 and the version is listed        -> published: skip
//   - E404 (no such version, or no such name) -> not published: publish
//   - anything else (network, 5xx, 401, an    -> fail the job. An error is never
//     output we do not recognise, a timeout)     read as "not published", because
//                                                publishing on a guess is how a
//                                                rerun would republish a name.
// `npm view` runs anonymously (an empty user config): the metadata of a public
// package needs no credentials, and a placeholder or expired token that setup-node
// writes into the user config must not turn a public read into a 401.
//
// Every name is staged and looked up before the first publish, so a staging
// error or a registry error never leaves a release half done that did not have
// to be. The names are then published in order and the run stops at the first
// failure. If `npm publish` fails, the registry is asked once more before the
// failure is believed: an upload that timed out client-side can have succeeded
// server-side, and then the version is there and the name is done.
//
// Usage (run with the npm that publishes; needs no credentials with --decide-only):
//   node npm/cvc-mcp/scripts/publish-convergent.mjs --version 0.7.1
//   node npm/cvc-mcp/scripts/publish-convergent.mjs --version 0.7.1 --decide-only
// `--decide-only` stages and looks every name up, prints what would happen, and
// never publishes.
import { realpathSync, rmSync } from 'node:fs';
import { devNull, tmpdir } from 'node:os';
import { execFile, spawnSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';
import { PACKAGES, stagePackage } from './stage-package.mjs';

export const REGISTRY = 'https://registry.npmjs.org/';
export const PUBLISHED = 'published';
export const UNPUBLISHED = 'unpublished';
// Releases publish to npm's latest tag, so only exact stable versions are
// accepted, as in .github/scripts/check-release-version.mjs.
const STABLE_SEMVER = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;

/**
 * Arguments of the registry lookup: anonymous and revalidated against the
 * registry (see the header), and without npm's update check, which would
 * otherwise send a second request and put its notices ahead of an error.
 */
export function viewArguments(name, version, registry = REGISTRY) {
  return ['view', `${name}@${version}`, 'version', '--json', `--registry=${registry}`, `--userconfig=${devNull}`, '--prefer-online', '--no-update-notifier'];
}

/**
 * Arguments of the publish. `--access public` is required for the first publish
 * of a scoped name. There is deliberately no `--provenance`: trusted publishing
 * over OIDC attaches provenance by itself (see RELEASING.md).
 */
export function publishArguments(registry = REGISTRY) {
  return ['publish', '--access', 'public', `--registry=${registry}`];
}

const excerpt = text => (String(text).trim().split('\n').find(line => line.trim()) ?? '').slice(0, 300);

/**
 * Interprets the outcome of `npm view <name>@<version> version --json`, given as
 * { status, signal, stdout, stderr, error }: PUBLISHED, UNPUBLISHED, or a thrown
 * Error for everything else. Only an explicit E404 is UNPUBLISHED.
 */
export function classifyView(result, name, version) {
  const spec = `${name}@${version}`;
  const { status, signal, error } = result;
  const stdout = result.stdout ?? '';
  if (error) throw new Error(`could not run npm to look up ${spec}: ${error.message}`);
  if (status === 0) {
    let listed;
    try { listed = JSON.parse(stdout); } catch { /* reported below */ }
    const versions = (Array.isArray(listed) ? listed : [listed]).filter(entry => typeof entry === 'string');
    if (versions.includes(version)) return PUBLISHED;
    throw new Error(`npm view ${spec} succeeded but did not list that version, so its state is unknown: ${excerpt(stdout) || '(no output)'}`);
  }
  let code;
  try { code = JSON.parse(stdout)?.error?.code; } catch { /* not npm's JSON error */ }
  if (code === 'E404') return UNPUBLISHED;
  const reason = typeof code === 'string' ? code : (signal ? `signal ${signal}` : `exit ${status}`);
  throw new Error(`npm view ${spec} failed (${reason}); that is not E404, so it is not read as "not published": ${excerpt(result.stderr ?? '') || excerpt(stdout) || '(no output)'}`);
}

/**
 * Runs the registry lookup with the real npm and classifies it. Asynchronous so
 * a test can serve a local registry from the same process.
 */
export function npmView(name, version, registry = REGISTRY, { timeoutMs = 180_000 } = {}) {
  return new Promise(resolve => {
    execFile('npm', viewArguments(name, version, registry), { encoding: 'utf8', cwd: tmpdir(), timeout: timeoutMs, maxBuffer: 1 << 20 }, (error, stdout, stderr) => {
      // A non-zero exit carries npm's reply; anything else (npm missing, a
      // timeout, output too large) is a failure to run it at all.
      if (!error) resolve({ status: 0, stdout, stderr });
      else if (typeof error.code === 'number') resolve({ status: error.code, stdout, stderr });
      else resolve({ error: new Error(error.killed ? `timed out after ${timeoutMs} ms` : error.message), stdout, stderr });
    });
  }).then(result => classifyView(result, name, version));
}

// Everything convergePublish touches outside itself, so tests can replace the
// registry, npm publish, staging and the output. `view` resolves to PUBLISHED or
// UNPUBLISHED or throws; `publish` resolves to npm's exit status.
const REAL_DEPS = Object.freeze({
  stage: (name, version) => stagePackage(name, { version }),
  view: npmView,
  publish: (dir, registry) => {
    const result = spawnSync('npm', publishArguments(registry), { cwd: dir, stdio: 'inherit' });
    if (result.error) throw result.error;
    return result.status ?? 1;
  },
  cleanup: root => rmSync(root, { recursive: true, force: true }),
  log: message => console.log(message),
  warn: message => console.error(message),
});

/**
 * Brings every package name to `version` on the registry. Returns
 * { version, decideOnly, results: [{ name, action }] } with `action` one of
 * 'skip' (already there), 'publish' (published), 'publish-despite-error' (npm
 * publish failed but the registry lists the version), or 'would-publish'
 * (decide-only). Throws, after logging, on the first problem.
 */
export async function convergePublish(config, overrides = {}) {
  const { version, decideOnly = false, registry = REGISTRY } = config;
  if (typeof version !== 'string' || !STABLE_SEMVER.test(version)) throw new Error(`--version must be an exact MAJOR.MINOR.PATCH; received ${JSON.stringify(version)}`);
  const { stage, view, publish, cleanup, log, warn } = { ...REAL_DEPS, ...overrides };
  const staged = [];
  try {
    // Phase 1: no side effects on the registry. What is looked up is what is
    // staged, so the identity comes from the staged manifest and is checked
    // against the request rather than trusted.
    const plan = [];
    for (const { name } of PACKAGES) {
      const pkg = await stage(name, version);
      staged.push(pkg);
      if (pkg.manifest.name !== name || pkg.manifest.version !== version) {
        throw new Error(`staged ${name} carries ${pkg.manifest.name}@${pkg.manifest.version}, expected ${name}@${version}`);
      }
      plan.push({ name, dir: pkg.dir, state: await view(name, version, registry) });
    }

    // Phase 2: publish what is missing, in order, stopping at the first failure.
    const results = [];
    for (const { name, dir, state } of plan) {
      const spec = `${name}@${version}`;
      if (state === PUBLISHED) {
        log(`skip ${spec}: already on the registry`);
        results.push({ name, action: 'skip' });
      } else if (decideOnly) {
        log(`would publish ${spec}: not on the registry (decide only, nothing published)`);
        results.push({ name, action: 'would-publish' });
      } else {
        log(`publish ${spec}: not on the registry`);
        const status = await publish(dir, registry);
        if (status === 0) {
          log(`${spec}: published`);
          results.push({ name, action: 'publish' });
          continue;
        }
        const failure = `npm publish ${spec} failed with exit ${status}`;
        let after;
        try {
          after = await view(name, version, registry);
        } catch (error) {
          throw new Error(`${failure}, and the registry could not be re-checked (${error.message})`);
        }
        if (after !== PUBLISHED) throw new Error(`${failure}, and the registry does not list the version`);
        warn(`${failure}, but the registry lists ${spec} now (a timed-out upload can succeed server-side); treating it as published`);
        results.push({ name, action: 'publish-despite-error' });
      }
    }
    return { version, decideOnly, results };
  } finally {
    for (const pkg of staged) await cleanup(pkg.root);
  }
}

function parseArguments(argv) {
  const options = { version: undefined, decideOnly: false };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === '--version') options.version = argv[++index];
    else if (argument === '--decide-only') options.decideOnly = true;
    else throw new Error(`Unknown argument ${JSON.stringify(argument)}`);
  }
  if (!options.version) throw new Error('Usage: publish-convergent.mjs --version <MAJOR.MINOR.PATCH> [--decide-only]');
  return options;
}

async function main(argv) {
  const options = parseArguments(argv);
  const { version, decideOnly, results } = await convergePublish(options);
  console.log(`${decideOnly ? 'Decisions' : 'Done'} for ${version}: ${results.map(({ name, action }) => `${name} ${action}`).join(', ')}`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(realpathSync(process.argv[1])).href) {
  try {
    await main(process.argv.slice(2));
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
